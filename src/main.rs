use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::collections::HashSet;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

mod models;
mod config;
use models::*;
use config::logger;

const POOL_NAME: &str = "one-pool";

fn main() {
    logger::init_logger();

    // 检查 unsquashfs 命令是否存在
    if Command::new("unsquashfs").output().is_err() {
        log::warn!("unsquashfs command not found, upgrade functionality may be unavailable");
    }

    let socket_path = "/run/zuti-helper.sock";

    // 如果 socket 文件已存在，先删除
    if std::path::Path::new(socket_path).exists() {
        if let Err(e) = std::fs::remove_file(socket_path) {
            log::error!("Failed to remove existing socket: {}", e);
            std::process::exit(1);
        }
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
            Request::CreateDirectory(req) => handle_create_directory(req),
            Request::CreateZfsShare(req) => handle_create_zfs_share(req),
            Request::Upgrade(req) => handle_upgrade(req),
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

            // 2. 设置 mountpoint
            let set_mp = Command::new("zfs")
                .args(["set", &format!("mountpoint={}", mount_point), pool_name])
                .output();
            match set_mp {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    let stderr = String::from_utf8_lossy(&output.stderr);
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
                Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to import pool '{}': {}", POOL_NAME, stderr)),
                }
            }
        }
        Err(e) => Response {
            success: false,
            data: None,
            error: Some(format!(
                "Failed to execute zpool import for '{}': {}",
                pool_name, e
            )),
        },
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

