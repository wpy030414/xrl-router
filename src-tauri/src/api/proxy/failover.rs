//! 故障转移（Failover）：provider 级冷却标记。
//!
//! 同一模型配置多个 provider 时，主 provider 请求失败（5xx/网络错误/超时/
//! key 耗尽）会切换到下一个候选。本模块维护内存级 provider 冷却表：
//! 失败的 provider 在一段时间内不再被优先尝试，避免「每次都先打坏的
//! provider 再失败一次」。2xx 成功时清除冷却，provider 恢复后立即参与。
//!
//! 纯内存、不持久化、不广播——与密钥健康（keys/pool/health.rs）同一哲学。

use crate::gateway::server::AppState;

/// provider 级冷却时长（秒）。5xx/网络/超时通常短暂（重启、抖动），
/// 60s 足够避免锤击坏 provider，又不惩罚快速恢复。
pub(crate) const PROVIDER_COOLDOWN_SECS: i64 = 60;

/// 标记 provider 失败：进入冷却（now + 60s）。
pub(super) fn mark_provider_failed(state: &AppState, provider_id: &str) {
    let expire = chrono::Utc::now().timestamp() + PROVIDER_COOLDOWN_SECS;
    let mut map = state.provider_cooldowns.write().unwrap();
    map.insert(provider_id.to_string(), expire);
}

/// 标记 provider 成功：清除冷却，恢复参与候选。
pub(super) fn mark_provider_ok(state: &AppState, provider_id: &str) {
    let mut map = state.provider_cooldowns.write().unwrap();
    map.remove(provider_id);
}

/// 该 provider 是否处于冷却期（开关开启时跳过）。
pub(super) fn is_provider_cooling(state: &AppState, provider_id: &str) -> bool {
    let map = state.provider_cooldowns.read().unwrap();
    match map.get(provider_id) {
        Some(&expire) => expire > chrono::Utc::now().timestamp(),
        None => false,
    }
}

/// 清空冷却表（开关关闭/测试用）。
#[cfg(test)]
pub(super) fn clear_cooldowns(state: &AppState) {
    let mut map = state.provider_cooldowns.write().unwrap();
    map.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;

    fn test_state() -> Arc<AppState> {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        Arc::new(AppState::new(
            crate::config::Config::default(),
            db,
            [7u8; 32],
            &std::env::temp_dir(),
        ))
    }

    #[test]
    fn test_mark_failed_enters_cooldown() {
        let state = test_state();
        assert!(!is_provider_cooling(&state, "p1"));
        mark_provider_failed(&state, "p1");
        assert!(is_provider_cooling(&state, "p1"));
        // 其他 provider 不受影响
        assert!(!is_provider_cooling(&state, "p2"));
    }

    #[test]
    fn test_mark_ok_clears_cooldown() {
        let state = test_state();
        mark_provider_failed(&state, "p1");
        mark_provider_ok(&state, "p1");
        assert!(!is_provider_cooling(&state, "p1"));
    }

    #[test]
    fn test_expired_cooldown_is_not_cooling() {
        let state = test_state();
        // 手动写入一个已过期的条目（模拟 60s 冷却结束）
        {
            let mut map = state.provider_cooldowns.write().unwrap();
            map.insert("p1".to_string(), chrono::Utc::now().timestamp() - 1);
        }
        assert!(!is_provider_cooling(&state, "p1"));
    }

    #[test]
    fn test_clear_cooldowns() {
        let state = test_state();
        mark_provider_failed(&state, "p1");
        mark_provider_failed(&state, "p2");
        clear_cooldowns(&state);
        assert!(!is_provider_cooling(&state, "p1"));
        assert!(!is_provider_cooling(&state, "p2"));
    }
}
