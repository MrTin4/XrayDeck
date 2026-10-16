#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(all(windows, feature = "desktop"))]
mod single_instance;
#[cfg(all(windows, feature = "desktop"))]
mod tray;
#[cfg(all(windows, feature = "desktop"))]
mod ui;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--inspect") => {
            let address = args.get(2).map(String::as_str).unwrap_or("127.0.0.1:8080");
            let snapshot = xray_deck::api::inspect_blocking(address)?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        Some("--apply-update") => {
            let path = std::path::PathBuf::from(
                args.get(2).ok_or_else(|| anyhow::anyhow!("缺少升级计划"))?,
            );
            xray_deck::update::validate_plan_location(&path)?;
            let result = xray_deck::update::apply(&path);
            let text = match &result {
                Ok(()) => "升级成功".into(),
                Err(e) => format!("{e:#}"),
            };
            std::fs::write(path.with_file_name("result.txt"), text)?;
            result?;
        }
        Some("--check-updates") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&xray_deck::update::check(
                    args.iter().any(|a| a == "--prerelease")
                )?)?
            );
        }
        Some("--validate-service") => {
            let path = std::path::PathBuf::from(
                args.get(2)
                    .ok_or_else(|| anyhow::anyhow!("缺少设置文件路径"))?,
            );
            let settings = xray_deck::settings::Settings::load(&path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&xray_deck::service::validate(&settings)?)?
            );
        }
        Some(other) => anyhow::bail!("未知参数：{other}"),
        None => {
            #[cfg(all(windows, feature = "desktop"))]
            if let Some(instance) = single_instance::Instance::start()? {
                ui::run(instance)?;
            }
            #[cfg(not(all(windows, feature = "desktop")))]
            anyhow::bail!("请启用 desktop 功能运行 Windows 界面，或使用 --inspect ADDRESS");
        }
    }
    Ok(())
}
