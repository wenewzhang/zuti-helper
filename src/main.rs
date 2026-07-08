use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::collections::HashSet;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use zuti_helper::{config, models};
use models::*;
use config::logger;

#[derive(Debug, Clone)]
pub enum UpgradeProgressState {
    Nope,
    Upgrade(u8),
}

static UPGRADE_PROGRESS: OnceLock<Mutex<UpgradeProgressState>> = OnceLock::new();

fn get_upgrade_progress() -> &'static Mutex<UpgradeProgressState> {
    UPGRADE_PROGRESS.get_or_init(|| Mutex::new(UpgradeProgressState::Nope))
}

fn set_upgrade_progress(state: UpgradeProgressState) {
    if let Ok(mut guard) = get_upgrade_progress().lock() {
        *guard = state;
    }
}

const POOL_NAME: &str = "one-pool";

fn main() {
    logger::init_logger();

    // 检查 unsquashfs 命令是否存在
    if Command::new("unsquashfs").output().is_err() {
        log::warn!("unsquashfs command not found, upgrade functionality may be unavailable");
    }

    let socket_path = "/run/zuti-helper.sock";

    // 如果 socket 文件已存在，先删除
    if std::path::Path::new(socket_path).exists()
        && let Err(e) = std::fs::remove_file(socket_path)
    {
        log::error!("Failed to remove existing socket: {}", e);
        std::process::exit(1);
    }

    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(e) => {
            log::error!("Failed to bind to {}: {}", socket_path, e);
            std::process::exit(1);
        }
    };

    // 设置 socket 文件权限，允许所有本地用户连接
    if let Err(e) = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o666)) {
        log::error!("Failed to set socket permissions: {}", e);
        std::process::exit(1);
    }

    log::info!("zuti-helper listening on {}", socket_path);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(|| handle_connection(stream));
            }
            Err(e) => {
                log::error!("Connection failed: {}", e);
            }
        }
    }
}

fn handle_connection(mut stream: UnixStream) {
    log::info!("New connection from: {:?}", stream.peer_addr());

    let reader = match stream.try_clone() {
        Ok(r) => BufReader::new(r),
        Err(e) => {
            log::error!("Failed to clone stream: {}", e);
            return;
        }
    };

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        let request: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response {
                    success: false,
                    data: None,
                    error: Some(format!("Invalid JSON request: {}", e)),
                };
                send_response(&mut stream, &resp);
                continue;
            }
        };

        let resp = match request {
            Request::CreatePool(req) => handle_create_pool(req),
            Request::ExportPool(req) => handle_export_pool(req),
            Request::ImportPool(req) => handle_import_pool(req),
            Request::ImportPoolPlus(req) => handle_import_pool_plus(req),
            Request::CreateDatasetDirectory(req) => handle_create_dataset_and_directory(req),
            Request::CreateZfsShare(req) => handle_create_zfs_share(req),
            Request::UpdateZfsShare(req) => handle_update_zfs_share(req),
            Request::Upgrade(req) => handle_upgrade(req),
            Request::UpgradingProgress(req) => handle_upgrading_progress(req),
            Request::ListZfsShares(req) => handle_list_zfs_shares(req),
            Request::ZfsShareInfo(req) => handle_zfs_share_info(req),
        };

        if !send_response(&mut stream, &resp) {
            break;
        }
    }
}

fn send_response(stream: &mut UnixStream, resp: &Response) -> bool {
    let json = match serde_json::to_string(resp) {
        Ok(j) => j,
        Err(e) => {
            log::error!("Failed to serialize response: {}", e);
            return false;
        }
    };

    if let Err(e) = writeln!(stream, "{}", json) {
        log::error!("Failed to write response: {}", e);
        return false;
    }

    true
}

fn handle_import_pool(req: ImportPoolRequest) -> Response {
    let pool_name = &req.pool_name;

    // 验证 pool_name 不为空
    if pool_name.is_empty() {
        log::error!("Pool name is required");
        return Response {
            success: false,
            data: None,
            error: Some("Pool name is required".to_string()),
        };
    }

    // 构建 zpool import 命令
    let import_result = if let Some(ref mount_point) = req.mount_point {
        if mount_point.is_empty() {
            // mount_point 为空字符串时，等同于 null
            Command::new("zpool")
                .args(["import", pool_name])
                .output()
        } else {
            // mount_point 有值时，先临时导入设置 mountpoint，再正常导入
            let temp_dir = format!(
                "/tmp/zuti_helper_{}_{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            );
            if let Err(e) = std::fs::create_dir_all(&temp_dir) {
                log::error!("Failed to create temp dir '{}': {}", temp_dir, e);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to create temp dir '{}': {}", temp_dir, e)),
                };
            }

            // 1. 临时导入: zpool import -o readonly=on -R <temp_dir> <pool>
            let temp_import = Command::new("zpool")
                .args(["import", "-R", &temp_dir, pool_name])
                .output();
            match temp_import {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    log::error!("Failed to temp import pool '{}': {}", pool_name, stderr);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Failed to temp import pool '{}': {}",
                            pool_name, stderr
                        )),
                    };
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    log::error!("Failed to execute temp zpool import for '{}': {}", pool_name, e);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Failed to execute temp zpool import for '{}': {}",
                            pool_name, e
                        )),
                    };
                }
            }

            // 2. 设置 mountpoint（-u 只改属性，不立即挂载/卸载，避免临时目录 busy）
            let set_mp = Command::new("zfs")
                .args(["set", "-u", &format!("mountpoint={}", mount_point), pool_name])
                .output();
            match set_mp {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    log::error!("Failed to set mountpoint for '{}': {}", pool_name, stderr);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Failed to set mountpoint for '{}': {}",
                            pool_name, stderr
                        )),
                    };
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    log::error!("Failed to execute zfs set mountpoint for '{}': {}", pool_name, e);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Failed to execute zfs set mountpoint for '{}': {}",
                            pool_name, e
                        )),
                    };
                }
            }

            // 3. 导出
            let export_result = Command::new("zpool")
                .args(["export", pool_name])
                .output();
            match export_result {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    log::error!("Failed to export pool '{}': {}", pool_name, stderr);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Failed to export pool '{}': {}",
                            pool_name, stderr
                        )),
                    };
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    log::error!("Failed to execute zpool export for '{}': {}", pool_name, e);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Failed to execute zpool export for '{}': {}",
                            pool_name, e
                        )),
                    };
                }
            }

            // 4. 正常导入
            let final_import = Command::new("zpool")
                .args(["import", pool_name])
                .output();

            // 清理临时目录
            let _ = std::fs::remove_dir_all(&temp_dir);

            final_import
        }
    } else {
        // mount_point 为 null
        Command::new("zpool")
            .args(["import", pool_name])
            .output()
    };

    match import_result {
        Ok(output) => {
            if output.status.success() {             
                // 设置 canmount
                let canmount_value = if req.boot_enabled == Some(true) {
                    "on"
                } else {
                    "noauto"
                };
                let canmount_result = Command::new("zfs")
                    .args(["set", &format!("canmount={}", canmount_value), pool_name])
                    .output();
                if let Err(e) = canmount_result {
                    log::error!("Pool '{}' failed to set canmount: {}", pool_name, e);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Pool '{}' failed to set canmount: {}",
                            pool_name, e
                        )),
                    };
                }                   
                let resp_data = ImportPoolResponse {
                    success: true,
                    message: format!("Pool '{}' imported successfully", pool_name),
                    error: None,
                };
                Response {
                    success: true,
                    data: serde_json::to_value(resp_data).ok(),
                    error: None,
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::error!("Failed to import pool '{}': {}", pool_name, stderr);
                Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to import pool '{}': {}", pool_name, stderr)),
                }
            }
        }
        Err(e) => {
            log::error!("Failed to execute zpool import for '{}': {}", pool_name, e);
            Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to execute zpool import for '{}': {}",
                    pool_name, e
                )),
            }
        }
    }
}

