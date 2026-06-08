use serde::{Deserialize, Serialize};

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
    pub mount_point: Option<String>,
    pub boot_enabled: Option<bool>,
}

// import_pool 响应结构体
#[derive(Serialize, Debug)]
pub struct ImportPoolResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// import_pool_plus 请求结构体
#[derive(Deserialize, Debug)]
pub struct ImportPoolPlusRequest {
    pub pool_name: String,
    pub force: Option<bool>,
    pub readonly: Option<bool>,
}

// import_pool_plus 响应结构体
#[derive(Serialize, Debug)]
pub struct ImportPoolPlusResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// create_dataset_and_directory 请求结构体
#[derive(Deserialize, Debug)]
pub struct CreateDatasetDirectoryRequest {
    pub directory: String,
    pub owner: String,
    pub arg: String, // 权限模式，如 "755"
}

// create_dataset_and_directory 响应结构体
#[derive(Serialize, Debug)]
pub struct CreateDatasetDirectoryResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// create_zfs_share 请求结构体
#[derive(Deserialize, Debug)]
pub struct CreateZfsShareRequest {
    pub dataset_name: String,
    pub quota: String,
    pub samba_user: String,
}

// upgrade 请求结构体
#[derive(Deserialize, Debug)]
pub struct UpgradeRequest {
    pub file: String,
    pub fresh_install: bool,
}

// upgrade 响应结构体
#[derive(Serialize, Debug)]
pub struct UpgradeResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// upgrading_progress 请求结构体
#[derive(Deserialize, Debug)]
pub struct UpgradingProgressRequest {}

// upgrading_progress 响应结构体
#[derive(Serialize, Debug)]
pub struct UpgradingProgressResponse {
    pub state: String, // "nope" 或 "upgrade"
    pub progress: u8,  // 0-100，当 state 为 "nope" 时为 0
}

// create_zfs_share 响应结构体
#[derive(Serialize, Debug)]
pub struct CreateZfsShareResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// update_zfs_share 请求结构体
#[derive(Deserialize, Debug)]
pub struct UpdateZfsShareRequest {
    pub dataset: String,
    pub owner: String,
    pub permission: String,
    pub guest_permission: String,
    pub directory: String,
}

// update_zfs_share 响应结构体
#[derive(Serialize, Debug)]
pub struct UpdateZfsShareResponse {
    pub success: bool,
    pub message: String,
    pub error: Option<String>,
}

// list_zfs_shares 请求结构体
#[derive(Deserialize, Debug)]
pub struct ListZfsSharesRequest {}

// ZFS SMB Share 信息
#[derive(Serialize, Debug)]
pub struct ZfsSmbShareInfo {
    pub dataset: String,
    pub owner: String,
    pub permission: String,
    pub guest_permission: String,
}

// list_zfs_shares 响应结构体
#[derive(Serialize, Debug)]
pub struct ListZfsSharesResponse {
    pub success: bool,
    pub shares: Vec<ZfsSmbShareInfo>,
    pub message: String,
    pub error: Option<String>,
}

// zfs_share_info 请求结构体
#[derive(Deserialize, Debug)]
pub struct ZfsShareInfoRequest {
    pub dataset: String,
}

// zfs_share_info 数据
#[derive(Serialize, Debug)]
pub struct ZfsShareInfoData {
    pub owner: String,
    pub permission: String,
    pub guest_permission: String,
    pub quota: String,
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
    #[serde(rename = "import_pool_plus")]
    ImportPoolPlus(ImportPoolPlusRequest),
    #[serde(rename = "create_dataset_and_directory")]
    CreateDatasetDirectory(CreateDatasetDirectoryRequest),
    #[serde(rename = "create_zfs_share")]
    CreateZfsShare(CreateZfsShareRequest),
    #[serde(rename = "update_zfs_share")]
    UpdateZfsShare(UpdateZfsShareRequest),
    #[serde(rename = "upgrade")]
    Upgrade(UpgradeRequest),
    #[serde(rename = "upgrading_progress")]
    UpgradingProgress(UpgradingProgressRequest),
    #[serde(rename = "list_zfs_shares")]
    ListZfsShares(ListZfsSharesRequest),
    #[serde(rename = "zfs_share_info")]
    ZfsShareInfo(ZfsShareInfoRequest),
}

// 通用响应包装
#[derive(Serialize, Debug)]
pub struct Response {
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}
