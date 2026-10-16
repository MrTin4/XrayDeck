use crate::{
    service,
    settings::{Settings, data_dir},
    version::parse_version,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const RELEASES: &str = "https://api.github.com/repos/XTLS/Xray-core/releases?per_page=100";
const ASSET_NAME: &str = "Xray-windows-64.zip";
const MAX_DOWNLOAD: u64 = 150 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub prerelease: bool,
    pub draft: bool,
    pub html_url: String,
    pub published_at: Option<String>,
    pub assets: Vec<Asset>,
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("XrayDeck/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()?)
}

pub fn choose_release(releases: Vec<Release>, include_prerelease: bool) -> Result<Release> {
    releases
        .into_iter()
        .filter(|r| !r.draft && (include_prerelease || !r.prerelease))
        .filter_map(|r| parse_version(&r.tag_name).ok().map(|v| (v, r)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r)| r)
        .context("官方发布列表中没有符合渠道要求的版本")
}

pub fn check(include_prerelease: bool) -> Result<Release> {
    choose_release(
        client()?.get(RELEASES).send()?.error_for_status()?.json()?,
        include_prerelease,
    )
}

fn official_asset_url(url: &str) -> Result<()> {
    let url = url::Url::parse(url)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.path().starts_with("/XTLS/Xray-core/releases/download/")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "拒绝非官方核心下载地址"
    );
    Ok(())
}

fn download(url: &str, limit: u64) -> Result<Vec<u8>> {
    official_asset_url(url)?;
    let response = client()?.get(url).send()?.error_for_status()?;
    let mut bytes = Vec::new();
    response.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "下载文件超出大小限制");
    Ok(bytes)
}

pub fn verify_digest(bytes: &[u8], digest: &str) -> Result<()> {
    let (algorithm, expected) = digest.trim().split_once(':').context("无效校验值")?;
    let actual = match algorithm {
        "sha256" => hex::encode(Sha256::digest(bytes)),
        "sha512" => hex::encode(Sha512::digest(bytes)),
        _ => bail!("不支持的校验算法"),
    };
    ensure!(
        expected.len() == actual.len() && actual.eq_ignore_ascii_case(expected),
        "下载校验失败，未修改核心"
    );
    Ok(())
}

fn digest_for(release: &Release, asset: &Asset) -> Result<String> {
    if let Some(digest) = &asset.digest
        && (digest.starts_with("sha256:") || digest.starts_with("sha512:"))
    {
        return Ok(digest.clone());
    }
    let digest_asset = release
        .assets
        .iter()
        .find(|a| a.name == format!("{ASSET_NAME}.dgst"))
        .context("官方发布缺少校验信息，不能升级")?;
    let bytes = download(&digest_asset.browser_download_url, 64 * 1024)?;
    let text = String::from_utf8(bytes)?;
    parse_digest_file(&text)
}

fn parse_digest_file(text: &str) -> Result<String> {
    for line in text.lines() {
        let label = line.to_ascii_lowercase().replace('-', "");
        if (label.contains("sha512") || label.contains("sha2512"))
            && let Some(digest) = line
                .split(|c: char| !c.is_ascii_hexdigit())
                .find(|v| v.len() == 128)
        {
            return Ok(format!("sha512:{digest}"));
        }
        if (label.contains("sha256") || label.contains("sha2256"))
            && let Some(digest) = line
                .split(|c: char| !c.is_ascii_hexdigit())
                .find(|v| v.len() == 64)
        {
            return Ok(format!("sha256:{digest}"));
        }
    }
    bail!("无法解析官方校验文件")
}