fn handle_import_pool_plus(req: ImportPoolPlusRequest) -> Response {
    let pool_name = &req.pool_name;

    // 验证 pool_name 不为空
    if pool_name.is_empty() {
        log::error!("Pool name is required");
        return Response {
            success: false,
            data: None,
            error: Some("Pool name is required".to_string()),
        };
    }

    // 构建 zpool import 命令参数
    let mut args: Vec<String> = vec!["import".to_string()];

    // force 参数: -f
    if req.force == Some(true) {
        args.push("-f".to_string());
    }

    // readonly 参数: -o readonly=on
    if req.readonly == Some(true) {
        args.push("-o".to_string());
        args.push("readonly=on".to_string());
    }

    args.push(pool_name.clone());

    // 执行 zpool import 命令
    match Command::new("zpool").args(&args).output() {
        Ok(output) => {
            if output.status.success() {
                let resp_data = ImportPoolPlusResponse {
                    success: true,
                    message: format!("Pool '{}' imported successfully", pool_name),
                    error: None,
                };
                Response {
                    success: true,
                    data: serde_json::to_value(resp_data).ok(),
                    error: None,
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::error!("Failed to import pool '{}': {}", pool_name, stderr);
                Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to import pool '{}': {}", pool_name, stderr)),
                }
            }
        }
        Err(e) => {
            log::error!("Failed to execute zpool import for '{}': {}", pool_name, e);
            Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to execute zpool import for '{}': {}",
                    pool_name, e
                )),
            }
        }
    }
}

fn handle_export_pool(req: ExportPoolRequest) -> Response {
    let pool_name = &req.pool_name;

    // 验证 pool_name 不为空
    if pool_name.is_empty() {
        return Response {
            success: false,
            data: None,
            error: Some("Pool name is required".to_string()),
        };
    }

    // 执行 zpool export 命令
    match Command::new("zpool").args(["export", pool_name]).output() {
        Ok(output) => {
            if output.status.success() {
                let resp_data = ExportPoolResponse {
                    success: true,
                    message: format!("Pool '{}' exported successfully", pool_name),
                    error: None,
                };
                Response {
                    success: true,
                    data: serde_json::to_value(resp_data).ok(),
                    error: None,
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to export pool '{}': {}", POOL_NAME, stderr)),
                }
            }
        }
        Err(e) => Response {
            success: false,
            data: None,
            error: Some(format!("Failed to execute zpool export for '{}': {}", pool_name, e)),
        },
    }
}

fn handle_create_dataset_and_directory(req: CreateDatasetDirectoryRequest) -> Response {
    let directory = &req.directory;
    let owner = &req.owner;
    let arg = &req.arg;

    // 验证 directory 不为空
    if directory.is_empty() {
        log::error!("Directory path is required");
        return Response {
            success: false,
            data: None,
            error: Some("Directory path is required".to_string()),
        };
    }

    // 去除前后 '/'，将 /store/abcde/ 转换为 store/abcde 作为 ZFS dataset 名称
    let dataset = directory.trim_matches('/');

    // 1. 检查 dataset 是否已存在
    let dataset_exists = match Command::new("zfs")
        .args(["list", "-H", "-o", "name", dataset])
        .output()
    {
        Ok(result) => result.status.success(),
        Err(_) => false,
    };

    // 2. 若不存在则创建 ZFS dataset
    if !dataset_exists {
        match Command::new("zfs").arg("create").arg(dataset).output() {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!("Failed to create directory '{}': {}", dataset, stderr)),
                    };
                }
            }
            Err(e) => {
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to execute zfs create for '{}': {}", dataset, e)),
                };
            }
        }
    } else {
        log::info!("Dataset '{}' already exists, skipping zfs create", dataset);
    }

    // 3. 设置拥有者
    if !owner.is_empty() {
        match Command::new("chown").arg(owner).arg(directory).output() {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Failed to chown directory '{}' to '{}': {}",
                            directory, owner, stderr
                        )),
                    };
                }
            }
            Err(e) => {
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!(
                        "Failed to execute chown for '{}': {}",
                        directory, e
                    )),
                };
            }
        }
    }

    // 4. 设置权限
    if !arg.is_empty() {
        match Command::new("chmod").arg(arg).arg(directory).output() {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!(
                            "Failed to chmod directory '{}' with '{}': {}",
                            directory, arg, stderr
                        )),
                    };
                }
            }
            Err(e) => {
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!(
                        "Failed to execute chmod for '{}': {}",
                        directory, e
                    )),
                };
            }
        }
    }

    let resp_data = CreateDatasetDirectoryResponse {
        success: true,
        message: format!(
            "Directory '{}' created with owner '{}' and permissions '{}'",
            directory, owner, arg
        ),
        error: None,
    };
    Response {
        success: true,
        data: serde_json::to_value(resp_data).ok(),
        error: None,
    }
}

