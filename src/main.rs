use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;

// create_pool 请求结构体
#[derive(Deserialize, Debug)]
pub struct CreatePoolRequest {
    pub pool_name: String,
    pub pool_type: String, // single, strip, mirror, raidz1, raidz2, raidz3, raid10
    pub devices: Vec<String>, // 如 ["sda", "nvme0n1", "sdb1"]
}

// create_pool 响应结构体
#[derive(Serialize, Debug)]
pub struct CreatePoolResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// export_pool 请求结构体
#[derive(Deserialize, Debug)]
pub struct ExportPoolRequest {
    pub pool_name: String,
}

// export_pool 响应结构体
#[derive(Serialize, Debug)]
pub struct ExportPoolResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// import_pool 请求结构体
#[derive(Deserialize, Debug)]
pub struct ImportPoolRequest {
    pub pool_name: String,
    pub dir: Option<String>,
}

// import_pool 响应结构体
#[derive(Serialize, Debug)]
pub struct ImportPoolResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// 通用请求包装
#[derive(Deserialize, Debug)]
#[serde(tag = "action")]
pub enum Request {
    #[serde(rename = "create_pool")]
    CreatePool(CreatePoolRequest),
    #[serde(rename = "export_pool")]
    ExportPool(ExportPoolRequest),
    #[serde(rename = "import_pool")]
    ImportPool(ImportPoolRequest),
}

// 通用响应包装
#[derive(Serialize, Debug)]
pub struct Response {
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

fn main() {
    let socket_path = "/run/zuti-helper.sock";

    // 如果 socket 文件已存在，先删除
    if std::path::Path::new(socket_path).exists() {
        if let Err(e) = std::fs::remove_file(socket_path) {
            eprintln!("Failed to remove existing socket: {}", e);
            std::process::exit(1);
        }
    }

    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind to {}: {}", socket_path, e);
            std::process::exit(1);
        }
    };

    // 设置 socket 文件权限，允许所有本地用户连接
    if let Err(e) = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o666)) {
        eprintln!("Failed to set socket permissions: {}", e);
        std::process::exit(1);
    }

    println!("zuti-helper listening on {}", socket_path);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(|| handle_connection(stream));
            }
            Err(e) => {
                eprintln!("Connection failed: {}", e);
            }
        }
    }
}

fn handle_connection(mut stream: UnixStream) {
    println!("New connection from: {:?}", stream.peer_addr());

    let reader = match stream.try_clone() {
        Ok(r) => BufReader::new(r),
        Err(e) => {
            eprintln!("Failed to clone stream: {}", e);
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
            eprintln!("Failed to serialize response: {}", e);
            return false;
        }
    };

    if let Err(e) = writeln!(stream, "{}", json) {
        eprintln!("Failed to write response: {}", e);
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
    let import_result = if let Some(ref dir) = req.dir {
        if dir.is_empty() {
            // dir 为空字符串时，等同于 null
            Command::new("zpool")
                .args(["import", pool_name])
                .output()
        } else {
            // dir 有值时，使用 -R 选项
            Command::new("zpool")
                .args(["import", "-R", dir, pool_name])
                .output()
        }
    } else {
        // dir 为 null
        Command::new("zpool")
            .args(["import", pool_name])
            .output()
    };

    match import_result {
        Ok(output) => {
            if output.status.success() {
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
                    error: Some(format!("Failed to import pool '{}': {}", pool_name, stderr)),
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
                    error: Some(format!("Failed to export pool '{}': {}", pool_name, stderr)),
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

fn handle_create_pool(req: CreatePoolRequest) -> Response {
    let pool_name = &req.pool_name;
    let pool_type = req.pool_type.to_lowercase();
    let devices = &req.devices;

    // 2. 验证池名称合法性
    if !pool_name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Response {
            success: false,
            data: None,
            error: Some(
                "Pool name must contain only alphanumeric characters, underscores, or hyphens"
                    .to_string(),
            ),
        };
    }

    // 3. 验证 pool_type 和设备数量
    let min_devices = match pool_type.as_str() {
        "single" => 1,
        "strip" => 2,
        "mirror" => 2,
        "raidz1" => 3,
        "raidz2" => 4,
        "raidz3" => 5,
        "raid10" => 4,
        _ => {
            return Response {
                success: false,
                data: None,
                error: Some(
                    "Pool type must be one of: single, strip, mirror, raidz1, raidz2, raidz3, raid10"
                        .to_string(),
                ),
            };
        }
    };

    if devices.len() < min_devices {
        return Response {
            success: false,
            data: None,
            error: Some(format!(
                "Pool type '{}' requires at least {} devices, but only {} provided",
                pool_type,
                min_devices,
                devices.len()
            )),
        };
    }

    // 4. 验证设备名称合法性
    for device in devices {
        if !device
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return Response {
                success: false,
                data: None,
                error: Some(format!(
                    "Invalid device name: {}. Device name must be alphanumeric",
                    device
                )),
            };
        }
    }

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
