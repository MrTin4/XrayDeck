use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub api_address: String,
    pub service_name: String,
    pub winsw_config: PathBuf,
    pub xray_path: PathBuf,
    pub include_prerelease: bool,
    pub update_check_interval_mins: u64,
    pub auto_refresh: bool,
    pub refresh_interval_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_address: "127.0.0.1:8080".into(),
            service_name: String::new(),
            winsw_config: PathBuf::new(),
            xray_path: PathBuf::new(),
            include_prerelease: false,
            update_check_interval_mins: 60,
            auto_refresh: false,
            refresh_interval_secs: 30,
        }
    }
}

pub fn data_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("APPDATA").context("APPDATA 未设置")?).join("XrayDeck"))
}

pub fn api_endpoint(address: &str) -> Result<String> {
    let value = address.trim();
    let endpoint = if value.contains("://") {
        value.to_owned()
    } else {
        format!("http://{value}")
    };
    let url = url::Url::parse(&endpoint).context("API 地址格式无效")?;
    let explicit_port = endpoint
        .strip_prefix("http://")
        .and_then(|v| v.split('/').next())
        .and_then(|v| v.rsplit_once(':'))
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .is_some_and(|p| p > 0);
    if url.scheme() != "http"
        || url.host_str().is_none()
        || !explicit_port
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("请输入明文 gRPC 地址，例如 127.0.0.1:8080；必须显式指定端口")
    }
    Ok(endpoint.trim_end_matches('/').to_owned())
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let settings: Self =
                    serde_json::from_slice(&bytes).context("设置文件损坏，请修复后重新打开")?;
                validate_refresh_interval(settings.refresh_interval_secs)?;
                validate_update_check_interval(settings.update_check_interval_mins)?;
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        api_endpoint(&self.api_address)?;
        validate_refresh_interval(self.refresh_interval_secs)?;
        validate_update_check_interval(self.update_check_interval_mins)?;
        let parent = path.parent().context("无效设置路径")?;
        fs::create_dir_all(parent)?;
        // Write fully before replacing the last good settings file.
        let temporary = path.with_extension("json.new");
        let mut file = fs::File::create(&temporary)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            unsafe extern "system" {
                fn MoveFileExW(old: *const u16, new: *const u16, flags: u32) -> i32;
            }
            let old: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let new: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe { MoveFileExW(old.as_ptr(), new.as_ptr(), 0x1 | 0x8) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        #[cfg(not(windows))]
        fs::rename(temporary, path)?;
        Ok(())
    }
}

pub fn parse_update_check_interval(value: &str) -> Result<u64> {
    let minutes = value
        .trim()
        .parse()
        .context("更新检测间隔请输入整数分钟（1–10080）")?;
    validate_update_check_interval(minutes)?;
    Ok(minutes)
}

fn validate_update_check_interval(minutes: u64) -> Result<()> {
    anyhow::ensure!(
        (1..=10080).contains(&minutes),
        "更新检测间隔须为 1–10080 分钟"
    );
    Ok(())
}

pub fn parse_refresh_interval(value: &str) -> Result<u64> {
    let seconds = value
        .trim()
        .parse()
        .context("刷新间隔请输入整数秒（5–86400）")?;
    validate_refresh_interval(seconds)?;
    Ok(seconds)
}

fn validate_refresh_interval(seconds: u64) -> Result<()> {
    anyhow::ensure!((5..=86400).contains(&seconds), "刷新间隔须为 5–86400 秒");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_settings_keep_auto_refresh_disabled_with_a_default_interval() {
        let settings: Settings =
            serde_json::from_str(r#"{"api_address":"127.0.0.1:8080","service_name":"winsw-xray"}"#)
                .unwrap();
        assert!(!settings.auto_refresh);
        assert_eq!(settings.refresh_interval_secs, 30);
        assert_eq!(settings.update_check_interval_mins, 60);
        assert_eq!(settings.service_name, "winsw-xray");
    }

    #[test]
    fn update_check_interval_validation_preserves_saved_settings() {
        for value in ["", "0", "10081", "-1", "1.5", "abc", "18446744073709551616"] {
            assert!(parse_update_check_interval(value).is_err(), "{value}");
        }
        assert_eq!(parse_update_check_interval(" 1 ").unwrap(), 1);
        assert_eq!(parse_update_check_interval("10080").unwrap(), 10080);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut settings = Settings::default();
        settings.save(&path).unwrap();
        let original = fs::read(&path).unwrap();
        for minutes in [0, 10081, u64::MAX] {
            settings.update_check_interval_mins = minutes;
            assert!(settings.save(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
            fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
            assert!(Settings::load(&path).is_err());
            fs::write(&path, &original).unwrap();
        }
    }

    #[test]
    fn invalid_intervals_are_rejected_without_overwriting_saved_settings() {
        for value in [
            "",
            "0",
            "4",
            "86401",
            "-5",
            "1.5",
            "abc",
            "18446744073709551616",
        ] {
            assert!(parse_refresh_interval(value).is_err(), "{value}");
        }
        assert_eq!(parse_refresh_interval(" 5 ").unwrap(), 5);
        assert_eq!(parse_refresh_interval("86400").unwrap(), 86400);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut settings = Settings::default();
        settings.save(&path).unwrap();
        let original = fs::read(&path).unwrap();
        settings.refresh_interval_secs = 0;
        assert!(settings.save(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(Settings::load(&path).is_err());
    }
    #[test]
    fn rejects_ambiguous_or_non_grpc_addresses() {
        assert_eq!(
            api_endpoint("127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert!(api_endpoint("[::1]:8080").is_ok());
        assert!(api_endpoint("localhost:80").is_ok());
        assert!(api_endpoint("localhost:0").is_err());
        for value in [
            "localhost",
            "https://localhost:8080",
            "http://localhost:8080/proxies",
            "user:pass@localhost:8080",
            "localhost:8080?x=1",
        ] {
            assert!(api_endpoint(value).is_err(), "{value}");
        }
    }
    #[test]
    fn settings_round_trip_and_corruption_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let settings = Settings {
            service_name: "winsw-xray".into(),
            auto_refresh: true,
            refresh_interval_secs: 17,
            update_check_interval_mins: 120,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        fs::write(&path, "broken").unwrap();
        assert!(Settings::load(&path).is_err());
    }
}