fn handle_create_pool(req: CreatePoolRequest) -> Response {
    let pool_name = &req.pool_name;
    let pool_type = req.pool_type.to_lowercase();
    let devices = &req.devices;

    // 5. 查找设备的 by-id 路径
    let mut device_by_ids: Vec<String> = Vec::new();
    for device in devices {
        match get_device_by_id(device) {
            Ok(id_path) => device_by_ids.push(id_path),
            Err(e) => {
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to resolve device {}: {}", device, e)),
                };
            }
        }
    }

    // 6. 构建 zpool create 命令
    let mut args: Vec<String> = vec![
        "create".to_string(),
        "-f".to_string(),
        "-o".to_string(),
        "ashift=12".to_string(),
    ];

    match pool_type.as_str() {
        "single" | "strip" => {
            args.push(pool_name.clone());
            args.extend(device_by_ids);
        }
        "mirror" => {
            args.push(pool_name.clone());
            args.push("mirror".to_string());
            args.extend(device_by_ids);
        }
        "raidz1" => {
            args.push(pool_name.clone());
            args.push("raidz1".to_string());
            args.extend(device_by_ids);
        }
        "raidz2" => {
            args.push(pool_name.clone());
            args.push("raidz2".to_string());
            args.extend(device_by_ids);
        }
        "raidz3" => {
            args.push(pool_name.clone());
            args.push("raidz3".to_string());
            args.extend(device_by_ids);
        }
        "raid10" => {
            if device_by_ids.len() < 2 || device_by_ids.len() % 2 != 0 {
                return Response {
                    success: false,
                    data: None,
                    error: Some(
                        "RAID10 requires an even number of disks (at least 2)".to_string(),
                    ),
                };
            }
            args.push(pool_name.clone());
            for chunk in device_by_ids.chunks(2) {
                args.push("mirror".to_string());
                args.extend(chunk.iter().cloned());
            }
        }
        _ => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Pool type '{}' is not supported", pool_type)),
            };
        }
    }

    // 7. 执行 zpool create 命令
    let output = match Command::new("zpool").args(&args).output() {
        Ok(result) => result,
        Err(e) => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute zpool create command: {}", e)),
            };
        }
    };

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let resp_data = CreatePoolResponse {
            success: true,
            message: format!(
                "Successfully created ZFS pool '{}' of type '{}' with {} device(s)",
                pool_name,
                pool_type,
                devices.len()
            ),
            error: if stdout.is_empty() {
                None
            } else {
                Some(stdout.to_string())
            },
        };
        Response {
            success: true,
            data: serde_json::to_value(resp_data).ok(),
            error: None,
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Response {
            success: false,
            data: None,
            error: Some(format!(
                "Failed to create ZFS pool '{}': {}",
                pool_name, stderr
            )),
        }
    }
}

// ==================== get_device_by_id helpers ====================

/// 获取设备的 by-id 路径
fn get_device_by_id(device: &str) -> Result<String, String> {
    let is_partition = device
        .chars()
        .last()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false);

    if is_partition {
        if device.starts_with("nvme") {
            if let Some(pos) = device.rfind('p') {
                let disk_name = &device[..pos];
                let part_suffix = &device[pos..]; // 包含 p
                return find_partition_by_id(disk_name, part_suffix);
            }
        } else {
            let chars: Vec<char> = device.chars().collect();
            let mut num_start = chars.len();
            for (i, c) in chars.iter().enumerate().rev() {
                if c.is_ascii_digit() {
                    num_start = i;
                } else {
                    break;
                }
            }
            if num_start < chars.len() {
                let disk_name: String = chars[..num_start].iter().collect();
                let part_num: String = chars[num_start..].iter().collect();
                return find_partition_by_id(&disk_name, &part_num);
            }
        }
    }

    find_disk_by_id(device)
}

/// 在 /dev/disk/by-id/ 下查找设备的长 ID
fn find_disk_by_id(device: &str) -> Result<String, String> {
    let is_partition = device
        .chars()
        .last()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false);
    let device_path = format!("/dev/{}", device);

    let entries = match std::fs::read_dir("/dev/disk/by-id/") {
        Ok(entries) => entries,
        Err(e) => return Err(format!("Failed to read /dev/disk/by-id/: {}", e)),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let file_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };

        if file_name.starts_with("scsi-")
            || file_name.starts_with("ata-")
            || file_name.starts_with("nvme-")
            || file_name.starts_with("wwn-")
        {
            match std::fs::canonicalize(&path) {
                Ok(real_path) => {
                    if is_partition {
                        if real_path.to_string_lossy().ends_with(device)
                            && (file_name.starts_with("ata-") || file_name.starts_with("nvme-eui."))
                        {
                            return Ok(path.to_string_lossy().to_string());
                        }
                    } else {
                        let real_path_str = real_path.to_string_lossy();
                        if real_path_str == device_path
                            && (file_name.starts_with("ata-")
                                || (file_name.starts_with("nvme-") && !file_name.contains("-part")))
                        {
                            return Ok(path.to_string_lossy().to_string());
                        }
                    }
                }
                Err(_) => continue,
            }
        }
    }

    Err(format!(
        "Cannot find long ID for device '{}' in /dev/disk/by-id/",
        device
    ))
}

/// 查找设备的分区 long ID
fn find_partition_by_id(disk_name: &str, part_suffix: &str) -> Result<String, String> {
    let device_path = format!("/dev/{}{}", disk_name, part_suffix);

    let entries = match std::fs::read_dir("/dev/disk/by-id/") {
        Ok(entries) => entries,
        Err(e) => return Err(format!("Failed to read /dev/disk/by-id/: {}", e)),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let file_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };

        if file_name.contains("-part") {
            match std::fs::canonicalize(&path) {
                Ok(real_path) => {
                    if real_path.to_string_lossy() == device_path {
                        return Ok(path.to_string_lossy().to_string());
                    }
                }
                Err(_) => continue,
            }
        }
    }

    Err(format!(
        "Cannot find partition ID for '{}{}'",
        disk_name, part_suffix
    ))
}

