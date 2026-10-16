use anyhow::{Result, bail};
use semver::Version;

pub fn parse_version(text: &str) -> Result<Version> {
    for word in text.split_whitespace() {
        let candidate = word.trim_start_matches('v');
        if let Ok(version) = Version::parse(candidate) {
            return Ok(version);
        }
    }
    bail!("无法识别 Xray 版本")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_and_release_tag() {
        assert_eq!(
            parse_version("Xray 26.10.10 (Xray) hash").unwrap(),
            Version::new(26, 10, 10)
        );
        assert_eq!(parse_version("v26.9.30").unwrap(), Version::new(26, 9, 30));
        assert!(parse_version("permission denied").is_err());
    }

    #[test]
    fn compares_numeric_parts_and_prereleases() {
        assert!(parse_version("26.10.10").unwrap() > parse_version("26.9.30").unwrap());
        assert!(parse_version("26.10.10").unwrap() > parse_version("26.10.10-rc.1").unwrap());
    }
}
