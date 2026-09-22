//! 密钥选取（round-robin）与基于上游状态码的健康反馈。

use crate::gateway::server::AppState;
use crate::keys::KeyPool;

use super::route::{PickedKey, ResolvedRoute};

/// 插件委托候选的占位凭证：Router 不为插件管理密钥，恒发此占位值
/// （messages kind → `x-api-key`，其余 → `Authorization: Bearer`）。
/// 插件必须忽略它，以自身凭证（login 流程持有）访问上游。
pub(super) const PLUGIN_KEY_PLACEHOLDER: &str = "xrl-router";

/// Pick the next available key for a provider from the pool (round-robin,
/// skips Red/Yellow). Returns PickedKey with plaintext key, id, name, masked.
/// Called in the retry loop so 401/402/403/429 rotate keys.
///
/// 插件委托候选：不查池，恒返回一个占位密钥（内层循环天然单次尝试——
/// max_attempts 已在调用点强制为 1）。健康反馈对占位密钥天然 no-op
/// （池中无此条目），即无"红色拉黑"黏性——插件按 V24 契约以真实状态码
/// （401/403 会话失效、402/429 配额、5xx 上游故障）反馈可用性，
/// Router 的 failover/provider 冷却机制据此反应，每个请求都会重新尝试。
pub(super) fn pick_key_for(state: &AppState, cand: &ResolvedRoute) -> Option<PickedKey> {
    if cand.plugin_id.is_some() {
        return Some(PickedKey {
            id: format!("plugin:{}", cand.provider_id),
            name: cand.provider_name.clone(),
            key_hash: PLUGIN_KEY_PLACEHOLDER.to_string(),
            key_masked: PLUGIN_KEY_PLACEHOLDER.to_string(),
        });
    }
    match state.keys.get_next_key(&cand.provider_id) {
        Ok(entry) => Some(PickedKey {
            key_hash: entry.key_hash,
            id: entry.id,
            name: entry.name,
            key_masked: entry.key_masked,
        }),
        Err(_) => None,
    }
}

/// Drive the key pool health based on an upstream HTTP status code.
/// 401/403 -> red (invalid key), 402/429 -> yellow (quota/rate limit),
/// 2xx -> green (success). 5xx and other 4xx (400/404…) are NOT key problems,
/// so they leave the key state untouched.
/// 5xx 及其他非 key 状态不改变 key 健康度（由上层按通用错误处理）。
pub(super) fn update_key_health(pool: &KeyPool, provider_id: &str, key: &str, status: u16) {
    match status {
        401 | 403 => { let _ = pool.mark_key_invalid(provider_id, key); }
        402 | 429 => { let _ = pool.mark_key_low_quota(provider_id, key); }
        200..=299 => { let _ = pool.record_key_success(provider_id, key, 0); }
        _ => {}
    }
}