fn handle_update_zfs_share(req: UpdateZfsShareRequest) -> Response {
    let dataset = &req.dataset;
    let directory = &req.directory;

    // 验证参数不为空
    if dataset.is_empty() {
        log::error!("Dataset name is required");
        return Response {
            success: false,
            data: None,
            error: Some("Dataset name is required".to_string()),
        };
    }
    if directory.is_empty() {
        return Response {
            success: false,
            data: None,
            error: Some("Directory path is required".to_string()),
        };
    }

    let is_all_readonly = req.permission == "readonly" && req.guest_permission == "readonly";

    if is_all_readonly {
        // 直接设置 dataset readonly=on
        let output = match Command::new("zfs")
            .args(["set", "readonly=on", dataset])
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                log::error!("Failed to set readonly=on for dataset '{}': {}", dataset, e);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!(
                        "Failed to set readonly=on for dataset '{}': {}",
                        dataset, e
                    )),
                };
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::error!("Failed to set readonly=on for dataset '{}': {}", dataset, stderr);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to set readonly=on for dataset '{}': {}",
                    dataset, stderr
                )),
            };
        }
    } else {
        // 先确保 readonly=off
        let output = match Command::new("zfs")
            .args(["set", "readonly=off", dataset])
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                log::error!("Failed to set readonly=off for dataset '{}': {}", dataset, e);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!(
                        "Failed to set readonly=off for dataset '{}': {}",
                        dataset, e
                    )),
                };
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::error!("Failed to set readonly=off for dataset '{}': {}", dataset, stderr);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to set readonly=off for dataset '{}': {}",
                    dataset, stderr
                )),
            };
        }

        // 修改 mountpoint 的 owner
        let output = match Command::new("chown")
            .args(["-R", &format!("{}:{}", req.owner, req.owner), directory])
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                log::error!("Failed to chown for directory '{}': {}", directory, e);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!(
                        "Failed to chown for directory '{}': {}",
                        directory, e
                    )),
                };
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::error!("Failed to chown for directory '{}': {}", directory, stderr);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to chown for directory '{}': {}",
                    directory, stderr
                )),
            };
        }

        // 修改 mountpoint 的权限
        let owner_mod = if req.permission == "readonly" { "u-w+r+x" } else { "u+w+r+x" };
        let group_mod = match req.guest_permission.as_str() {
            "readonly" => "g-w+r+x",
            "none"     => "g=",
            _          => "g+w+r+x",
        };
        let guest_mod = match req.guest_permission.as_str() {
            "readonly" => "o-w+r+x",
            "none"     => "o=",
            _          => "o+w+r+x",
        };
        let chmod_arg = format!("{},{},{}", owner_mod, group_mod, guest_mod);
        log::info!("chmod_arg: {} directory: {}", chmod_arg, directory);

        let output = match Command::new("chmod")
            .args(["-R", &chmod_arg, directory])
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                log::error!("Failed to chmod for directory '{}': {}", directory, e);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!(
                        "Failed to chmod for directory '{}': {}",
                        directory, e
                    )),
                };
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::error!("Failed to chmod for directory '{}': {}", directory, stderr);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to chmod for directory '{}': {}",
                    directory, stderr
                )),
            };
        }
    }

    let resp_data = UpdateZfsShareResponse {
        success: true,
        message: format!(
            "ZFS share '{}' updated successfully, directory '{}'",
            dataset, directory
        ),
        error: None,
    };
    Response {
        success: true,
        data: serde_json::to_value(resp_data).ok(),
        error: None,
    }
}

fn handle_create_zfs_share(req: CreateZfsShareRequest) -> Response {
    let dataset = &req.dataset_name;
    let quota = &req.quota;
    let samba_user = &req.samba_user;
    // let mountpoint = format!("/{}/{}", dataset_name, share_name);

    // let dataset = format!("{}/{}", dataset_name, share_name);

    // Step 0: 检查 dataset 是否已存在
    let check_output = Command::new("zfs")
        .args(["list", "-H", "-o", "name", &dataset])
        .output();

    let dataset_exists = match check_output {
        Ok(result) => result.status.success(),
        Err(_) => false,
    };
    
    if !dataset_exists {
        return Response {
            success: false,
            data: None,
            error: Some(format!("Dataset '{}' does not exist", dataset)),
        };
    }            
    // 获取 dataset 实际 mountpoint
    let mp_output = match Command::new("zfs")
        .args(["get", "-H", "-o", "value", "mountpoint", dataset])
        .output()
    {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to get mountpoint for dataset '{}': {}", dataset, stderr)),
                };
            }
            output
        }
        Err(e) => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute zfs get mountpoint for '{}': {}", dataset, e)),
            };
        }
    };

    let mountpoint = String::from_utf8_lossy(&mp_output.stdout).trim().to_string();
    if mountpoint == "none" || mountpoint == "-" {
        return Response {
            success: false,
            data: None,
            error: Some(format!("Dataset '{}' is not mounted", dataset)),
        };
    }

    let output = Command::new("zfs")
        .args(["set", "sharesmb=on", &dataset])
        .output();

    match output {
        Ok(result) => {
            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to set sharesmb=on for dataset '{}': {}", dataset, stderr)),
                };
            }
        }
        Err(e) => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute zfs set sharesmb=on '{}': {}", dataset, e)),
            };
        }
    }
    // Step 2: zfs set quota=<quota> <pool>/<share_name>（quota 为 none 时跳过）
    if quota.to_lowercase() != "none" {
        let output = Command::new("zfs")
            .args([
                "set",
                &format!("quota={}", quota),
                &dataset,
            ])
            .output();

        match output {
            Ok(result) => {
                if !result.status.success() {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!("Failed to set quota '{}': {}", quota, stderr)),
                    };
                }
            }
            Err(e) => {
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to execute zfs set quota '{}': {}", quota, e)),
                };
            }
        }
    }

    // Step 4: chown -R <samba_user>:<samba_user> <mountpoint>
    let output = Command::new("chown")
        .args([
            &format!("{}:{}", samba_user, samba_user),
            &mountpoint,
        ])
        .output();

    match output {
        Ok(result) => {
            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to set ownership for user '{}': {}", samba_user, stderr)),
                };
            }
        }
        Err(e) => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute chown for user '{}': {}", samba_user, e)),
            };
        }
    }

    let resp_data = CreateZfsShareResponse {
        success: true,
        message: format!(
            "ZFS share '{}' created successfully, mounted at '{}' with quota '{}'",
            dataset, mountpoint, quota
        ),
        error: None,
    };
    Response {
        success: true,
        data: serde_json::to_value(resp_data).ok(),
        error: None,
    }
}


