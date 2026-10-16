use crate::{
    settings::{Settings, api_endpoint},
    version::parse_version,
};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub state: String,
    pub wrapper_path: PathBuf,
    pub core_path: PathBuf,
    pub process_id: u32,
    pub file_version: String,
    pub runtime_version: Option<String>,
    #[serde(default)]
    pub runtime_version_source: Option<String>,
    #[serde(default)]
    pub runtime_version_note: Option<String>,
    pub image_verified: bool,
}

#[derive(Debug, Deserialize)]
struct WinSw {
    id: String,
    executable: String,
    #[serde(default)]
    workingdirectory: Option<String>,
}

pub fn ps_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

pub fn powershell(script: &str) -> Result<String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
    hidden(&mut cmd);
    let output = cmd.output().context("无法执行 Windows 管理命令")?;
    ensure!(
        output.status.success(),
        "Windows 操作失败：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?
        .trim_start_matches('\u{feff}')
        .trim()
        .to_string())
}

pub fn hidden(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

pub fn file_version(path: &Path) -> Result<String> {
    let mut command = Command::new(path);
    hidden(&mut command);
    let mut child = command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("无法执行指定的 xray.exe")?;
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("核心版本查询超时");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output()?;
    ensure!(output.status.success(), "核心版本命令失败");
    parse_core_version(&String::from_utf8_lossy(&output.stdout))
}

fn parse_core_version(text: &str) -> Result<String> {
    let text = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches('\u{feff}');
    ensure!(text.starts_with("Xray "), "所选程序不是可识别的 Xray 核心");
    Ok(parse_version(text)?.to_string())
}

pub fn is_x64_pe(bytes: &[u8]) -> bool {
    if bytes.get(..2) != Some(b"MZ") {
        return false;
    }
    let Some(offset) = bytes
        .get(0x3c..0x40)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_le_bytes)
    else {
        return false;
    };
    let offset = offset as usize;
    bytes.get(offset..offset.saturating_add(6)) == Some(&[b'P', b'E', 0, 0, 0x64, 0x86])
}

fn same_path(a: &Path, b: &Path) -> Result<bool> {
    Ok(a.canonicalize()?.to_string_lossy().to_lowercase()
        == b.canonicalize()?.to_string_lossy().to_lowercase())
}

#[cfg(windows)]
pub(crate) fn process_image(pid: u32) -> Result<PathBuf> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(
            !handle.is_null(),
            "无法查询进程路径：{}",
            std::io::Error::last_os_error()
        );
        let mut buffer = vec![0u16; 32768];
        let mut size = buffer.len() as u32;
        let success = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        let error = std::io::Error::last_os_error();
        CloseHandle(handle);
        ensure!(success != 0, "无法查询进程路径：{error}");
        Ok(PathBuf::from(String::from_utf16(&buffer[..size as usize])?))
    }
}
#[cfg(not(windows))]
pub(crate) fn process_image(_: u32) -> Result<PathBuf> {
    bail!("仅支持 Windows")
}

pub(crate) fn expand_path(value: &str, base: &Path) -> Result<PathBuf> {
    let mut expanded = value.replace("%BASE%", &base.to_string_lossy());
    while let Some(start) = expanded.find('%') {
        let tail = &expanded[start + 1..];
        let end = tail.find('%').context("WinSW 路径环境变量格式无效")? + start + 1;
        let value =
            std::env::var(&expanded[start + 1..end]).context("无法解析 WinSW 路径中的环境变量")?;
        expanded.replace_range(start..=end, &value);
    }
    let path = PathBuf::from(expanded);
    Ok(if path.is_absolute() {
        path
    } else {
        base.join(path)
    })
}

#[derive(Deserialize)]
struct WindowsAssociation {
    state: String,
    wrapper: String,
    pid: u32,
    core: String,
    associated: bool,
}

/// Validates WinSW identity and checks that the API belongs to its Xray child.
pub fn validate(settings: &Settings) -> Result<ServiceInfo> {
    validate_impl(settings, false)
}

pub fn validate_for_upgrade(settings: &Settings) -> Result<ServiceInfo> {
    validate_impl(settings, true)
}

