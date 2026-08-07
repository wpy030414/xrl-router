//! 模型名 → 上游 URL 的路由解析（含插件委托 provider 的实时覆盖）。

use tracing::warn;

use crate::gateway::server::AppState;

/// 一条已解析的路由：上游 URL、provider/model 标识、（可选）插件 ID。
#[derive(Clone)]
pub(super) struct ResolvedRoute {
    pub(super) upstream_url: String,
    pub(super) provider_kind: String,
    pub(super) provider_id: String,
    pub(super) provider_name: String,
    pub(super) real_model_id: String,
    /// models.id (UUID primary key) — needed for usage_log.model_id FK.
    pub(super) model_row_id: String,
    /// Plugin ID if this is a delegated provider (None for regular providers).
    pub(super) plugin_id: Option<String>,
}

/// 从 KeyPool 取出的下一个可用 key（明文 hash + 标识）。
#[derive(Clone)]
pub(super) struct PickedKey {
    pub(super) key_hash: String,
    pub(super) id: String,
    pub(super) name: String,
    pub(super) key_masked: String,
}

/// Resolve a route for the given model name using V5 normalized schema.
/// 1. Look up model by display_name (alias) in models table
/// 2. JOIN providers to get base_url, api_path, kind, config_json
/// 3. If delegated (plugin_id in config_json), override base_url/api_path from PluginManager
/// 4. Return None if delegated provider's plugin is offline
pub(super) async fn resolve_route(state: &AppState, model_name: &str) -> Option<ResolvedRoute> {
    let conn = state.database.conn();

    // Find model by display_name (alias) ONLY — calling with the real model_id
    // is rejected; clients must use the alias.
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.model_id, m.provider_id, p.name, p.base_url, p.api_path, p.kind, p.config_json
             FROM models m
             JOIN providers p ON m.provider_id = p.id
             WHERE m.display_name = ?1
               AND m.enabled = 1
               AND p.enabled = 1
             ORDER BY p.sort_order ASC, p.created_at ASC
             LIMIT 1",
        )
        .ok()?;

    let (model_row_id, real_model_id, provider_id, provider_name, base_url, api_path, kind, config_json_str): (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = stmt
        .query_row([&model_name.to_string()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })
        .ok()?;

    // Check if this provider is delegated (has plugin_id in config_json)
    let config: serde_json::Value = serde_json::from_str(&config_json_str).unwrap_or_default();
    let plugin_id = config.get("plugin_id").and_then(|v| v.as_str()).map(String::from);

    // For delegated providers, override base_url/api_path from PluginManager (live WS data)
    let (final_base_url, final_api_path) = if let Some(ref pid) = plugin_id {
        // Plugin must be connected — otherwise this provider shouldn't be enabled
        // (disconnect() sets enabled=false), but double-check for safety.
        let pm_base = state.plugins.get_base_url(pid);
        let pm_path = state.plugins.get_api_path(pid);
        match (pm_base, pm_path) {
            (Some(b), Some(p)) => (b, p),
            _ => {
                warn!("Delegated provider {} has plugin_id {} but plugin is offline", provider_id, pid);
                return None;
            }
        }
    } else {
        (base_url, api_path)
    };

    let upstream_url = format!("{}{}", final_base_url, final_api_path);

    Some(ResolvedRoute {
        upstream_url,
        provider_kind: kind,
        provider_id,
        provider_name,
        real_model_id,
        model_row_id,
        plugin_id,
    })
}