fn handle_list_zfs_shares(_req: ListZfsSharesRequest) -> Response {
    let output = match Command::new("zfs")
        .args(["get", "-H", "-o", "name,value", "sharesmb"])
        .output()
    {
        Ok(result) => result,
        Err(e) => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute zfs get command: {}", e)),
            };
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Response {
            success: false,
            data: None,
            error: Some(format!("Failed to list ZFS shares: {}", stderr)),
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut share_list = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1] == "on" {
            let dataset = parts[0].to_string();

            let mountpoint_output = Command::new("zfs")
                .args(["get", "-H", "-o", "value", "mountpoint", &dataset])
                .output();

            let (owner, permission, guest_permission) = match mountpoint_output {
                Ok(mp_result) if mp_result.status.success() => {
                    let mountpoint = String::from_utf8_lossy(&mp_result.stdout).trim().to_string();
                    let path = std::path::Path::new(&mountpoint);
                    if path.exists() && path.is_dir() {
                        match std::fs::metadata(path) {
                            Ok(metadata) => {
                                let uid = metadata.uid();
                                let mode = metadata.mode();
                                let owner_name = Command::new("id")
                                    .args(["-nu", &uid.to_string()])
                                    .output()
                                    .ok()
                                    .and_then(|out| {
                                        if out.status.success() {
                                            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                                        } else {
                                            None
                                        }
                                    })
                                    .unwrap_or_else(|| uid.to_string());

                                let readonly = Command::new("zfs")
                                    .args(["get", "-H", "-o", "value", "readonly", &dataset])
                                    .output()
                                    .ok()
                                    .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string() == "on")
                                    .unwrap_or(false);

                                let owner_perm = if readonly {
                                    "readonly".to_string()
                                } else if mode & 0o200 != 0 {
                                    "write".to_string()
                                } else {
                                    "readonly".to_string()
                                };

                                let guest_perm = if readonly {
                                    "readonly".to_string()
                                } else if mode & 0o002 != 0 {
                                    "write".to_string()
                                } else if mode & 0o004 != 0 {
                                    "readonly".to_string()
                                } else {
                                    "none".to_string()
                                };

                                (owner_name, owner_perm, guest_perm)
                            }
                            Err(_) => ("unknown".to_string(), "readonly".to_string(), "readonly".to_string()),
                        }
                    } else {
                        ("unknown".to_string(), "readonly".to_string(), "readonly".to_string())
                    }
                }
                _ => ("unknown".to_string(), "readonly".to_string(), "readonly".to_string()),
            };

            share_list.push(ZfsSmbShareInfo {
                dataset,
                owner,
                permission,
                guest_permission,
            });
        }
    }

    let resp_data = ListZfsSharesResponse {
        success: true,
        shares: share_list,
        message: "ZFS SMB shares listed successfully".to_string(),
        error: None,
    };
    Response {
        success: true,
        data: serde_json::to_value(resp_data).ok(),
        error: None,
    }
}

fn handle_zfs_share_info(req: ZfsShareInfoRequest) -> Response {
    let dataset = &req.dataset;

    if dataset.is_empty() {
        return Response {
            success: false,
            data: None,
            error: Some("Dataset name is required".to_string()),
        };
    }

    let output = match Command::new("zfs")
        .args(["get", "-H", "-o", "value", "mountpoint", dataset])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to get mountpoint for dataset '{}': {}", dataset, e)),
            };
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Response {
            success: false,
            data: None,
            error: Some(format!("Failed to get mountpoint for dataset '{}': {}", dataset, stderr)),
        };
    }
    let directory = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let path = std::path::Path::new(&directory);
    if !path.exists() || !path.is_dir() {
        return Response {
            success: false,
            data: None,
            error: Some(format!("Directory '{}' does not exist or is not a directory", directory)),
        };
    }

    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to get directory metadata: {}", e)),
            };
        }
    };

    let uid = metadata.uid();
    let mode = metadata.mode();

    let owner = Command::new("id")
        .args(["-nu", &uid.to_string()])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| uid.to_string());

    let readonly = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "readonly", dataset])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string() == "on")
        .unwrap_or(false);

    let quota = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "quota", dataset])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "none".to_string());

    let owner_permission = if readonly {
        "readonly".to_string()
    } else if mode & 0o200 != 0 {
        "write".to_string()
    } else {
        "readonly".to_string()
    };

    let guest_permission = if readonly {
        "readonly".to_string()
    } else if mode & 0o002 != 0 {
        "write".to_string()
    } else if mode & 0o004 != 0 {
        "readonly".to_string()
    } else {
        "none".to_string()
    };

    let resp_data = ZfsShareInfoData {
        owner,
        permission: owner_permission,
        guest_permission,
        quota,
    };
    Response {
        success: true,
        data: serde_json::to_value(resp_data).ok(),
        error: None,
    }
}

fn handle_upgrading_progress(_req: UpgradingProgressRequest) -> Response {
    let (state_str, progress) = match get_upgrade_progress().lock() {
        Ok(guard) => match *guard {
            UpgradeProgressState::Nope => ("nope".to_string(), 0u8),
            UpgradeProgressState::Upgrade(p) => ("upgrade".to_string(), p),
        },
        Err(_) => ("nope".to_string(), 0u8),
    };

    let resp_data = UpgradingProgressResponse {
        state: state_str,
        progress,
    };
    Response {
        success: true,
        data: serde_json::to_value(resp_data).ok(),
        error: None,
    }
}

