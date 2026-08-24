//! Service key 认证：查 service_keys 表 + argon2 校验（哈希函数见 `crypto`）。

use crate::gateway::server::AppState;

/// 已通过认证的 service key 快照（含 allowed_models 白名单与 token 配额）。
///
/// `pub(crate)`：代理 `/v1/*` 与 MCP 端点 `/mcp` 共用。
pub(crate) struct ServiceKeyInfo {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) key_masked: String,
    pub(super) allowed_models: Vec<String>,
    /// 5h 滚动窗口 token 上限，0 = 不设限。
    pub(super) quota_5h: i64,
    /// 7d 滚动窗口 token 上限，0 = 不设限。
    pub(super) quota_7d: i64,
}

/// Verify a service key against the service_keys table (argon2 hash).
/// Returns the service_key info on success, None on failure.
///
/// `pub(crate)`：代理 `/v1/*` 与 MCP 端点 `/mcp` 共用同一套 Service Key 鉴权。
pub(crate) async fn verify_service_key(state: &AppState, api_key: &str) -> Option<ServiceKeyInfo> {
    if api_key.is_empty() {
        return None;
    }

    // argon2 hashes are salted and not directly comparable, so enumerate and verify each.
    let conn = state.database.conn();
    let mut stmt = conn.prepare("SELECT id, name, key_masked, key_hash, allowed_models, quota_5h, quota_7d FROM service_keys").ok()?;

    let rows: Vec<(String, String, String, String, String, i64, i64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();

    for (id, name, key_masked, stored, allowed_str, quota_5h, quota_7d) in rows {
        if crate::crypto::verify_service_key(api_key, &stored) {
            let allowed_models: Vec<String> = serde_json::from_str(&allowed_str).unwrap_or_default();
            return Some(ServiceKeyInfo { id, name, key_masked, allowed_models, quota_5h, quota_7d });
        }
    }
    None
}
