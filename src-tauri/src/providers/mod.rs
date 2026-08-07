pub mod adapter;
pub mod anthropic;
pub mod openai;

use crate::db::Database;
use crate::types::{Provider, ProviderKind};
use anyhow::Result;
use dashmap::DashMap;
use rusqlite::params;
use std::sync::Arc;
use tracing::info;

/// Provider registry with in-memory cache backed by database.
#[derive(Clone)]
pub struct ProviderRegistry {
    database: Database,
    providers: Arc<DashMap<String, Provider>>,
}

impl ProviderRegistry {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            providers: Arc::new(DashMap::new()),
        }
    }

    /// Expose the underlying DashMap (shared Arc) for PluginManager
    /// to sync in-memory provider state (register/confirm/disconnect).
    pub fn providers_map(&self) -> Arc<DashMap<String, Provider>> {
        self.providers.clone()
    }

    /// Load all providers from database into memory cache.
    pub fn load_from_db(&self) -> Result<()> {
        let conn = self.database.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, base_url, api_path, enabled, config_json, created_at, updated_at, sort_order
             FROM providers ORDER BY sort_order, created_at"
        )?;

        let providers: Vec<Provider> = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let kind_str: String = row.get(2)?;
                let base_url: String = row.get(3)?;
                let api_path: String = row.get(4)?;
                let enabled_i: i32 = row.get(5)?;
                let config_json: String = row.get(6)?;
                let created_at: i64 = row.get(7)?;
                let updated_at: i64 = row.get(8)?;
                let sort_order: i64 = row.get(9)?;

                let kind = match kind_str.as_str() {
                    "openai" => ProviderKind::Openai,
                    "anthropic" => ProviderKind::Anthropic,
                    _ => ProviderKind::Openai,
                };

                let config: serde_json::Value = serde_json::from_str(&config_json)
                    .unwrap_or_else(|_| serde_json::json!({}));

                Ok(Provider {
                    id,
                    name,
                    kind,
                    base_url,
                    api_path,
                    enabled: enabled_i != 0,
                    config,
                    created_at,
                    updated_at,
                    sort_order,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        for p in providers {
            self.providers.insert(p.id.clone(), p);
        }

        info!("Loaded {} providers from database", self.providers.len());
        Ok(())
    }

    /// Get all enabled providers, ordered by sort_order (drag priority).
    pub fn get_enabled(&self) -> Vec<Provider> {
        let mut all: Vec<Provider> = self
            .providers
            .iter()
            .filter(|p| p.value().enabled)
            .map(|p| p.value().clone())
            .collect();
        all.sort_by_key(|p| p.sort_order);
        all
    }

    /// Find a provider by ID.
    pub fn find_by_id(&self, id: &str) -> Option<Provider> {
        self.providers.get(id).map(|p| p.value().clone())
    }

    /// Create a new provider.
    pub fn create(&self, provider: &Provider) -> Result<()> {
        let conn = self.database.conn();
        let config_json = serde_json::to_string(&provider.config)?;
        conn.execute(
            "INSERT INTO providers (id, name, kind, base_url, api_path, enabled, config_json, created_at, updated_at, sort_order)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                provider.id,
                provider.name,
                provider.kind.to_string(),
                provider.base_url,
                provider.api_path,
                provider.enabled as i32,
                config_json,
                provider.created_at,
                provider.updated_at,
                provider.sort_order,
            ],
        )?;
        self.providers.insert(provider.id.clone(), provider.clone());
        info!("Created provider: {}", provider.id);
        Ok(())
    }

    /// Delete a provider.
    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self.database.conn();
        conn.execute("DELETE FROM providers WHERE id = ?", params![id])?;
        self.providers.remove(id);
        info!("Deleted provider: {}", id);
        Ok(())
    }

    /// 批量重排（拖拽保存）：内存与 DB 同步写入 0..n 的 sort_order。
    pub fn reorder(&self, ids: &[String]) -> Result<()> {
        self.database.reorder_providers(ids)?;
        for (i, id) in ids.iter().enumerate() {
            if let Some(mut p) = self.providers.get_mut(id) {
                p.sort_order = i as i64;
                p.updated_at = chrono::Utc::now().timestamp();
            }
        }
        Ok(())
    }

    /// Get provider count.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Get all providers (not just enabled), ordered by sort_order (drag priority).
    /// DashMap 迭代无序，必须显式排序，否则拖拽后的顺序无法在列表/路由中体现。
    pub fn list_all(&self) -> Vec<Provider> {
        let mut all: Vec<Provider> = self.providers.iter().map(|p| p.value().clone()).collect();
        all.sort_by_key(|p| p.sort_order);
        all
    }

    /// Get a provider by ID.
    pub fn get(&self, id: &str) -> Option<Provider> {
        self.providers.get(id).map(|p| p.value().clone())
    }

    /// Insert or update a provider.
    pub fn insert(&self, provider: Provider) {
        self.providers.insert(provider.id.clone(), provider);
    }

    /// Check if a provider exists.
    pub fn contains(&self, id: &str) -> bool {
        self.providers.contains_key(id)
    }

    /// Remove a provider.
    pub fn remove(&self, id: &str) -> Option<Provider> {
        self.providers.remove(id).map(|(_, p)| p)
    }

    /// Create a new adapter for a provider with the given API key.
    pub fn create_adapter(
        &self,
        provider: &Provider,
        api_key: &str,
    ) -> Result<Box<dyn adapter::Adapter>> {
        use crate::providers::{anthropic::AnthropicAdapter, openai::OpenAIAdapter};

        let key = api_key.to_string();

        let adapter: Box<dyn adapter::Adapter> = match provider.kind {
            ProviderKind::Openai | ProviderKind::Responses => {
                Box::new(OpenAIAdapter::new(provider.base_url.clone(), key))
            }
            ProviderKind::Anthropic => {
                Box::new(AnthropicAdapter::new(provider.base_url.clone(), key))
            }
        };

        Ok(adapter)
    }
}
