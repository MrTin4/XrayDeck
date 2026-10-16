use std::{fs, io::Write};

/// Fixed application events only; never log node payloads or credentials.
pub fn event(message: &str) {
    let Ok(dir) = crate::settings::data_dir().map(|p| p.join("logs")) else {
        return;
    };
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("xray-deck.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > 1024 * 1024) {
        let _ = fs::rename(&path, dir.join("xray-deck.previous.log"));
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "{} {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            message
        );
    }
}
