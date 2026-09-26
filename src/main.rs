mod ui;
use anyhow::{Context, Result};
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut data_dir = None;
    let mut demo = false;
    let mut screenshot = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                data_dir = Some(PathBuf::from(
                    args.next().context("--data-dir requires a path")?,
                ))
            }
            "--demo" => demo = true,
            "--screenshot" => {
                screenshot = Some(PathBuf::from(
                    args.next().context("--screenshot requires a PNG path")?,
                ))
            }
            "--help" | "-h" => {
                println!(
                    "rushvn [--data-dir PATH] [--demo] [--screenshot PNG]\n--demo: isolated offline sample articles; no connection\n--screenshot: save the first ready window and exit"
                );
                return Ok(());
            }
            _ => anyhow::bail!("Unknown option: {arg}"),
        }
    }
    let dir = data_dir.unwrap_or_else(|| {
        if demo {
            std::env::temp_dir().join("rushvn-demo")
        } else {
            directories::ProjectDirs::from("org", "rushVN", "rushVN")
                .expect("application data directory")
                .data_local_dir()
                .to_path_buf()
        }
    });
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let db = dir.join("news.sqlite3");
    if demo {
        ui::seed_demo(&db)?;
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(if demo {
                "rushVN — デモ / オフライン"
            } else {
                "rushVN — fj ニュースリーダー"
            })
            .with_inner_size([1220.0, 820.0])
            .with_min_inner_size([850.0, 580.0]),
        persistence_path: Some(dir.join("window.ron")),
        ..Default::default()
    };
    eframe::run_native(
        "rushVN",
        options,
        Box::new(move |cc| Ok(Box::new(ui::NewsApp::new(cc, db, demo, screenshot)))),
    )
    .map_err(|e| anyhow::anyhow!("GUIを起動できません: {e}"))
}
