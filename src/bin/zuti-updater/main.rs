use std::fs;
use std::path::Path;

use zuti_helper::config::consts::UPGRADE_FILES;
use zuti_helper::config::logger::init_logger_for;

fn main() {
    init_logger_for("zuti-updater");
    log::info!("zuti-updater started");

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        log::error!("Usage: zuti-updater <target_dir>");
        std::process::exit(1);
    }
    let target_dir = &args[1];
    log::info!("Target directory: {}", target_dir);

    for src_path in UPGRADE_FILES {
        let src = Path::new(src_path);
        if !src.exists() {
            log::warn!("Source file '{}' does not exist, skipping", src_path);
            continue;
        }

        let dest_path = format!("{}{}", target_dir, src_path);
        let dest = Path::new(&dest_path);

        if let Some(parent) = dest.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::error!(
                    "Failed to create directory '{}': {}",
                    parent.display(),
                    e
                );
                continue;
            }
        }

        match fs::copy(src, dest) {
            Ok(bytes) => {
                log::info!(
                    "Copied '{}' -> '{}' ({} bytes)",
                    src_path,
                    dest_path,
                    bytes
                );
            }
            Err(e) => {
                log::error!(
                    "Failed to copy '{}' -> '{}': {}",
                    src_path,
                    dest_path,
                    e
                );
            }
        }
    }

    log::info!("zuti-updater finished");
}
