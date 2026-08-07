use serde::{Deserialize, Serialize};

/// Provider type (OpenAI Chat Completions, Anthropic Messages, OpenAI Responses)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// Provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_path: String,
    pub enabled: bool,
    pub config: ProviderConfig,
    pub created_at: i64,
    pub updated_at: i64,
    /// 手动排序权重：数值越小优先级越高（拖拽排序），撞名时优先。
    pub sort_order: i64,
}

/// Provider-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Deap-specific: wukong-penetrate endpoint URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub penetrate_url: Option<String>,

    /// Deap-specific: direct deap base URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deap_base_url: Option<String>,

    /// Deap-specific: business headers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deap_headers: Option<DeapHeaders>,

    /// Key source mode
    pub key_source: KeySource,

    /// Extra headers for custom providers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<Vec<HeaderPair>>,

    /// Auth header name (e.g., "Authorization" or "x-api-key")
    pub auth_header_name: String,

    /// Auth prefix (e.g., "Bearer " or "")
    pub auth_prefix: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            penetrate_url: None,
            deap_base_url: None,
            deap_headers: None,
            key_source: KeySource::StaticManaged,
            extra_headers: None,
            auth_header_name: "Authorization".to_string(),
            auth_prefix: "Bearer ".to_string(),
        }
    }
}

/// Deap-specific business headers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeapHeaders {
    pub user_type: String,
    pub scenario_code: String,
    pub product_code: String,
    pub ability_code: String,
    pub wukong_client_version: String,
    pub wukong_device_type: String,
    pub agent_loop_version: String,
    pub biz_param: String,
}

/// Custom header pair
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

/// Key source mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    /// Keys managed locally via admin API
    StaticManaged,
    /// Keys fetched from external service (e.g., wukong-penetrate)
    Delegate,
}

/// Delegate key configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateKeyConfig {
    /// Endpoint URL (e.g., "http://127.0.0.1:19067")
    pub endpoint: String,
    /// How often to poll (seconds)
    pub poll_interval_secs: u32,
    /// Balance endpoint path (e.g., "/user/balance")
    pub balance_endpoint: String,
    /// Info endpoint path (e.g., "/")
    pub info_endpoint: String,
}
