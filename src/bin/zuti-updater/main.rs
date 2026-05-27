use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use zuti_helper::config::consts::{SAMBA_PASSDB_PATH, SQLITE_MIGRATIONS_DIR, UPGRADE_FILES, UPGRADE_MUST_COPY_FILES, ZUTI_DB_PATH};
use zuti_helper::config::logger::init_logger_for;

fn copy_entry(src: &Path, dest: &Path) {
    if let Some(parent) = dest.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        log::error!("Failed to create directory '{}': {}", parent.display(), e);
        return;
    }

    match fs::copy(src, dest) {
        Ok(bytes) => {
            if let Ok(metadata) = fs::metadata(src) {
                let permissions = metadata.permissions();
                if let Err(e) = fs::set_permissions(dest, permissions) {
                    log::warn!(
                        "Failed to preserve permissions for '{}': {}",
                        dest.display(),
                        e
                    );
                }
            }
            log::info!(
                "Copied '{}' -> '{}' ({} bytes)",
                src.display(),
                dest.display(),
                bytes
            );
        }
        Err(e) => {
            log::error!(
                "Failed to copy '{}' -> '{}': {}",
                src.display(),
                dest.display(),
                e
            );
        }
    }
}

fn copy_directory(src_dir: &Path, dest_base: &Path) {
    let entries = match fs::read_dir(src_dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::error!("Failed to read directory '{}': {}", src_dir.display(), e);
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::error!("Failed to read entry in '{}': {}", src_dir.display(), e);
                continue;
            }
        };

        let src_path = entry.path();
        let relative = match src_path.strip_prefix(src_dir) {
            Ok(r) => r,
            Err(e) => {
                log::error!("Failed to get relative path for '{}': {}", src_path.display(), e);
                continue;
            }
        };
        let dest_path = dest_base.join(relative);

        if src_path.is_dir() {
            copy_directory(&src_path, &dest_path);
        } else {
            copy_entry(&src_path, &dest_path);
        }
    }
}

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
        log::error!("Usage: zuti-updater <target_dir> [is_fresh_install]");
        std::process::exit(1);
    }
    let target_dir = &args[1];
    let is_fresh_install = args.get(2).map(|s| s == "fresh_install").unwrap_or(false);
    log::info!("Target directory: {}", target_dir);
    log::info!("Is fresh install: {}", is_fresh_install);

    for src_path in UPGRADE_MUST_COPY_FILES {
        let src = Path::new(src_path);
        if !src.exists() {
            log::warn!("Source file '{}' does not exist, skipping", src_path);
            continue;
        }

        let dest_path = format!("{}{}", target_dir, src_path);
        let dest = Path::new(&dest_path);

        if src.is_dir() {
            copy_directory(src, dest);
        } else {
            copy_entry(src, dest);
        }
    }

    if !is_fresh_install {
    for src_path in UPGRADE_FILES {
        let src = Path::new(src_path);
        if !src.exists() {
            log::warn!("Source file '{}' does not exist, skipping", src_path);
            continue;
        }

        let dest_path = format!("{}{}", target_dir, src_path);
        let dest = Path::new(&dest_path);

        if src.is_dir() {
            copy_directory(src, dest);
        } else {
            copy_entry(src, dest);
        }
    }
    } else {
        log::info!("Skipping files backup (fresh install)");
    }
    // 备份 SAMBA passdb.tdb
    if !is_fresh_install {
    let passdb_path = Path::new(SAMBA_PASSDB_PATH);
    if passdb_path.exists() {
        match Command::new("tdbbackup")
            .args(["-s", ".bak", SAMBA_PASSDB_PATH])
            .output()
        {
            Ok(output) if output.status.success() => {
                let bak_src = format!("{}.bak", SAMBA_PASSDB_PATH);
                let bak_dest = format!("{}{}", target_dir, SAMBA_PASSDB_PATH);

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

    // 备份 samba 用户信息
    match Command::new("pdbedit").arg("-L").output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let samba_users: Vec<&str> = stdout
                .lines()
                .filter_map(|line| line.split(':').next())
                .collect();

            if !samba_users.is_empty() {
                match fs::read_to_string("/etc/passwd") {
                    Ok(content) => {
                        let filtered: Vec<&str> = content
                            .lines()
                            .filter(|line| {
                                line.split(':').next().is_some_and(|user| samba_users.contains(&user))
                            })
                            .collect();
                        let passwd_dest = format!("{}/etc/passwd", target_dir);
                        if let Some(parent) = Path::new(&passwd_dest).parent()
                            && let Err(e) = fs::create_dir_all(parent)
                        {
                            log::error!("Failed to create directory '{}': {}", parent.display(), e);
                        }
                        match fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&passwd_dest)
                        {
                            Ok(mut file) => {
                                if let Err(e) = file.write_all(filtered.join("\n").as_bytes()) {
                                    log::error!("Failed to append to '{}': {}", passwd_dest, e);
                                } else {
                                    log::info!("Backed up Samba users to '{}'", passwd_dest);
                                }
                            }
                            Err(e) => log::error!("Failed to open '{}': {}", passwd_dest, e),
                        }
                    }
                    Err(e) => log::error!("Failed to read /etc/passwd: {}", e),
                }

                match fs::read_to_string("/etc/shadow") {
                    Ok(content) => {
                        let filtered: Vec<&str> = content
                            .lines()
                            .filter(|line| {
                                line.split(':').next().is_some_and(|user| samba_users.contains(&user))
                            })
                            .collect();
                        let shadow_dest = format!("{}/etc/shadow", target_dir);
                        if let Some(parent) = Path::new(&shadow_dest).parent()
                            && let Err(e) = fs::create_dir_all(parent)
                        {
                            log::error!("Failed to create directory '{}': {}", parent.display(), e);
                        }
                        match fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&shadow_dest)
                        {
                            Ok(mut file) => {
                                if let Err(e) = file.write_all(filtered.join("\n").as_bytes()) {
                                    log::error!("Failed to append to '{}': {}", shadow_dest, e);
                                } else {
                                    log::info!("Backed up Samba user shadows to '{}'", shadow_dest);
                                }
                            }
                            Err(e) => log::error!("Failed to open '{}': {}", shadow_dest, e),
                        }
                    }
                    Err(e) => log::error!("Failed to read /etc/shadow: {}", e),
                }
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::error!("pdbedit -L failed: {}", stderr);
        }
        Err(e) => {
            log::error!("Failed to execute pdbedit -L: {}", e);
        }
    }
    } else {
        log::info!("Skipping samba backup (fresh install)");
    }
    // 备份 ZUTI SQLite 数据库
    if !is_fresh_install {
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

                        // 查询 users 表中的 name
                        match Command::new("sqlite3")
                            .args([ZUTI_DB_PATH, "SELECT name FROM users;"])
                            .output()
                        {
                            Ok(output) if output.status.success() => {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                let names: Vec<&str> = stdout
                                    .lines()
                                    .map(|s| s.trim())
                                    .filter(|s| !s.is_empty())
                                    .collect();

                                if !names.is_empty() {
                                    match fs::read_to_string("/etc/passwd") {
                                        Ok(passwd_content) => {
                                            let mut entries_to_append = String::new();
                                            for name in &names {
                                                for line in passwd_content.lines() {
                                                    if line.starts_with(&format!("{}:", name)) {
                                                        entries_to_append.push_str(line);
                                                        entries_to_append.push('\n');
                                                        break;
                                                    }
                                                }
                                            }

                                            if !entries_to_append.is_empty() {
                                                let target_passwd = format!("{}/etc/passwd", target_dir);
                                                if let Some(parent) = Path::new(&target_passwd).parent() {
                                                    if let Err(e) = fs::create_dir_all(parent) {
                                                        log::error!("Failed to create directory '{}': {}", parent.display(), e);
                                                    } else {
                                                        match fs::OpenOptions::new()
                                                            .create(true)
                                                            .append(true)
                                                            .open(&target_passwd)
                                                        {
                                                            Ok(mut file) => {
                                                                if let Err(e) = file.write_all(entries_to_append.as_bytes()) {
                                                                    log::error!("Failed to append to '{}': {}", target_passwd, e);
                                                                } else {
                                                                    log::info!("Appended user entries to '{}'", target_passwd);
                                                                }
                                                            }
                                                            Err(e) => {
                                                                log::error!("Failed to open '{}': {}", target_passwd, e);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            log::error!("Failed to read /etc/passwd: {}", e);
                                        }
                                    }

                                    // 处理 /etc/shadow
                                    match fs::read_to_string("/etc/shadow") {
                                        Ok(shadow_content) => {
                                            let mut shadow_entries_to_append = String::new();
                                            for name in &names {
                                                for line in shadow_content.lines() {
                                                    if line.starts_with(&format!("{}:", name)) {
                                                        shadow_entries_to_append.push_str(line);
                                                        shadow_entries_to_append.push('\n');
                                                        break;
                                                    }
                                                }
                                            }

                                            if !shadow_entries_to_append.is_empty() {
                                                let target_shadow = format!("{}/etc/shadow", target_dir);
                                                if let Some(parent) = Path::new(&target_shadow).parent() {
                                                    if let Err(e) = fs::create_dir_all(parent) {
                                                        log::error!("Failed to create directory '{}': {}", parent.display(), e);
                                                    } else {
                                                        match fs::OpenOptions::new()
                                                            .create(true)
                                                            .append(true)
                                                            .open(&target_shadow)
                                                        {
                                                            Ok(mut file) => {
                                                                if let Err(e) = file.write_all(shadow_entries_to_append.as_bytes()) {
                                                                    log::error!("Failed to append to '{}': {}", target_shadow, e);
                                                                } else {
                                                                    log::info!("Appended user entries to '{}'", target_shadow);
                                                                }
                                                            }
                                                            Err(e) => {
                                                                log::error!("Failed to open '{}': {}", target_shadow, e);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            log::error!("Failed to read /etc/shadow: {}", e);
                                        }
                                    }
                                }
                            }
                            Ok(output) => {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                log::error!("sqlite3 query failed: {}", stderr);
                            }
                            Err(e) => {
                                log::error!("Failed to execute sqlite3 query: {}", e);
                            }
                        }
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
    } else {
        log::info!("Skipping ZUTI database backup (fresh install)");
    }
    // 升级 SQLite 数据库
    let db_path = format!("{}{}", target_dir, ZUTI_DB_PATH);
    let old_migrations_dir = SQLITE_MIGRATIONS_DIR;
    let new_migrations_dir = format!("{}{}", target_dir, SQLITE_MIGRATIONS_DIR);

    let old_dirs = collect_subdir_names(old_migrations_dir);
    let new_dirs = collect_subdir_names(&new_migrations_dir);

    // 需要升级的目录：新系统中存在但旧系统中不存在的目录
    let mut upgrade_dirs: Vec<String> = new_dirs
        .into_iter()
        .filter(|d| !old_dirs.contains(d))
        .collect();
    upgrade_dirs.sort();

    if upgrade_dirs.is_empty() {
        log::info!("No new SQLite migrations to apply");
    } else {
        if let Some(parent) = Path::new(&db_path).parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            log::error!("Failed to create database directory '{}': {}", parent.display(), e);
        }
        for dir in &upgrade_dirs {
            let up_sql_path = format!("{}/{}/up.sql", new_migrations_dir, dir);
            if !Path::new(&up_sql_path).exists() {
                log::warn!("Migration up.sql not found: {}", up_sql_path);
                continue;
            }
            log::info!("Applying SQLite migration: {}", up_sql_path);
            match fs::File::open(&up_sql_path) {
                Ok(file) => {
                    match Command::new("sqlite3").arg(&db_path).stdin(file).output() {
                        Ok(output) if output.status.success() => {
                            log::info!("Applied migration '{}' successfully", dir);
                        }
                        Ok(output) => {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            log::error!("Migration '{}' failed: {}", dir, stderr);
                        }
                        Err(e) => {
                            log::error!("Failed to execute sqlite3 for migration '{}': {}", dir, e);
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to open migration file '{}': {}", up_sql_path, e);
                }
            }
        }
    }

    // 备份 Podman images
    if !is_fresh_install {
    match Command::new("sh")
        .arg("-c")
        .arg("podman save -m -o /tmp/all-images.tar $(podman images -q)")
        .output()
    {
        Ok(output) if output.status.success() => {
            log::info!("Podman images saved to /tmp/all-images.tar");
            let tmp_dir = format!("{}/tmp", target_dir);
            if let Err(e) = fs::create_dir_all(&tmp_dir) {
                log::error!("Failed to create directory '{}': {}", tmp_dir, e);
            } else {
                let dest = format!("{}/all-images.tar", tmp_dir);
                match fs::copy("/tmp/all-images.tar", &dest) {
                    Ok(bytes) => {
                        log::info!("Copied '/tmp/all-images.tar' -> '{}' ({} bytes)", dest, bytes);

                        // 为 chroot 准备 proc/sys/dev
                        let proc_dir = format!("{}/proc", target_dir);
                        let sys_dir = format!("{}/sys", target_dir);
                        let dev_dir = format!("{}/dev", target_dir);
                        let _ = fs::create_dir_all(&proc_dir);
                        let _ = fs::create_dir_all(&sys_dir);
                        let _ = fs::create_dir_all(&dev_dir);

                        let mount = |desc: &str, args: &[&str]| {
                            match Command::new("mount").args(args).output() {
                                Ok(output) if output.status.success() => {
                                    log::info!("Mounted {}", desc);
                                }
                                Ok(output) => {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    log::warn!("Failed to mount {}: {}", desc, stderr.trim());
                                }
                                Err(e) => {
                                    log::warn!("Failed to execute mount {}: {}", desc, e);
                                }
                            }
                        };

                        mount("proc", &["-t", "proc", "proc", &proc_dir]);
                        mount("sysfs", &["-t", "sysfs", "sys", &sys_dir]);
                        mount("dev", &["--bind", "/dev", &dev_dir]);

                        match Command::new("chroot")
                            .arg(target_dir)
                            .args(["podman", "load", "-i", "/tmp/all-images.tar"])
                            .output()
                        {
                            Ok(output) if output.status.success() => {
                                log::info!("Podman images loaded in chroot successfully");
                            }
                            Ok(output) => {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                log::error!("Podman load in chroot failed: {}", stderr);
                            }
                            Err(e) => {
                                log::error!("Failed to execute chroot podman load: {}", e);
                            }
                        }

                        let umount = |desc: &str, path: &str| {
                            match Command::new("umount").arg(path).output() {
                                Ok(output) if output.status.success() => {
                                    log::info!("Unmounted {}", desc);
                                }
                                Ok(output) => {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    log::warn!("Failed to unmount {}: {}", desc, stderr.trim());
                                }
                                Err(e) => {
                                    log::warn!("Failed to execute umount {}: {}", desc, e);
                                }
                            }
                        };

                        umount("proc", &proc_dir);
                        umount("sysfs", &sys_dir);
                        umount("dev", &dev_dir);
                    }
                    Err(e) => {
                        log::error!("Failed to copy '/tmp/all-images.tar' -> '{}': {}", dest, e);
                    }
                }
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::error!("Podman save failed: {}", stderr);
        }
        Err(e) => {
            log::error!("Failed to execute podman save: {}", e);
        }
    }
    } else {
        log::info!("Skipping Podman images backup (fresh install)");
    }

    log::info!("zuti-updater finished");
}

fn collect_subdir_names(dir: &str) -> Vec<String> {
    let path = Path::new(dir);
    if !path.exists() || !path.is_dir() {
        return Vec::new();
    }
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir()
                && let Some(name) = entry_path.file_name().and_then(|n| n.to_str())
            {
                names.push(name.to_string());
            }
        }
    }
    names
}
