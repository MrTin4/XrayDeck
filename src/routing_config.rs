//! Read only routing metadata from the configuration of the local API process.
//! Configuration contents and command lines must never be logged or serialized.
use crate::{service, settings::api_endpoint};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize)]
pub(crate) struct Balancer {
    pub tag: String,
    #[serde(default)]
    pub selector: Vec<String>,
    #[serde(default, rename = "fallbackTag")]
    pub fallback_tag: String,
}

#[derive(Default, Deserialize)]
struct Routing {
    #[serde(default)]
    balancers: Vec<Balancer>,
}

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    routing: Option<Routing>,
}

#[derive(Deserialize)]
struct Wrapper {
    workingdirectory: Option<String>,
    arguments: Option<String>,
    #[serde(default, rename = "argument")]
    arguments_list: Vec<String>,
}

#[derive(Deserialize)]
struct Runtime {
    command: Option<String>,
    parent: u32,
    wrapper: Option<String>,
}

pub(crate) fn discover(address: &str) -> Result<Vec<Balancer>> {
    let url = url::Url::parse(&api_endpoint(address)?)?;
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    ensure!(
        matches!(host, "127.0.0.1" | "::1" | "localhost"),
        "远程 API 无法自动读取负载均衡配置"
    );
    let port = url.port().context("API 端口无效")?;
    let script = format!(
        r#"
$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.Encoding]::UTF8
$listeners=@(Get-NetTCPConnection -LocalPort {port} -State Listen -ErrorAction Stop | Where-Object {{ $_.LocalAddress -eq {host} -or $_.LocalAddress -eq '0.0.0.0' -or $_.LocalAddress -eq '::' }})
$ids=@($listeners.OwningProcess | Sort-Object -Unique)
if($ids.Count -ne 1) {{ throw 'Cannot identify API process' }}
$proc=Get-CimInstance Win32_Process -Filter ('ProcessId=' + $ids[0])
if(!$proc) {{ throw 'API process unavailable' }}
$service=Get-CimInstance Win32_Service | Where-Object {{ $_.ProcessId -eq $proc.ParentProcessId }} | Select-Object -First 1
@{{command=$proc.CommandLine; parent=[uint32]$proc.ParentProcessId; wrapper=$service.PathName}} | ConvertTo-Json -Compress
"#,
        host = service::ps_literal(if host == "localhost" {
            "127.0.0.1"
        } else {
            host
        })
    );
    let output =
        service::powershell(&script).map_err(|_| anyhow::anyhow!("无法读取 API 进程启动信息"))?;
    let runtime: Runtime =
        serde_json::from_str(&output).map_err(|_| anyhow::anyhow!("无法识别 API 进程启动信息"))?;
    // Service ImagePath remains readable when SYSTEM process images and command
    // lines are unavailable to the ordinary desktop user.
    let wrapper = if let Some(image) = runtime.wrapper.filter(|value| !value.trim().is_empty()) {
        PathBuf::from(
            split_command_line(&image)?
                .first()
                .context("Windows 服务路径为空")?,
        )
    } else {
        service::process_image(runtime.parent).context("无法定位 WinSW 包装器")?
    }
    .with_extension("xml");
    let xml = std::fs::read_to_string(&wrapper).context("无法读取 API 进程对应的 WinSW XML")?;
    let config: Wrapper =
        quick_xml::de::from_str(&xml).map_err(|_| anyhow::anyhow!("无法解析 WinSW XML"))?;
    let base = wrapper.parent().context("WinSW 目录无效")?;
    let working = config
        .workingdirectory
        .as_deref()
        .map(|value| service::expand_path(value, base))
        .transpose()?
        .unwrap_or_else(|| base.to_path_buf());
    let args = if let Some(command) = runtime.command.filter(|value| !value.trim().is_empty()) {
        let mut args = split_command_line(&command)?;
        ensure!(!args.is_empty(), "核心启动参数为空");
        args.remove(0);
        args
    } else if !config.arguments_list.is_empty() {
        config
            .arguments_list
            .into_iter()
            .map(|value| value.replace("%BASE%", &base.to_string_lossy()))
            .collect()
    } else {
        split_command_line(
            &config
                .arguments
                .unwrap_or_default()
                .replace("%BASE%", &base.to_string_lossy()),
        )?
    };
    load_balancers(&args, &working)
}