pub fn extract_core(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut core = archive
        .by_name("xray.exe")
        .context("官方压缩包中缺少 xray.exe")?;
    ensure!(core.size() <= MAX_DOWNLOAD, "核心文件过大");
    let mut exe = Vec::new();
    core.by_ref().take(MAX_DOWNLOAD + 1).read_to_end(&mut exe)?;
    ensure!(
        exe.len() as u64 <= MAX_DOWNLOAD && service::is_x64_pe(&exe),
        "压缩包不是 Windows x64 核心"
    );
    Ok(exe)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradePlan {
    pub settings: Settings,
    pub tag: String,
    pub archive: PathBuf,
}

/// Preparation is read-only with respect to the live service.
pub fn prepare(settings: &Settings, release: &Release) -> Result<PathBuf> {
    prepare_with_progress(settings, release, &mut |_| {})
}

pub fn prepare_with_progress(
    settings: &Settings,
    release: &Release,
    progress: &mut dyn FnMut(&str),
) -> Result<PathBuf> {
    progress("正在复核服务关联与目标版本…");
    let association = service::validate(settings)?;
    ensure!(
        parse_version(&release.tag_name)? > parse_version(&association.file_version)?,
        "目标版本没有高于当前文件版本"
    );
    // Re-fetch the release; never trust URLs supplied by a saved plan or the UI.
    let official = fetch_release(&release.tag_name)?;
    ensure!(
        !official.prerelease || settings.include_prerelease,
        "未启用预发布渠道"
    );
    let asset = official
        .assets
        .iter()
        .find(|a| a.name == ASSET_NAME)
        .context("该版本没有 Windows x64 发布包")?;
    progress("正在通过系统网络下载官方 Windows x64 核心…");
    let bytes = download(&asset.browser_download_url, MAX_DOWNLOAD)?;
    progress("正在校验官方摘要和核心架构…");
    verify_digest(&bytes, &digest_for(&official, asset)?)?;
    extract_core(&bytes)?;
    let dir = data_dir()?.join("updates").join(format!(
        "{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    ));
    fs::create_dir_all(&dir)?;
    let archive = dir.join("core.zip");
    fs::write(&archive, bytes)?;
    let plan = UpgradePlan {
        settings: settings.clone(),
        tag: official.tag_name,
        archive,
    };
    let path = dir.join("plan.json");
    fs::write(&path, serde_json::to_vec_pretty(&plan)?)?;
    Ok(path)
}

fn fetch_release(tag: &str) -> Result<Release> {
    ensure!(
        tag.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-+_".contains(c)),
        "无效版本标识"
    );
    let url = format!("https://api.github.com/repos/XTLS/Xray-core/releases/tags/{tag}");
    let release: Release = client()?.get(url).send()?.error_for_status()?.json()?;
    ensure!(!release.draft && release.tag_name == tag, "无效官方发布");
    Ok(release)
}

pub trait UpgradeOps {
    fn stop(&mut self) -> Result<()>;
    fn install(&mut self) -> Result<()>;
    fn start(&mut self) -> Result<()>;
    fn healthy(&mut self) -> Result<()>;
    fn restore(&mut self) -> Result<()>;
}

pub fn run_transaction(ops: &mut impl UpgradeOps) -> Result<()> {
    // A failed stop must never be followed by replacement of a running executable.
    ops.stop()?;
    let result = ops
        .install()
        .and_then(|_| ops.start())
        .and_then(|_| ops.healthy());
    if let Err(error) = result {
        // If stopping the new process fails, keep the backup and avoid overwriting it.
        let rollback = ops
            .stop()
            .and_then(|_| ops.restore())
            .and_then(|_| ops.start())
            .and_then(|_| ops.healthy());
        return match rollback {
            Ok(()) => Err(anyhow::anyhow!("升级失败，已回滚并恢复服务：{error:#}")),
            Err(rollback) => Err(anyhow::anyhow!(
                "升级失败：{error:#}；回滚未完成：{rollback:#}。请保留备份并手动恢复"
            )),
        };
    }
    Ok(())
}

struct WindowsUpgrade {
    settings: Settings,
    new_core: Vec<u8>,
    old_pid: u32,
    installing: bool,
    target_version: String,
    previous_version: String,
    progress_path: PathBuf,
    backup_bytes: Vec<u8>,
}

impl WindowsUpgrade {
    fn report(&self, message: &str) {
        let _ = fs::write(&self.progress_path, message);
    }
    fn control(&self, start: bool) -> Result<()> {
        let name = service::ps_literal(&self.settings.service_name);
        let verb = if start { "Start" } else { "Stop" };
        let status = if start { "Running" } else { "Stopped" };
        service::powershell(&format!(
            "$ErrorActionPreference='Stop'; $s=Get-Service | Where-Object Name -eq {name} | Select-Object -First 1; if(!$s){{throw 'Service not found'}}; if($s.Status -ne '{status}'){{ $s.{verb}() }}; $s.WaitForStatus('{status}',[TimeSpan]::FromSeconds(30))"
        ))?;
        Ok(())
    }
}
impl UpgradeOps for WindowsUpgrade {
    fn stop(&mut self) -> Result<()> {
        self.report("正在停止服务并等待核心进程退出…");
        self.control(false)?;
        if self.old_pid != 0 {
            service::powershell(&format!(
                "$ErrorActionPreference='Stop'; $p=Get-Process -Id {} -ErrorAction SilentlyContinue; if($p -and !$p.WaitForExit(15000)){{throw 'Xray process did not exit'}}",
                self.old_pid
            ))?;
            self.old_pid = 0;
        }
        Ok(())
    }
    fn install(&mut self) -> Result<()> {
        self.report("正在替换核心文件…");
        fs::write(&self.settings.xray_path, &self.new_core)?;
        self.installing = true;
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        self.report("正在启动服务…");
        self.control(true)
    }
    fn healthy(&mut self) -> Result<()> {
        self.report("正在验证核心版本、服务归属及 API…");
        let mut last = String::new();
        for _ in 0..15 {
            match service::validate_for_upgrade(&self.settings) {
                Ok(info) => {
                    self.old_pid = info.process_id;
                    let expected = if self.installing {
                        &self.target_version
                    } else {
                        &self.previous_version
                    };
                    if info.file_version == *expected
                        && info.image_verified
                        && crate::api::inspect_blocking(&self.settings.api_address).is_ok()
                    {
                        return Ok(());
                    }
                }
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        bail!("服务/API 健康检查失败：{last}")
    }
    fn restore(&mut self) -> Result<()> {
        self.report("升级失败，正在恢复旧核心…");
        fs::write(&self.settings.xray_path, &self.backup_bytes)?;
        self.installing = false;
        Ok(())
    }
}

/// Runs in a separately elevated copy of the application. Revalidates everything.
pub fn apply(path: &Path) -> Result<()> {
    validate_plan_location(path)?;
    let plan: UpgradePlan = serde_json::from_slice(&fs::read(path)?)?;
    let directory = path.parent().context("无效升级路径")?;
    ensure!(
        plan.archive == directory.join("core.zip"),
        "升级压缩包必须位于计划目录"
    );
    reject_links(&plan.archive)?;
    reject_links(&plan.settings.xray_path)?;
    reject_links(&plan.settings.winsw_config)?;
    let association = service::validate_for_upgrade(&plan.settings)?;
    ensure!(
        association.image_verified,
        "提权后仍无法确认实际核心进程路径，拒绝升级"
    );
    let official = fetch_release(&plan.tag)?;
    ensure!(
        !official.prerelease || plan.settings.include_prerelease,
        "预发布渠道未启用"
    );
    ensure!(
        parse_version(&plan.tag)? > parse_version(&association.file_version)?,
        "拒绝重复升级或降级"
    );
    let asset = official
        .assets
        .iter()
        .find(|a| a.name == ASSET_NAME)
        .context("没有 x64 核心")?;
    ensure!(
        fs::metadata(&plan.archive)?.len() <= MAX_DOWNLOAD,
        "升级压缩包过大"
    );
    let bytes = fs::read(&plan.archive)?;
    verify_digest(&bytes, &digest_for(&official, asset)?)?;
    let new_core = extract_core(&bytes)?;
    let directory = path.parent().context("无效升级计划路径")?;
    let backup = directory.join("xray.previous.exe");
    let lock_path = data_dir()?.join("upgrade.lock");
    let mut lock = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("另一个升级正在运行，或之前异常退出留下 upgrade.lock")?;
    writeln!(lock, "{}", std::process::id())?;
    let result = (|| {
        let backup_bytes = fs::read(&plan.settings.xray_path)?;
        let mut backup_file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&backup)?;
        backup_file.write_all(&backup_bytes)?;
        backup_file.sync_all()?;
        let mut ops = WindowsUpgrade {
            settings: plan.settings,
            new_core,
            old_pid: association.process_id,
            installing: false,
            target_version: parse_version(&plan.tag)?.to_string(),
            previous_version: association.file_version,
            progress_path: directory.join("progress.txt"),
            backup_bytes,
        };
        run_transaction(&mut ops)
    })();
    drop(lock);
    let _ = fs::remove_file(lock_path);
    result
}

fn reject_links(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            ensure!(!metadata.file_type().is_symlink(), "升级路径不允许符号链接");
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    metadata.file_attributes() & 0x400 == 0,
                    "升级路径不允许重解析点"
                );
            }
        }
    }
    Ok(())
}

