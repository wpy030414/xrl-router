use serde::{Deserialize, Serialize};

// Re-export from submodules
pub mod key;

pub use key::KeyStatus;

/// Provider entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_path: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// 手动排序权重：数值越小优先级越高（拖拽排序），撞名时优先。
    pub sort_order: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Messages,
    ChatCompletions,
    Responses,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderKind::Messages => write!(f, "messages"),
            ProviderKind::ChatCompletions => write!(f, "chat_completions"),
            ProviderKind::Responses => write!(f, "responses"),
        }
    }
}

impl ProviderKind {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "messages" => ProviderKind::Messages,
            "chat_completions" => ProviderKind::ChatCompletions,
            "responses" => ProviderKind::Responses,
            _ => ProviderKind::ChatCompletions,
        }
    }
}

/// API Key entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub key_hash: String,
    pub key_masked: String,
    pub key_plain: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
    pub last_error_code: Option<i32>,
    pub last_error_time: Option<i64>,
    pub last_used_at: Option<i64>,
    pub balance: Option<f64>,
    pub balance_updated_at: Option<i64>,
    pub total_requests: i64,
    pub total_tokens: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Model entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    pub tier: String,
    pub context_window: i64,
    pub max_output_tokens: i64,
    pub capabilities: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 组合别名：多个模型 display_name 按顺序捆绑成新别名，路由时依次尝试直到可用。
/// 成员列表存于 combo_members 表（TEXT 软引用），本结构体不含成员。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Combo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}
