//! 只读管理桥接的宿主信息类型。SDK 不自行连接桥接或取得认证令牌。
use serde::{Deserialize, Serialize};

/// 描述被查询的宿主，不代表某个插件已获授权或已启用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub abi_version: u32,
    pub platform: String,
    pub arch: String,
    /// 安装清单接受的声明。仍受能力依赖、用户启用和每项权限检查约束。
    /// `appserver.call.v1` 仅为兼容声明，不授予网络访问权限。
    pub accepted_capabilities: Vec<String>,
    pub limits: HostLimits,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLimits {
    pub max_config_bytes: usize,
    pub max_message_bytes: usize,
    pub max_package_bytes: u64,
    pub transport_chunk_bytes: usize,
    pub max_transport_body_bytes: usize,
}