fn handle_create_directory(req: CreateDirectoryRequest) -> Response {
    let directory = &req.directory;
    let owner = &req.owner;
    let arg = &req.arg;

    // 验证 directory 不为空
    if directory.is_empty() {
        return Response {
            success: false,
            data: None,
            error: Some("Directory path is required".to_string()),
        };
    }

    // 1. 创建目录
    match Command::new("mkdir").arg("-p").arg(directory).output() {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to create directory '{}': {}", directory, stderr)),
                };
            }
        }
        Err(e) => {
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute mkdir for '{}': {}", directory, e)),
            };
        }
    }

    // 2. 设置拥有者
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

    // 3. 设置权限
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

    let resp_data = CreateDirectoryResponse {
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
                        if real_path.to_string_lossy().ends_with(device) {
                            if file_name.starts_with("ata-") || file_name.starts_with("nvme-eui.")
                            {
                                return Ok(path.to_string_lossy().to_string());
                            }
                        }
                    } else {
                        let real_path_str = real_path.to_string_lossy();
                        if real_path_str == device_path {
                            if file_name.starts_with("ata-")
                                || (file_name.starts_with("nvme-") && !file_name.contains("-part"))
                            {
                                return Ok(path.to_string_lossy().to_string());
                            }
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

fn handle_create_zfs_share(req: CreateZfsShareRequest) -> Response {
    let share_name = &req.share_name;
    let dataset_name = &req.dataset_name;
    let quota = &req.quota;
    let samba_user = &req.samba_user;
    let mountpoint = format!("/{}/{}", dataset_name, share_name);

    let dataset = format!("{}/{}", dataset_name, share_name);

    // Step 0: 检查 dataset 是否已存在
    let check_output = Command::new("zfs")
        .args(["list", "-H", "-o", "name", &dataset])
        .output();

    let dataset_exists = match check_output {
        Ok(result) => result.status.success(),
        Err(_) => false,
    };

    // Step 1: 如果 dataset 不存在则创建，已存在则设置 sharesmb=on
    if !dataset_exists {
        let output = Command::new("zfs")
            .args([
                "create",
                "-o", "sharesmb=on",
                "-o", "compression=lz4",
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
                        error: Some(format!("Failed to create ZFS dataset '{}': {}", dataset, stderr)),
                    };
                }
            }
            Err(e) => {
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to execute zfs create '{}': {}", dataset, e)),
                };
            }
        }
    } else {
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
                    let _ = Command::new("zfs").args(["destroy", &dataset]).output();
                    return Response {
                        success: false,
                        data: None,
                        error: Some(format!("Failed to set quota '{}': {}", quota, stderr)),
                    };
                }
            }
            Err(e) => {
                let _ = Command::new("zfs").args(["destroy", &dataset]).output();
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to execute zfs set quota '{}': {}", quota, e)),
                };
            }
        }
    }

    // Step 3: zfs set mountpoint=<mountpoint> <pool>/<share_name>
    let output = Command::new("zfs")
        .args([
            "set",
            &format!("mountpoint={}", &mountpoint),
            &dataset,
        ])
        .output();

    match output {
        Ok(result) => {
            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                let _ = Command::new("zfs").args(["destroy", &dataset]).output();
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to set mountpoint '{}': {}", mountpoint, stderr)),
                };
            }
        }
        Err(e) => {
            let _ = Command::new("zfs").args(["destroy", &dataset]).output();
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute zfs set mountpoint '{}': {}", mountpoint, e)),
            };
        }
    }

    // Step 4: chown -R <samba_user>:<samba_user> <mountpoint>
    let output = Command::new("chown")
        .args([
            "-R",
            &format!("{}:{}", samba_user, samba_user),
            &mountpoint,
        ])
        .output();

    match output {
        Ok(result) => {
            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                let _ = Command::new("zfs").args(["destroy", &dataset]).output();
                return Response {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to set ownership for user '{}': {}", samba_user, stderr)),
                };
            }
        }
        Err(e) => {
            let _ = Command::new("zfs").args(["destroy", &dataset]).output();
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
            "ZFS share '{}' created successfully on pool '{}', mounted at '{}' with quota '{}'",
            share_name, dataset_name, mountpoint, quota
        ),
        error: None,
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
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to mount squashfs '{}': {}", file, stderr)),
            };
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&mount_dir);
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to execute mount for '{}': {}", file, e)),
            };
        }
    }

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
            return Response {
                success: false,
                data: None,
                error: Some("manifest.json missing 'version' field".to_string()),
            };
        }
    };

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
        return Response {
            success: false,
            data: None,
            error: Some(format!("Failed to create tmpdir '{}': {}", tmpdir, e)),
        };
    }

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

    // 6. zpool set bootfs=<dataset> <pool>
    let zpool_set = Command::new("zpool")
        .args(["set", &format!("bootfs={}", dataset_name), POOL_NAME])
        .output();
    match zpool_set {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            cleanup_upgrade(&mount_dir, &tmpdir);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to set bootfs for '{}': {}",
                    dataset_name, stderr
                )),
            };
        }
        Err(e) => {
            cleanup_upgrade(&mount_dir, &tmpdir);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to execute zpool set bootfs for '{}': {}",
                    dataset_name, e
                )),
            };
        }
    }

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
    let rootfs_path = format!("{}/rootfs.squashfs", mount_dir);
    let unsquashfs = Command::new("unsquashfs")
        .args(["-d", &tmpdir, "-f", "-da", "16", "-fr", "16", &rootfs_path])
        .output();
    match unsquashfs {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            cleanup_upgrade(&mount_dir, &tmpdir);
            return Response {
                success: false,
                data: None,
                error: Some(format!("Failed to unsquashfs '{}': {}", rootfs_path, stderr)),
            };
        }
        Err(e) => {
            cleanup_upgrade(&mount_dir, &tmpdir);
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Failed to execute unsquashfs '{}': {}",
                    rootfs_path, e
                )),
            };
        }
    }

    // 12. 卸载 mount_dir 并清理
    let _ = Command::new("umount").arg(&tmpdir).output();
    // let _ = std::fs::remove_dir_all(&tmpdir);

    let _ = Command::new("umount").arg(&mount_dir).output();
    // let _ = std::fs::remove_dir_all(&mount_dir);

    let resp_data = UpgradeResponse {
        success: true,
        message: format!(
            "Upgrade successful: dataset '{}' created and rootfs extracted",
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
    // let _ = Command::new("umount").arg(tmpdir).output();
    // let _ = Command::new("zfs").args(["umount", dataset_name]).output();
    // let _ = Command::new("zpool").args(["export", "-f", pool_name]).output();
    // let _ = Command::new("umount").arg(mount_dir).output();
    // let _ = std::fs::remove_dir_all(mount_dir);
    // let _ = std::fs::remove_dir_all(tmpdir);
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