// Windows quoting rules for paths with spaces and backslashes before quotes.
fn split_command_line(command: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut chars = command.chars().peekable();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while let Some(&ch) = chars.peek() {
            if !quoted && ch.is_whitespace() {
                break;
            }
            if ch == '\\' {
                let mut count = 0;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    count += 1;
                }
                if chars.peek() == Some(&'"') {
                    arg.extend(std::iter::repeat_n('\\', count / 2));
                    chars.next();
                    if count % 2 == 1 {
                        arg.push('"');
                    } else {
                        quoted = !quoted;
                    }
                } else {
                    arg.extend(std::iter::repeat_n('\\', count));
                }
            } else {
                chars.next();
                if ch == '"' {
                    quoted = !quoted;
                } else {
                    arg.push(ch);
                }
            }
        }
        ensure!(!quoted, "核心启动参数引号不完整");
        result.push(arg);
    }
    Ok(result)
}

fn config_files(args: &[String], working: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut directory = None;
    let mut format = "auto";
    let mut index = usize::from(args.first().is_some_and(|arg| arg == "run"));
    while index < args.len() {
        let arg = args[index].trim_start_matches('-');
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg, None), |(name, value)| (name, Some(value)));
        if matches!(name, "c" | "config" | "confdir" | "format") {
            let value = if let Some(value) = inline {
                value
            } else {
                index += 1;
                args.get(index).context("核心配置参数缺少值")?
            };
            if name == "format" {
                ensure!(
                    matches!(value, "auto" | "json"),
                    "自动负载均衡发现暂只支持 JSON / JSONC 配置"
                );
                format = value;
            } else {
                ensure!(
                    !value.contains("://") && value != "stdin:" && !value.contains('%'),
                    "无法自动读取远程、标准输入或未展开变量的核心配置"
                );
                let path = PathBuf::from(value);
                let path = if path.is_absolute() {
                    path
                } else {
                    working.join(path)
                };
                if name == "confdir" {
                    directory = Some(path);
                } else {
                    files.push(path);
                }
            }
        }
        index += 1;
    }
    if let Some(dir) = directory {
        let mut entries = std::fs::read_dir(dir)
            .context("无法读取核心配置目录")?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort();
        for path in entries {
            if path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        matches!(ext, "json" | "jsonc")
                            || (format == "auto" && matches!(ext, "toml" | "yaml" | "yml"))
                    })
            {
                files.push(path);
            }
        }
    }
    if files.is_empty() {
        for ext in ["json", "jsonc", "toml", "yaml", "yml"] {
            let path = working.join(format!("config.{ext}"));
            if path.is_file() {
                files.push(path);
                break;
            }
        }
    }
    ensure!(
        !files.is_empty(),
        "未找到启动参数指定的核心配置或默认 config.json"
    );
    ensure!(
        files.iter().all(|file| file
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "json" | "jsonc"))),
        "自动负载均衡发现暂只支持 JSON / JSONC 配置"
    );
    Ok(files)
}

