use std::fs;
use std::path::Path;
use std::process::Command;

use zuti_helper::config::consts::{SAMBA_PASSDB_PATH, UPGRADE_FILES, ZUTI_DB_PATH};
use zuti_helper::config::logger::init_logger_for;

fn main() {
    init_logger_for("zuti-updater");
    log::info!("zuti-updater started");

    // 检查 tdbbackup 命令是否存在
    if Command::new("tdbbackup").output().is_err() {
        log::warn!("tdbbackup command not found");
    } else {
        log::info!("tdbbackup command found");
    }

    // 检查 sqlite3 命令是否存在
    if Command::new("sqlite3").output().is_err() {
        log::warn!("sqlite3 command not found");
    } else {
        log::info!("sqlite3 command found");
    }

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

    // 备份 SAMBA passdb.tdb
    let passdb_path = Path::new(SAMBA_PASSDB_PATH);
    if passdb_path.exists() {
        match Command::new("tdbbackup")
            .args(["-s", ".bak", SAMBA_PASSDB_PATH])
            .output()
        {
            Ok(output) if output.status.success() => {
                let bak_src = format!("{}.bak", SAMBA_PASSDB_PATH);
                let bak_dest = format!("{}{}.bak", target_dir, SAMBA_PASSDB_PATH);

                if let Some(parent) = Path::new(&bak_dest).parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        log::error!("Failed to create directory '{}': {}", parent.display(), e);
                    } else {
                        match fs::copy(&bak_src, &bak_dest) {
                            Ok(bytes) => {
                                log::info!(
                                    "Backed up '{}' -> '{}' ({} bytes)",
                                    SAMBA_PASSDB_PATH,
                                    bak_dest,
                                    bytes
                                );
                            }
                            Err(e) => {
                                log::error!(
                                    "Failed to copy backup '{}' -> '{}': {}",
                                    bak_src,
                                    bak_dest,
                                    e
                                );
                            }
                        }
                    }
                }

                // 清理源目录的临时 .bak 文件
                if let Err(e) = fs::remove_file(&bak_src) {
                    log::warn!("Failed to remove temporary backup file '{}': {}", bak_src, e);
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::error!("tdbbackup failed for '{}': {}", SAMBA_PASSDB_PATH, stderr);
            }
            Err(e) => {
                log::error!("Failed to execute tdbbackup for '{}': {}", SAMBA_PASSDB_PATH, e);
            }
        }
    } else {
        log::warn!("SAMBA passdb file '{}' does not exist, skipping backup", SAMBA_PASSDB_PATH);
    }

    // 备份 ZUTI SQLite 数据库
    let zuti_db_path = Path::new(ZUTI_DB_PATH);
    if zuti_db_path.exists() {
        let db_dest = format!("{}{}", target_dir, ZUTI_DB_PATH);

        if let Some(parent) = Path::new(&db_dest).parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::error!("Failed to create directory '{}': {}", parent.display(), e);
            } else {
                match Command::new("sqlite3")
                    .args([ZUTI_DB_PATH, &format!(".backup {}", db_dest)])
                    .output()
                {
                    Ok(output) if output.status.success() => {
                        log::info!("Backed up '{}' -> '{}'", ZUTI_DB_PATH, db_dest);
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        log::error!("sqlite3 backup failed for '{}': {}", ZUTI_DB_PATH, stderr);
                    }
                    Err(e) => {
                        log::error!("Failed to execute sqlite3 backup for '{}': {}", ZUTI_DB_PATH, e);
                    }
                }
            }
        }
    } else {
        log::warn!("ZUTI database file '{}' does not exist, skipping backup", ZUTI_DB_PATH);
    }

    log::info!("zuti-updater finished");
}
