//! 插件生命周期：注册（首次/重连）、确认激活、断连清理。
//!
//! 调用 `mod.rs` 的共享私有 helper（emit_event、save_plugin、
//! update_plugin_status、get_plugin_by_id）。
//!
//! V24 契约：Router 不再为插件管理密钥——注册消息不含 keys，
//! 凭证由插件（TS + Hono 反向代理网关）自行登录持有。

use tracing::{info, warn};

use crate::types::{Provider, ProviderKind};

use super::types::*;

impl super::PluginManager {
    /// Handle a plugin register message.
    /// Returns (provider_id, needs_confirmation).
    pub fn register(&self, msg: PluginRegisterMsg) -> anyhow::Result<(String, bool)> {
        let now = chrono::Utc::now().timestamp();

        // Check if plugin already exists in DB
        let existing = self.get_plugin_by_id(&msg.plugin_id)?;
        if let Some(plugin) = existing {
            // Reconnection: update connection info and re-enable provider
            if let Some(ref provider_id) = plugin.provider_id {
                let conn = PluginConnection {
                    plugin_id: msg.plugin_id.clone(),
                    provider_id: Some(provider_id.clone()),
                    base_url: msg.provider.base_url.clone(),
                    api_path: msg.provider.api_path.clone(),
                    kind: msg.provider.kind.clone(),
                    models: msg.models.clone(),
                    last_heartbeat: now,
                };
                self.connections.insert(msg.plugin_id.clone(), conn);

                // Re-enable provider
                self.database.execute(
                    "UPDATE providers SET enabled = 1, updated_at = ?1 WHERE id = ?2",
                    rusqlite::params![now, provider_id],
                )?;
                self.update_plugin_status(&msg.plugin_id, "active", Some(provider_id))?;
                // workdir 仅在有值时覆盖（COALESCE），不破坏已有目录
                if let Some(ref wd) = msg.workdir {
                    if !wd.trim().is_empty() {
                        self.database.execute(
                            "UPDATE plugins SET work_dir = ?2, updated_at = ?3 WHERE id = ?1",
                            rusqlite::params![msg.plugin_id, wd.trim(), now],
                        )?;
                    }
                }

                self.emit_event("plugin-online", serde_json::json!({
                    "plugin_id": msg.plugin_id,
                    "provider_id": provider_id,
                }));

                info!("Plugin reconnected: {} → provider {}", msg.plugin_id, provider_id);
                return Ok((provider_id.clone(), false));
            }
        }

        // New plugin: create provider + save plugin record
        let provider_id = uuid::Uuid::new_v4().to_string();
        let provider_name = msg.plugin_id.clone(); // default name = plugin_id

        let config = serde_json::json!({
            "plugin_id": msg.plugin_id,
            "delegated": true
        });

        let provider = Provider {
            id: provider_id.clone(),
            name: provider_name,
            kind: ProviderKind::from_str(&msg.provider.kind),
            base_url: msg.provider.base_url.clone(),
            api_path: msg.provider.api_path.clone(),
            config,
            enabled: false, // disabled until user confirms in dialog
            created_at: now,
            updated_at: now,
            sort_order: 0, // 插件注册的供应商默认排最前（V13 前无此列，历史行也是 0）
        };
        self.database.save_provider(&provider)?;
        // 同步内存 registry（register 时 ProviderRegistry 尚未加载，需手动插入）
        self.providers.insert(provider.id.clone(), provider.clone());

        // Save models
        for m in &msg.models {
            let model = crate::types::Model {
                id: uuid::Uuid::new_v4().to_string(),
                provider_id: provider_id.clone(),
                model_id: m.model_id.clone(),
                display_name: m.display_name.clone(),
                tier: m.tier.clone(),
                context_window: 128000,
                max_output_tokens: 4096,
                capabilities: "[\"text\",\"tools\"]".to_string(),
                enabled: true,
                created_at: now,
                updated_at: now,
            };
            self.database.save_model(&model)?;
        }

        // Save plugin record（work_dir 走 COALESCE 语义，仅非空覆盖）
        let work_dir = msg
            .workdir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        self.save_plugin(&msg.plugin_id, Some(&provider_id), "pending", work_dir, now)?;

        // Store connection in memory
        let conn = PluginConnection {
            plugin_id: msg.plugin_id.clone(),
            provider_id: Some(provider_id.clone()),
            base_url: msg.provider.base_url.clone(),
            api_path: msg.provider.api_path.clone(),
            kind: msg.provider.kind.clone(),
            models: msg.models.clone(),
            last_heartbeat: now,
        };
        self.connections.insert(msg.plugin_id.clone(), conn);

        // Emit event to frontend to show registration dialog
        self.emit_event("plugin-register", serde_json::json!({
            "plugin_id": msg.plugin_id,
            "provider_id": provider_id,
            "provider_name": provider.name,
            "kind": provider.kind.to_string(),
            "base_url": provider.base_url,
            "api_path": provider.api_path,
            "workdir": work_dir,
            "models": msg.models.iter().map(|m| serde_json::json!({
                "model_id": m.model_id,
                "display_name": m.display_name,
                "tier": m.tier,
            })).collect::<Vec<_>>(),
        }));

        info!("Plugin registered: {} → provider {}", msg.plugin_id, provider_id);
        Ok((provider_id, true))
    }

    /// Confirm a plugin (user clicked "add" in dialog). Enables the provider.
    pub fn confirm(&self, plugin_id: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.update_plugin_status(plugin_id, "active", None)?;

        // Enable the associated provider
        if let Some(conn) = self.connections.get(plugin_id) {
            if let Some(ref provider_id) = conn.provider_id {
                self.database.execute(
                    "UPDATE providers SET enabled = 1, updated_at = ?1 WHERE id = ?2",
                    rusqlite::params![now, provider_id],
                )?;
                // 同步内存 registry
                if let Some(mut p) = self.providers.get_mut(provider_id) {
                    p.enabled = true;
                    p.updated_at = now;
                }
                self.emit_event("plugin-activated", serde_json::json!({
                    "plugin_id": plugin_id,
                    "provider_id": provider_id,
                }));
            }
        }
        Ok(())
    }

    /// Handle plugin disconnect (WS closed).
    pub fn disconnect(&self, plugin_id: &str) {
        if let Some((_, conn)) = self.connections.remove(plugin_id) {
            if let Some(ref provider_id) = conn.provider_id {
                let now = chrono::Utc::now().timestamp();
                // Disable provider so resolve_route skips it
                let _ = self.database.execute(
                    "UPDATE providers SET enabled = 0, updated_at = ?1 WHERE id = ?2",
                    rusqlite::params![now, provider_id],
                );
                // 同步内存 registry
                if let Some(mut p) = self.providers.get_mut(provider_id) {
                    p.enabled = false;
                    p.updated_at = now;
                }
                let _ = self.update_plugin_status(plugin_id, "offline", Some(provider_id));
                self.emit_event("plugin-offline", serde_json::json!({
                    "plugin_id": plugin_id,
                    "provider_id": provider_id,
                }));
                warn!("Plugin disconnected: {} → provider {} disabled", plugin_id, provider_id);
            }
        }
    }
}