fn load_balancers(args: &[String], working: &Path) -> Result<Vec<Balancer>> {
    let mut balancers = BTreeMap::new();
    for file in config_files(args, working)? {
        let content = std::fs::read_to_string(file).context("无法读取核心配置")?;
        // Deserialize only routing fields. Parsing errors intentionally omit content.
        let config: Config = serde_json::from_slice(&strip_comments(
            content.trim_start_matches('\u{feff}').as_bytes(),
        )?)
        .map_err(|_| anyhow::anyhow!("核心 JSON / JSONC 配置解析失败"))?;
        if let Some(routing) = config.routing {
            // Xray replaces the whole routing block when a later file provides one.
            balancers.clear();
            for balancer in routing.balancers {
                ensure!(!balancer.tag.is_empty(), "负载均衡器 tag 为空");
                balancers.insert(balancer.tag.clone(), balancer);
            }
        }
    }
    Ok(balancers.into_values().collect())
}

fn strip_comments(input: &[u8]) -> Result<Vec<u8>> {
    let mut output = input.to_vec();
    let mut index = 0;
    let mut quoted = false;
    while index < input.len() {
        if quoted {
            if input[index] == b'\\' {
                index += 2;
                continue;
            }
            if input[index] == b'"' {
                quoted = false;
            }
        } else if input[index] == b'"' {
            quoted = true;
        } else if input[index] == b'#' || input.get(index..index + 2) == Some(b"//") {
            while index < input.len() && input[index] != b'\n' {
                output[index] = b' ';
                index += 1;
            }
            continue;
        } else if input.get(index..index + 2) == Some(b"/*") {
            output[index..index + 2].fill(b' ');
            index += 2;
            while index < input.len() && input.get(index..index + 2) != Some(b"*/") {
                if input[index] != b'\n' {
                    output[index] = b' ';
                }
                index += 1;
            }
            if index + 2 > input.len() {
                bail!("核心配置注释未闭合");
            }
            output[index..index + 2].fill(b' ');
            index += 2;
            continue;
        }
        index += 1;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_arguments_preserve_spaces_and_backslashes() {
        assert_eq!(split_command_line(r#""C:\Program Files\Xray\xray.exe" run -c "configs\main config.json" -confdir="extra configs""#).unwrap(), [r"C:\Program Files\Xray\xray.exe", "run", "-c", r"configs\main config.json", "-confdir=extra configs"]);
        assert!(split_command_line("run -c \"incomplete").is_err());
    }

    #[test]
    fn reads_all_balancers_from_final_routing_block_without_secrets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.json"), r#"{ "routing": {"balancers":[{"tag":"a","selector":["node-"]}]}, "outbounds":[{"password":"never expose"}]}"#).unwrap();
        std::fs::create_dir(dir.path().join("extra configs")).unwrap();
        std::fs::write(dir.path().join("extra configs/b.jsonc"), "{ /* comment */ \"routing\": {\"balancers\":[{\"tag\":\"b\",\"selector\":[\"backup-\"],\"fallbackTag\":\"direct\"},{\"tag\":\"c\",\"selector\":[\"other-\"]}]} }").unwrap();
        let args = split_command_line(r#"run -c main.json -confdir="extra configs""#).unwrap();
        let balancers = load_balancers(&args, dir.path()).unwrap();
        assert_eq!(
            balancers.iter().map(|b| b.tag.as_str()).collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert_eq!(balancers[0].fallback_tag, "direct");
        assert_eq!(balancers[0].selector, ["backup-"]);
    }

    #[test]
    fn comments_keep_urls_and_escaped_strings_and_errors_hide_content() {
        let data =
            br##"{ "routing": {}, "url":"https://host/#path", "password":"a\"//b" } // end"##;
        let value: serde_json::Value =
            serde_json::from_slice(&strip_comments(data).unwrap()).unwrap();
        assert_eq!(value["url"], "https://host/#path");
        assert!(strip_comments(b"{/* unclosed").is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "secret-invalid-config").unwrap();
        let error = load_balancers(&[], dir.path()).err().unwrap().to_string();
        assert!(!error.contains("secret"));
        assert!(config_files(&["-c=stdin:".into()], dir.path()).is_err());
        assert!(config_files(&["-c=https://host/config".into()], dir.path()).is_err());
    }
}