pub fn validate_plan_location(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute() && path.file_name().is_some_and(|n| n == "plan.json"),
        "无效升级计划路径"
    );
    reject_links(path)?;
    for name in ["result.txt", "progress.txt", "xray.previous.exe"] {
        reject_links(&path.with_file_name(name))?;
    }
    let root = data_dir()?.join("updates").canonicalize()?;
    let resolved = path.canonicalize()?;
    ensure!(
        resolved.starts_with(&root) && resolved.parent() != Some(root.as_path()),
        "升级计划必须位于 XrayDeck 更新目录"
    );
    Ok(())
}

#[cfg(windows)]
pub fn elevate(plan: &Path) -> Result<()> {
    elevate_with_progress(plan, &mut |_| {})
}

#[cfg(windows)]
pub fn elevate_with_progress(plan: &Path, progress: &mut dyn FnMut(&str)) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_HIDE,
        },
    };
    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }
    let executable = wide(std::env::current_exe()?.as_os_str());
    ensure!(!plan.to_string_lossy().contains('"'), "无效计划路径");
    let args = wide(std::ffi::OsStr::new(&format!(
        "--apply-update \"{}\"",
        plan.display()
    )));
    let verb = wide(std::ffi::OsStr::new("runas"));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = executable.as_ptr();
    info.lpParameters = args.as_ptr();
    info.nShow = SW_HIDE;
    unsafe {
        ensure!(
            ShellExecuteExW(&mut info) != 0,
            "UAC 被拒绝或无法启动升级程序：{}",
            std::io::Error::last_os_error()
        );
        progress("等待 UAC 辅助进程复核升级包和服务…");
        let mut last = String::new();
        loop {
            let wait = WaitForSingleObject(info.hProcess, 500);
            if wait == 0 {
                break;
            }
            if wait != 258 {
                CloseHandle(info.hProcess);
                bail!("无法等待升级辅助进程")
            }
            if let Ok(stage) = fs::read_to_string(plan.with_file_name("progress.txt"))
                && stage != last
            {
                progress(&stage);
                last = stage;
            }
        }
        let mut code = 1;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if code != 0 {
            let result = fs::read_to_string(plan.with_file_name("result.txt"))
                .unwrap_or_else(|_| "升级程序异常退出，请检查备份".into());
            bail!("{result}");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn elevate(_: &Path) -> Result<()> {
    bail!("升级仅支持 Windows")
}

#[cfg(not(windows))]
pub fn elevate_with_progress(_: &Path, _: &mut dyn FnMut(&str)) -> Result<()> {
    bail!("升级仅支持 Windows")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(tag: &str, pre: bool) -> Release {
        Release {
            tag_name: tag.into(),
            prerelease: pre,
            draft: false,
            html_url: String::new(),
            published_at: None,
            assets: vec![],
        }
    }
    #[test]
    fn channel_filter_and_numeric_comparison() {
        let releases = vec![
            release("v26.9.30", false),
            release("v26.10.10", false),
            release("v26.11.1", true),
        ];
        assert_eq!(
            choose_release(releases.clone(), false).unwrap().tag_name,
            "v26.10.10"
        );
        assert_eq!(choose_release(releases, true).unwrap().tag_name, "v26.11.1");
        assert!(choose_release(vec![release("v26.11.1", true)], false).is_err());
    }
    #[test]
    fn checksum_rejects_tampering_and_unknown_algorithms() {
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"core")));
        assert!(verify_digest(b"core", &digest).is_ok());
        assert!(verify_digest(b"evil", &digest).is_err());
        assert!(verify_digest(b"core", "md5:abcd").is_err());
    }
    #[test]
    fn official_origin_is_required() {
        assert!(
            official_asset_url("https://github.com/XTLS/Xray-core/releases/download/v1/core.zip")
                .is_ok()
        );
        for url in [
            "http://github.com/XTLS/Xray-core/releases/download/v1/core.zip",
            "https://github.com.evil/XTLS/Xray-core/releases/download/v1/core.zip",
            "https://github.com/other/repo/releases/download/v1/core.zip",
        ] {
            assert!(official_asset_url(url).is_err());
        }
    }
    #[derive(Default)]
    struct Fake {
        events: Vec<&'static str>,
        fail: Option<&'static str>,
        failed: bool,
        rollback_fails: bool,
    }
    impl Fake {
        fn step(&mut self, name: &'static str) -> Result<()> {
            self.events.push(name);
            if name == "restore" && self.rollback_fails {
                bail!("backup restore failed")
            }
            if self.fail == Some(name) && !self.failed {
                self.failed = true;
                bail!("failure")
            };
            Ok(())
        }
    }
    impl UpgradeOps for Fake {
        fn stop(&mut self) -> Result<()> {
            self.step("stop")
        }
        fn install(&mut self) -> Result<()> {
            self.step("install")
        }
        fn start(&mut self) -> Result<()> {
            self.step("start")
        }
        fn healthy(&mut self) -> Result<()> {
            self.step("healthy")
        }
        fn restore(&mut self) -> Result<()> {
            self.step("restore")
        }
    }
    #[test]
    fn failed_stop_never_replaces_core() {
        let mut ops = Fake {
            fail: Some("stop"),
            ..Default::default()
        };
        assert!(run_transaction(&mut ops).is_err());
        assert_eq!(ops.events, vec!["stop"]);
    }
    #[test]
    fn failed_health_check_rolls_back() {
        let mut ops = Fake {
            fail: Some("healthy"),
            ..Default::default()
        };
        assert!(
            run_transaction(&mut ops)
                .unwrap_err()
                .to_string()
                .contains("已回滚")
        );
        assert_eq!(
            ops.events,
            vec![
                "stop", "install", "start", "healthy", "stop", "restore", "start", "healthy"
            ]
        );
    }
    #[test]
    fn successful_upgrade_does_not_restore() {
        let mut ops = Fake::default();
        run_transaction(&mut ops).unwrap();
        assert_eq!(ops.events, vec!["stop", "install", "start", "healthy"]);
    }
    #[test]
    fn failed_install_restores_before_restart() {
        let mut ops = Fake {
            fail: Some("install"),
            ..Default::default()
        };
        assert!(run_transaction(&mut ops).is_err());
        assert_eq!(
            ops.events,
            vec!["stop", "install", "stop", "restore", "start", "healthy"]
        );
    }
    #[test]
    fn failed_start_also_rolls_back() {
        let mut ops = Fake {
            fail: Some("start"),
            ..Default::default()
        };
        assert!(
            run_transaction(&mut ops)
                .unwrap_err()
                .to_string()
                .contains("已回滚")
        );
        assert_eq!(
            ops.events,
            vec![
                "stop", "install", "start", "stop", "restore", "start", "healthy"
            ]
        );
    }
    #[test]
    fn rollback_failure_is_not_reported_as_recovered() {
        let mut ops = Fake {
            fail: Some("healthy"),
            rollback_fails: true,
            ..Default::default()
        };
        let error = run_transaction(&mut ops).unwrap_err().to_string();
        assert!(error.contains("回滚未完成"));
        assert!(!error.contains("已回滚"));
        assert_eq!(ops.events.last(), Some(&"restore"));
    }
    #[test]
    fn accepts_openssl_digest_labels() {
        let checksum = hex::encode(Sha256::digest(b"core"));
        let digest =
            parse_digest_file(&format!("SHA2-256(Xray-windows-64.zip)= {checksum}")).unwrap();
        assert!(verify_digest(b"core", &digest).is_ok());
        assert!(parse_digest_file("SHA1= aabbcc").is_err());
    }
    #[test]
    fn never_extracts_a_traversal_entry_or_invalid_binary() {
        use zip::write::SimpleFileOptions;
        fn archive(name: &str, bytes: &[u8]) -> Vec<u8> {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
            zip.finish().unwrap().into_inner()
        }
        assert!(extract_core(&archive("../xray.exe", b"evil")).is_err());
        assert!(extract_core(&archive("xray.exe", b"not a PE file")).is_err());
        assert!(extract_core(b"broken zip").is_err());
    }
}
