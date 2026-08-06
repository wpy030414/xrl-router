use serde::Deserialize;
use tracing::warn;

/// Application configuration loaded from environment variables.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub port: u16,
    pub host: String,
    pub db_path: String,
    pub log_level: String,
    pub api_key: Option<String>,
    pub cors_origins: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 19068,
            host: "0.0.0.0".to_string(),
            db_path: "data/xrl-router.db".to_string(),
            log_level: "info".to_string(),
            api_key: None,
            cors_origins: vec![
                "http://localhost:5173".to_string(),
                "http://127.0.0.1:5173".to_string(),
                "http://localhost:19068".to_string(),
                "http://127.0.0.1:19068".to_string(),
                "tauri://localhost".to_string(),
                "https://tauri.localhost".to_string(),
                "http://tauri.localhost".to_string(),
            ],
        }
    }
}

impl Config {
    /// Load configuration from environment variables.
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(port) = std::env::var("PORT").and_then(|p| p.parse().map_err(|_| std::env::VarError::NotPresent)) {
            config.port = port;
        } else if std::env::var("PORT").is_ok() {
            warn!("Invalid PORT value, using default 19068");
        }

        if let Ok(host) = std::env::var("HOST") {
            config.host = host;
        }

        if let Ok(db_path) = std::env::var("DB_PATH") {
            config.db_path = db_path;
        }

        if let Ok(log_level) = std::env::var("LOG_LEVEL") {
            config.log_level = log_level;
        }

        if let Ok(api_key) = std::env::var("API_KEY") {
            if !api_key.is_empty() {
                config.api_key = Some(api_key);
            }
        }

        if let Ok(origins) = std::env::var("CORS_ORIGINS") {
            let parsed: Vec<String> = origins
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !parsed.is_empty() {
                config.cors_origins = parsed;
            }
        }

        config
    }

    /// Get the socket address string for the HTTP listener.
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