/// 返回该 display_name 下全部可用候选（按 sort_order ASC, created_at ASC）。
/// 故障转移（failover）用：主 provider 失败后按序尝试下一个。
/// 跳过插件离线的委托 provider（与 resolve_route 的 None 语义一致，但继续下一行），
/// 按 provider_id 去重（UNIQUE(provider_id, model_id) 允许同 provider 多行同别名）。
/// 空列表返回 None（handler 映射 400，语义与 resolve_route 相同）。
pub(super) async fn resolve_route_candidates(
    state: &AppState,
    model_name: &str,
) -> Option<Vec<ResolvedRoute>> {
    let conn = state.database.conn();

    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.model_id, m.provider_id, p.name, p.base_url, p.api_path, p.kind, p.config_json
             FROM models m
             JOIN providers p ON m.provider_id = p.id
             WHERE m.display_name = ?1
               AND m.enabled = 1
               AND p.enabled = 1
             ORDER BY p.sort_order ASC, p.created_at ASC",
        )
        .ok()?;

    let mut rows = stmt
        .query_map([&model_name.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .ok()?;

    let mut candidates: Vec<ResolvedRoute> = Vec::new();
    let mut seen_providers: std::collections::HashSet<String> = std::collections::HashSet::new();

    while let Some(Ok((
        model_row_id,
        real_model_id,
        provider_id,
        provider_name,
        base_url,
        api_path,
        kind,
        config_json_str,
    ))) = rows.next()
    {
        // 同一 provider 多行同 display_name 只保留首个（排序靠前的行优先）
        if !seen_providers.insert(provider_id.clone()) {
            continue;
        }

        // 复用 resolve_route 的插件委托覆盖逻辑
        let config: serde_json::Value = serde_json::from_str(&config_json_str).unwrap_or_default();
        let plugin_id = config
            .get("plugin_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let (final_base_url, final_api_path) = if let Some(ref pid) = plugin_id {
            let pm_base = state.plugins.get_base_url(pid);
            let pm_path = state.plugins.get_api_path(pid);
            match (pm_base, pm_path) {
                (Some(b), Some(p)) => (b, p),
                _ => {
                    warn!("Delegated provider {} has plugin_id {} but plugin is offline", provider_id, pid);
                    continue;
                }
            }
        } else {
            (base_url, api_path)
        };

        let upstream_url = format!("{}{}", final_base_url, final_api_path);

        candidates.push(ResolvedRoute {
            upstream_url,
            provider_kind: kind,
            provider_id,
            provider_name,
            real_model_id,
            model_row_id,
            plugin_id,
        });
    }

    if candidates.is_empty() {
        None
    } else {
        Some(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::db::Database;
    use crate::types::{Model, Provider, ProviderKind};

    fn test_state() -> Arc<AppState> {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        Arc::new(AppState::new(
            crate::config::Config::default(),
            db,
            [7u8; 32],
        ))
    }

    fn add_provider(db: &Database, id: &str, sort_order: i64, config: serde_json::Value) {
        db.save_provider(&Provider {
            id: id.to_string(),
            name: format!("P-{}", id),
            kind: ProviderKind::ChatCompletions,
            base_url: format!("https://{}.example.com", id),
            api_path: "/v1/chat/completions".to_string(),
            config,
            enabled: true,
            created_at: 1,
            updated_at: 1,
            sort_order,
        })
        .unwrap();
    }

    fn add_model(db: &Database, id: &str, provider_id: &str, display_name: &str) {
        db.save_model(&Model {
            id: id.to_string(),
            provider_id: provider_id.to_string(),
            model_id: format!("real-{}", id),
            display_name: display_name.to_string(),
            tier: "custom".to_string(),
            context_window: 128000,
            max_output_tokens: 4096,
            capabilities: "[\"text\"]".to_string(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    }

    /// 候选按 sort_order ASC 排序；resolve_route 回归：只返回 sort_order 最小的一条。
    #[tokio::test]
    async fn test_candidates_sorted_and_resolve_route_regression() {
        let state = test_state();
        add_provider(&state.database, "p2", 0, serde_json::json!({}));
        add_provider(&state.database, "p1", 1, serde_json::json!({}));
        add_provider(&state.database, "p3", 2, serde_json::json!({}));
        for p in ["p1", "p2", "p3"] {
            add_model(&state.database, &format!("m-{}", p), p, "gpt-x");
        }

        let candidates = resolve_route_candidates(&state, "gpt-x").await.unwrap();
        let order: Vec<&str> = candidates.iter().map(|c| c.provider_id.as_str()).collect();
        assert_eq!(order, vec!["p2", "p1", "p3"], "候选应按 sort_order 升序");

        // 回归：resolve_route 仍只返回 sort_order 最小的一条
        let r = resolve_route(&state, "gpt-x").await.unwrap();
        assert_eq!(r.provider_id, "p2");
        assert_eq!(r.upstream_url, "https://p2.example.com/v1/chat/completions");
    }

    /// 插件离线的委托 provider 应从候选列表剔除；仍有正常候选时返回其余候选。
    #[tokio::test]
    async fn test_candidates_skip_offline_plugin() {
        let state = test_state();
        // 唯一候选是插件委托且插件未连接 → None
        add_provider(
            &state.database,
            "pp",
            0,
            serde_json::json!({"plugin_id": "plug-nonexistent"}),
        );
        add_model(&state.database, "m-pp", "pp", "gpt-x");
        assert!(resolve_route_candidates(&state, "gpt-x").await.is_none());

        // 补一个正常候选 → 返回正常候选
        add_provider(&state.database, "p1", 1, serde_json::json!({}));
        add_model(&state.database, "m-p1", "p1", "gpt-x");
        let candidates = resolve_route_candidates(&state, "gpt-x").await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].provider_id, "p1");
    }

    /// 同一 provider 多行同 display_name → 只保留一行（排序靠前的行）。
    #[tokio::test]
    async fn test_candidates_dedup_provider() {
        let state = test_state();
        add_provider(&state.database, "p1", 0, serde_json::json!({}));
        add_model(&state.database, "m1a", "p1", "gpt-x");
        add_model(&state.database, "m1b", "p1", "gpt-x");
        add_provider(&state.database, "p2", 1, serde_json::json!({}));
        add_model(&state.database, "m2", "p2", "gpt-x");

        let candidates = resolve_route_candidates(&state, "gpt-x").await.unwrap();
        assert_eq!(candidates.len(), 2, "同 provider 去重后应只剩两个候选");
        let order: Vec<&str> = candidates.iter().map(|c| c.provider_id.as_str()).collect();
        assert_eq!(order, vec!["p1", "p2"]);
    }

    /// 无任何匹配 → None（handler 映射 400）。
    #[tokio::test]
    async fn test_candidates_none_when_no_match() {
        let state = test_state();
        assert!(resolve_route_candidates(&state, "nonexistent").await.is_none());
    }
}