fn handle_upgrade(req: UpgradeRequest) -> Response {
    let file = &req.file;

    // 验证 file 不为空且文件存在
    if file.is_empty() {
        return Response {
            success: false,
            data: None,
            error: Some("File path is required".to_string()),
        };
    }
    if !std::path::Path::new(file).exists() {
        return Response {
            success: false,
            data: None,
            error: Some(format!("File '{}' does not exist", file)),
        };
    }

    set_upgrade_progress(UpgradeProgressState::Upgrade(5));

    // 1. 创建临时挂载目录并挂载 squashfs
    let mount_dir = format!(
        "/tmp/zuti_update_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    if let Err(e) = std::fs::create_dir_all(&mount_dir) {
        return Response {
            success: false,
            data: None,
            error: Some(format!("Failed to create mount dir '{}': {}", mount_dir, e)),
        };
    }

    let mount_result = Command::new("mount")
        .args(["-t", "squashfs", file, &mount_dir])
        .output();
    match mount_result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let _ = std::fs::remove_dir_all(&mount_dir);
            set_upgrade_progress(UpgradeProgressState::Nope);
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to mount squashfs '{}': {}", file, stderr)),
            };
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&mount_dir);
            set_upgrade_progress(UpgradeProgressState::Nope);
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute mount for '{}': {}", file, e)),
            };
        }
    }

    set_upgrade_progress(UpgradeProgressState::Upgrade(10));

    // 2. 读取 manifest.json
    let manifest_path = format!("{}/manifest.json", mount_dir);
    let mut manifest_file = match File::open(&manifest_path) {
        Ok(f) => f,
        Err(e) => {
            let _ = Command::new("umount").arg(&mount_dir).output();
            let _ = std::fs::remove_dir_all(&mount_dir);
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to open manifest.json: {}", e)),
            };
        }
    };
    let mut manifest_str = String::new();
    if let Err(e) = manifest_file.read_to_string(&mut manifest_str) {
        let _ = Command::new("umount").arg(&mount_dir).output();
        let _ = std::fs::remove_dir_all(&mount_dir);
        return Response {
            success: false,
            data: None,
            error: Some(format!("Failed to read manifest.json: {}", e)),
        };
    }
    let manifest: serde_json::Value = match serde_json::from_str(&manifest_str) {
        Ok(v) => v,
        Err(e) => {
            let _ = Command::new("umount").arg(&mount_dir).output();
            let _ = std::fs::remove_dir_all(&mount_dir);
            set_upgrade_progress(UpgradeProgressState::Nope);
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to parse manifest.json: {}", e)),
            };
        }
    };
    let version = match manifest.get("version").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => {
            let _ = Command::new("umount").arg(&mount_dir).output();
            let _ = std::fs::remove_dir_all(&mount_dir);
            set_upgrade_progress(UpgradeProgressState::Nope);
            return Response {
                success: false,
                data: None,
                error: Some("manifest.json missing 'version' field".to_string()),
            };
        }
    };

    set_upgrade_progress(UpgradeProgressState::Upgrade(20));

    let dataset_name = format!("{}/ROOT/{}", POOL_NAME, version);

    // 3. udevadm trigger
    if let Err(e) = Command::new("udevadm").arg("trigger").output() {
        let _ = Command::new("umount").arg(&mount_dir).output();
        let _ = std::fs::remove_dir_all(&mount_dir);
        return Response {
            success: false,
            data: None,
            error: Some(format!("Failed to execute udevadm trigger: {}", e)),
        };
    }

    // 4. 创建临时目录 /mnt/xxx
    let tmpdir = format!(
        "/tmp/zuti_write_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    if let Err(e) = std::fs::create_dir_all(&tmpdir) {
        let _ = Command::new("umount").arg(&mount_dir).output();
        let _ = std::fs::remove_dir_all(&mount_dir);
        set_upgrade_progress(UpgradeProgressState::Nope);
        return Response {
            success: false,
            data: None,
            error: Some(format!("Failed to create tmpdir '{}': {}", tmpdir, e)),
        };
    }

    set_upgrade_progress(UpgradeProgressState::Upgrade(30));

    // 4.5 检查 dataset 是否已存在,如存在则生成新名称
    let dataset_name = if let Ok(output) = Command::new("zfs")
        .args(["list", "-H", "-o", "name"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let existing_datasets: HashSet<String> = stdout
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if existing_datasets.contains(&dataset_name) {
                let mut new_name = dataset_name.clone();
                for i in 1.. {
                    let probe = format!("{}-{}", dataset_name, i);
                    if !existing_datasets.contains(&probe) {
                        new_name = probe;
                        break;
                    }
                }
                new_name
            } else {
                dataset_name
            }
        } else {
            dataset_name
        }
    } else {
        dataset_name
    };

    // 5. zfs create -o canmount=noauto -o mountpoint=/ <dataset>
    let zfs_create = Command::new("zfs")
        .args(["create", "-o", "canmount=on", "-o", &format!("mountpoint={}",&tmpdir), &dataset_name])
        .output();
    match zfs_create {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            cleanup_upgrade(&mount_dir, &tmpdir);
            set_upgrade_progress(UpgradeProgressState::Nope);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to create ZFS dataset '{}': {}",
                    dataset_name, stderr
                )),
            };
        }
        Err(e) => {
            cleanup_upgrade(&mount_dir, &tmpdir);
            set_upgrade_progress(UpgradeProgressState::Nope);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to execute zfs create for '{}': {}",
                    dataset_name, e
                )),
            };
        }
    }

    set_upgrade_progress(UpgradeProgressState::Upgrade(40));

    // 10. 检查 mountpoint
    let is_mp = is_mountpoint(&tmpdir);
    log::info!("is_mountpoint('{}') = {}", tmpdir, is_mp);
    if !is_mp {
        cleanup_upgrade(&mount_dir, &tmpdir);
        return Response {
            success: false,
            data: None,
            error: Some(format!("Mountpoint '{}' is not mounted", tmpdir)),
        };
    }

    // 11. unsquashfs -d <tmpdir> -f -da 16 -fr 16 <mount_dir>/rootfs.squashfs
    // 改为后台任务执行，即时返回响应
    let rootfs_path = format!("{}/rootfs.squashfs", mount_dir);
    let mut child = match Command::new("unsquashfs")
        .args(["-d", &tmpdir, "-f", "-da", "16", "-fr", "16", &rootfs_path])
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            cleanup_upgrade(&mount_dir, &tmpdir);
            set_upgrade_progress(UpgradeProgressState::Nope);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to execute unsquashfs '{}': {}",
                    rootfs_path, e
                )),
            };
        }
    };

    set_upgrade_progress(UpgradeProgressState::Upgrade(60));

    let dataset_name_clone = dataset_name.clone();
    let mount_dir_clone = mount_dir.clone();
    let tmpdir_clone = tmpdir.clone();
    let fresh_install = req.fresh_install;
    std::thread::spawn(move || {
        match child.wait() {
            Ok(status) if status.success() => {
                log::info!("unsquashfs completed successfully for dataset '{}'", dataset_name_clone);
                set_upgrade_progress(UpgradeProgressState::Upgrade(80));

                // 配置 chroot 环境
                let run = |desc: &str, prog: &str, args: &[&str]| {
                    let cmd_str = format!("{} {}", prog, args.join(" "));
                    log::info!("Executing [{}]: {}", desc, cmd_str);
                    match Command::new(prog).args(args).output() {
                        Ok(output) if output.status.success() => {
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            if !stdout.trim().is_empty() {
                                log::info!("[{}] stdout: {}", desc, stdout.trim());
                            }
                        }
                        Ok(output) => {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            log::warn!("[{}] failed (exit code: {:?}): stderr: {} stdout: {}", desc, output.status.code(), stderr.trim(), stdout.trim());
                        }
                        Err(e) => {
                            log::warn!("[{}] failed to execute: {}", desc, e);
                        }
                    }
                };

                let t = &tmpdir_clone;
                run("chmod dpkg", "chmod", &["+x", &format!("{}/usr/bin/dpkg", t)]);
                run("chmod apt", "chmod", &["+x", &format!("{}/usr/bin/apt", t)]);
                run("chmod apt-get", "chmod", &["+x", &format!("{}/usr/bin/apt-get", t)]);

                run("mkdir proc/sys/dev", "mkdir", &["-p", &format!("{}/proc", t), &format!("{}/sys", t), &format!("{}/dev", t)]);
                run("mount proc", "mount", &["-t", "proc", "proc", &format!("{}/proc", t)]);
                run("mount sysfs", "mount", &["-t", "sysfs", "sys", &format!("{}/sys", t)]);
                run("mount dev", "mount", &["--bind", "/dev", &format!("{}/dev", t)]);

                set_upgrade_progress(UpgradeProgressState::Upgrade(85));

                let hostname = "onenas";
                run("set hostname", "sh", &["-c", &format!("echo '{}' > {}/etc/hostname", hostname, t)]);
                run("set hosts", "sh", &["-c", &format!("echo -e '127.0.1.1\\t{}' >> {}/etc/hosts", hostname, t)]);

                // 复制系统 apt sources 到 chroot 环境
                let src_list = "/etc/apt/sources.list";
                let dst_list = format!("{}/etc/apt/sources.list", t);
                if std::path::Path::new(src_list).exists() {
                    if let Err(e) = std::fs::copy(src_list, &dst_list) {
                        log::warn!("Failed to copy {} to {}: {}", src_list, dst_list, e);
                    } else {
                        log::info!("Copied {} -> {}", src_list, dst_list);
                    }
                }
                let src_list_d = "/etc/apt/sources.list.d";
                let dst_list_d = format!("{}/etc/apt/sources.list.d", t);
                if let Ok(entries) = std::fs::read_dir(src_list_d) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_file() {
                            let dst_file = format!("{}/{}", dst_list_d, path.file_name().unwrap_or_default().to_string_lossy());
                            if let Err(e) = std::fs::copy(&path, &dst_file) {
                                log::warn!("Failed to copy {:?} to {}: {}", path, dst_file, e);
                            } else {
                                log::info!("Copied {:?} -> {}", path, dst_file);
                            }
                        }
                    }
                }

                // 复制宿主系统 /etc/fstab 到目标系统
                let src_fstab = "/etc/fstab";
                let dst_fstab = format!("{}/etc/fstab", t);
                if std::path::Path::new(src_fstab).exists() {
                    if let Err(e) = std::fs::copy(src_fstab, &dst_fstab) {
                        log::warn!("Failed to copy {} to {}: {}", src_fstab, dst_fstab, e);
                    } else {
                        log::info!("Copied {} -> {}", src_fstab, dst_fstab);
                    }
                }

                run("dkms zfs.conf", "sh", &["-c", &format!("echo 'REMAKE_INITRD=yes' > {}/etc/dkms/zfs.conf", t)]);

                run("enable zfs.target", "chroot", &[t, "systemctl", "enable", "zfs.target"]);
                run("enable zfs-import-cache", "chroot", &[t, "systemctl", "enable", "zfs-import-cache"]);
                run("enable zfs-mount", "chroot", &[t, "systemctl", "enable", "zfs-mount"]);
                run("enable zfs-import.target", "chroot", &[t, "systemctl", "enable", "zfs-import.target"]);
                run("enable systemd-resolved", "chroot", &[t, "systemctl", "enable", "systemd-resolved"]);
                run("enable systemd-networkd", "chroot", &[t, "systemctl", "enable", "systemd-networkd"]);
                run("disable networking", "chroot", &[t, "systemctl", "disable", "networking"]);
                run("enable podman", "chroot", &[t, "systemctl", "enable", "podman"]);
                run("enable podman", "chroot", &[t, "systemctl", "enable", "podman-restart"]);
                run("enable nginx", "chroot", &[t, "systemctl", "enable", "nginx"]);

                set_upgrade_progress(UpgradeProgressState::Upgrade(90));

                run("update-initramfs", "chroot", &[t, "update-initramfs", "-c", "-k", "all"]);
                run("zfs set commandline", "chroot", &[t, "zfs", "set", "org.zfsbootmenu:commandline=\"loglevel=7\"", &format!("{}/ROOT", POOL_NAME)]);

                // 从宿主系统 /etc/shadow 获取 root 密码 hash 并写入目标系统
                let root_shadow_line = match Command::new("grep").args(["^root:", "/etc/shadow"]).output() {
                    Ok(output) if output.status.success() => {
                        String::from_utf8_lossy(&output.stdout).trim().to_string()
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        log::warn!("grep root shadow failed: {}", stderr);
                        String::new()
                    }
                    Err(e) => {
                        log::warn!("Failed to execute grep root shadow: {}", e);
                        String::new()
                    }
                };

                if !root_shadow_line.is_empty() {
                    let target_shadow = format!("{}/etc/shadow", t);
                    let mut shadow_content = String::new();
                    if std::path::Path::new(&target_shadow).exists()
                        && let Ok(content) = std::fs::read_to_string(&target_shadow)
                    {
                        shadow_content = content;
                    }
                    let mut lines: Vec<String> = shadow_content.lines().map(|s| s.to_string()).collect();
                    let mut found = false;
                    for line in &mut lines {
                        if line.starts_with("root:") {
                            *line = root_shadow_line.clone();
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        lines.push(root_shadow_line);
                    }
                    if let Err(e) = std::fs::write(&target_shadow, lines.join("\n") + "\n") {
                        log::warn!("Failed to write {}: {}", target_shadow, e);
                    } else {
                        log::info!("Updated root password in {}", target_shadow);
                    }
                }
                run("ssh-keygen", "chroot", &[t, "ssh-keygen", "-A"]);

                run("umount proc", "umount", &[&format!("{}/proc", t)]);
                run("umount sysfs", "umount", &[&format!("{}/sys", t)]);
                run("umount dev", "umount", &[&format!("{}/dev", t)]);
                // 11.5 重新设置 dataset mountpoint 为 /
                let zfs_set_canmount = Command::new("zfs")
                    .args(["set", "canmount=noauto", &dataset_name_clone])
                    .output();
                match zfs_set_canmount {
                    Ok(output) if output.status.success() => {}
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        log::error!("Failed to set canmount=noauto for '{}': {}", dataset_name_clone, stderr);
                    }
                    Err(e) => {
                        log::error!("Failed to execute zfs set canmount=noauto for '{}': {}", dataset_name_clone, e);
                    }
                }
                let zfs_set_mp = Command::new("zfs")
                    .args(["set", "-u", "mountpoint=/", &dataset_name_clone])
                    .output();
                match zfs_set_mp {
                    Ok(output) if output.status.success() => {}
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        log::error!("Failed to set mountpoint for '{}': {}", dataset_name_clone, stderr);
                    }
                    Err(e) => {
                        log::error!("Failed to execute zfs set mountpoint for '{}': {}", dataset_name_clone, e);
                    }
                }


    // 6. zpool set bootfs=<dataset> <pool>
                let zpool_set = Command::new("zpool")
                .args(["set", &format!("bootfs={}", dataset_name_clone), POOL_NAME])
                .output();

                match zpool_set {
                        Ok(output) if output.status.success() => {}
                        Ok(output) => {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            log::error!("Failed to set bootfs for '{}': {}", dataset_name_clone, stderr);
                        }
                        Err(e) => {
                            log::error!("Failed to set bootfs for '{}': {}", dataset_name_clone, e);
                        }                            
                }
              
                // 调用新系统中的 zuti-updater，将 tmpdir_clone 作为目标目录参数传入，等待执行完成后再继续
                let updater_path = format!("{}/usr/bin/zuti-updater", tmpdir_clone);
                let mut updater_cmd = Command::new(&updater_path);
                updater_cmd.arg(&tmpdir_clone);
                if fresh_install {
                    updater_cmd.arg("fresh_install");
                }
                match updater_cmd.output() {
                    Ok(output) if output.status.success() => {
                        log::info!("zuti-updater executed successfully");
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        log::error!("zuti-updater exited with error: {}", stderr);
                    }
                    Err(e) => {
                        log::error!("Failed to execute zuti-updater from '{}': {}", updater_path, e);
                    }
                }

                set_upgrade_progress(UpgradeProgressState::Upgrade(100));

                
                // 12. 卸载 mount_dir 并清理
                let _ = Command::new("umount").arg(&tmpdir_clone).output();
                let _ = Command::new("umount").arg(&mount_dir_clone).output();
                // set_upgrade_progress(UpgradeProgressState::Nope);
            }
            Ok(status) => {
                log::error!("unsquashfs exited with non-zero status for dataset '{}': {:?}", dataset_name_clone, status.code());
                let _ = Command::new("umount").arg(&tmpdir_clone).output();
                let _ = Command::new("umount").arg(&mount_dir_clone).output();
                set_upgrade_progress(UpgradeProgressState::Nope);
            }
            Err(e) => {
                log::error!("Failed to wait for unsquashfs process for dataset '{}': {}", dataset_name_clone, e);
                let _ = Command::new("umount").arg(&tmpdir_clone).output();
                let _ = Command::new("umount").arg(&mount_dir_clone).output();
                set_upgrade_progress(UpgradeProgressState::Nope);
            }
        }
    });

    let resp_data = UpgradeResponse {
        success: true,
        message: format!(
            "Upgrade started: dataset '{}' created, rootfs extraction running in background",
            dataset_name
        ),
        error: None,
    };
    Response {
        success: true,
        data: serde_json::to_value(resp_data).ok(),
        error: None,
    }
}

/// 清理 upgrade 过程中产生的临时资源（尽力而为）
fn cleanup_upgrade(mount_dir: &str, tmpdir: &str) {
    let _ = Command::new("umount").arg(tmpdir).output();
    let _ = Command::new("umount").arg(mount_dir).output();
}

/// 检查指定路径是否为挂载点（通过 mountpoint -q 命令）
fn is_mountpoint(path: &str) -> bool {
    std::process::Command::new("mountpoint")
        .arg("-q")
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
