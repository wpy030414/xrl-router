//! 插件协议的数据结构：连接态、注册/心跳/配置消息、DB 记录。

use serde::{Deserialize, Serialize};

/// In-memory state for a connected plugin.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct PluginConnection {
    pub plugin_id: String,
    pub provider_id: Option<String>,
    pub base_url: String,
    pub api_path: String,
    pub kind: String,
    pub models: Vec<PluginModel>,
    pub last_heartbeat: i64,
}

/// Model info sent by a plugin during registration.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginModel {
    pub model_id: String,
    pub display_name: String,
    pub tier: String,
}

/// Register message sent by a plugin on WS connect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRegisterMsg {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub plugin_id: String,
    pub provider: PluginProviderInfo,
    #[serde(default)]
    pub models: Vec<PluginModel>,
    /// 插件项目根目录（TS + Hono 网关所在目录），供伴生启动 `pnpm run serve/login` 使用。
    /// 缺省不覆盖已存值（COALESCE 语义）。
    #[serde(default)]
    pub workdir: Option<String>,
}

/// Provider info within a register message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginProviderInfo {
    pub kind: String,
    pub base_url: String,
    pub api_path: String,
}

/// keys_update message.
/// V24 契约已废除密钥同步：Router 不再为插件管理密钥（收到即回错断开）。
/// 结构保留仅用于文档对照，实际不再反序列化。

/// heartbeat message.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHeartbeatMsg {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(default)]
    pub timestamp: Option<i64>,
}

/// config_update message.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfigUpdateMsg {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_path: Option<String>,
}

/// Generic WS message from plugin (loosely typed for flexible parsing).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginWsMsg {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// Database record for a plugin.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct PluginRecord {
    pub id: String,
    pub provider_id: Option<String>,
    pub status: String,
    pub last_heartbeat_at: Option<i64>,
    /// 插件上报的项目根目录（TS + Hono 网关），伴生启动的 cwd。
    pub work_dir: Option<String>,
    /// 伴生启动开关：Router 启动时自动 `pnpm run serve` 拉起该网关。
    pub autostart: bool,
    pub created_at: i64,
    pub updated_at: i64,
}
