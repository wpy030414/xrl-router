use serde::{Deserialize, Serialize};

// Re-export from submodules
pub mod balance;
pub mod chat;
pub mod key;
pub mod model;
pub mod provider;
pub mod route;

pub use balance::BalanceInfo;
pub use key::KeyStatus;
pub use model::{Capability, ModelTier};
pub use provider::{
    DelegateKeyConfig, DeapHeaders, HeaderPair, KeySource, ProviderConfig,
};
pub use route::Route;

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
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Openai,
    Anthropic,
    Responses,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderKind::Openai => write!(f, "openai"),
            ProviderKind::Anthropic => write!(f, "anthropic"),
            ProviderKind::Responses => write!(f, "responses"),
        }
    }
}

impl ProviderKind {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "openai" => ProviderKind::Openai,
            "anthropic" => ProviderKind::Anthropic,
            "responses" => ProviderKind::Responses,
            _ => ProviderKind::Openai,
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