fn validate_impl(settings: &Settings, require_image: bool) -> Result<ServiceInfo> {
    ensure!(
        !settings.service_name.trim().is_empty(),
        "请填写 Windows 服务名"
    );
    let url = url::Url::parse(&api_endpoint(&settings.api_address)?)?;
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    ensure!(
        matches!(host, "127.0.0.1" | "::1" | "localhost"),
        "仅支持为本机回环 API 关联升级服务"
    );
    let config: WinSw = quick_xml::de::from_str(
        &std::fs::read_to_string(&settings.winsw_config).context("无法读取 WinSW 配置")?,
    )
    .context("无法解析 WinSW XML")?;
    ensure!(
        config.id == settings.service_name,
        "Windows 服务名与 WinSW 配置 id 不一致：当前填写 {}，应填写 {}（服务名不是 WinSW 的 exe 文件名）",
        settings.service_name,
        config.id
    );
    let base = settings
        .winsw_config
        .parent()
        .context("WinSW 配置路径无效")?;
    let working = config
        .workingdirectory
        .as_deref()
        .map(|s| expand_path(s, base))
        .transpose()?
        .unwrap_or(base.to_path_buf());
    // BASE is the wrapper directory, even when a different workingdirectory is set.
    let executable = config.executable.replace("%BASE%", &base.to_string_lossy());
    let core = expand_path(&executable, &working)?;
    ensure!(
        same_path(&core, &settings.xray_path)?,
        "WinSW executable 与指定核心路径不一致"
    );
    ensure!(
        is_x64_pe(&std::fs::read(&core)?),
        "首版仅支持 Windows x64 核心"
    );
    let script = format!(
        r#"
$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.Encoding]::UTF8
$s=Get-CimInstance Win32_Service | Where-Object Name -eq {name} | Select-Object -First 1
if (!$s) {{ throw 'Service not found' }}
$raw=$s.PathName.Trim(); $wrapper=if($raw.StartsWith('"')){{$raw.Split('"')[1]}}else{{$raw.Split(' ')[0]}}
$port={port}; $corePath={core}; $all=@(Get-CimInstance Win32_Process)
$listeners=@(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Where-Object {{$_.LocalAddress -eq {host}}})
$matched=$null
foreach($l in $listeners){{
  $p=$all | Where-Object ProcessId -eq $l.OwningProcess | Select-Object -First 1
  if(!$p -or ($p.ExecutablePath -and $p.ExecutablePath -ine $corePath)){{continue}}
  $ancestor=$p; $seen=@{{}}
  while($ancestor -and !$seen.ContainsKey([string]$ancestor.ProcessId)){{
    $seen[[string]$ancestor.ProcessId]=$true
    if($ancestor.ProcessId -eq $s.ProcessId){{$matched=$p;break}}
    $parent=$ancestor.ParentProcessId; $ancestor=$all | Where-Object ProcessId -eq $parent | Select-Object -First 1
  }}
}}
@{{state=$s.State;wrapper=$wrapper;pid=if($matched){{$matched.ProcessId}}else{{0}};core=$corePath;associated=($null -ne $matched)}} | ConvertTo-Json -Compress
"#,
        name = ps_literal(&settings.service_name),
        port = url.port_or_known_default().context("缺少 API 端口")?,
        core = ps_literal(
            core.canonicalize()?
                .to_string_lossy()
                .trim_start_matches(r"\\?\")
        ),
        host = ps_literal(if host == "localhost" {
            "127.0.0.1"
        } else {
            host
        })
    );
    let info: WindowsAssociation =
        serde_json::from_str(&powershell(&script)?).context("无法读取 Windows 服务关联信息")?;
    ensure!(
        info.state == "Running",
        "服务当前未运行，无法校验 API 与进程的关系"
    );
    ensure!(
        info.associated,
        "API 监听进程与指定服务/核心不匹配，或没有查询权限"
    );
    let image_verified = match process_image(info.pid) {
        Ok(image) => {
            ensure!(
                same_path(&image, &core)?,
                "API 进程实际路径与指定核心不一致"
            );
            true
        }
        Err(_) => false,
    };
    // Never execute a configured binary with elevation before verifying its image.
    ensure!(
        !require_image || image_verified,
        "提权后仍无法确认实际核心进程路径，拒绝升级"
    );
    let wrapper = PathBuf::from(info.wrapper);
    ensure!(
        same_path(&wrapper.with_extension("xml"), &settings.winsw_config)?,
        "仅支持服务程序同名的 WinSW XML 配置"
    );
    let version = file_version(&core)?;
    let source = if image_verified {
        "已校验进程映像路径"
    } else {
        "WinSW 服务配置关联路径"
    };
    Ok(ServiceInfo {
        name: settings.service_name.clone(),
        state: info.state,
        wrapper_path: wrapper,
        core_path: PathBuf::from(info.core),
        process_id: info.pid,
        file_version: version.clone(),
        runtime_version: Some(version),
        runtime_version_source: Some(format!("{} --version · PID {} · {source}", core.display(), info.pid)),
        runtime_version_note: (!image_verified).then(|| "实际进程映像路径无法直接查询，版本来自已校验服务配置关联的核心文件命令；并非读取进程内存版本。".into()),
        image_verified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_command_output_and_rejects_non_core_output() {
        assert_eq!(
            parse_core_version("Xray 26.10.10 (Xray) build\nA unified platform.\n").unwrap(),
            "26.10.10"
        );
        assert_eq!(
            parse_core_version("\u{feff}Xray 26.10.10-rc.1 (Xray) build\r\n").unwrap(),
            "26.10.10-rc.1"
        );
        for text in [
            "",
            "unknown option --version\nXray 26.10.10",
            "not Xray 26.10.10",
            "Xray invalid\n26.10.10",
        ] {
            assert!(parse_core_version(text).is_err(), "{text}");
        }
    }

    #[test]
    fn powershell_values_remain_literals() {
        assert_eq!(
            ps_literal("a'; Stop-Service x; '"),
            "'a''; Stop-Service x; '''"
        );
    }
    #[test]
    fn validates_pe_architecture_and_bounds() {
        assert!(!is_x64_pe(b"MZ"));
        let mut bytes = vec![0u8; 256];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&128u32.to_le_bytes());
        bytes[128..134].copy_from_slice(&[b'P', b'E', 0, 0, 0x64, 0x86]);
        assert!(is_x64_pe(&bytes));
        bytes[132] = 0x4c;
        assert!(!is_x64_pe(&bytes));
    }
    #[test]
    fn reads_winsw_without_needing_node_config() {
        let config: WinSw=quick_xml::de::from_str("<service><id>winsw-xray</id><executable>%BASE%\\xray.exe</executable><arguments>run -c secret.json</arguments></service>").unwrap();
        assert_eq!(config.id, "winsw-xray");
        assert_eq!(config.executable, "%BASE%\\xray.exe");
    }
}
