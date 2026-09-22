//! 插件系统：外部服务经 WebSocket 注册为委托 Provider。
//!
//! 本文件为模块入口：定义 `PluginManager` 结构体与基础设施方法（构造、
//! AppHandle 注入、事件推送、共享 DB helper）。生命周期/健康/进程托管逻辑
//! 分别下沉到 `registry`/`health`/`host` 子模块，它们以独立 `impl` 块
//! 挂回 `PluginManager`。本文件内的私有方法（emit_event、DB helpers）
//! 对所有 plugin 后代子模块可见，供其复用。

pub mod health;
pub mod host;
pub mod registry;
pub mod types;

use dashmap::DashMap;
use std::sync::Arc;
use tauri::Emitter;

use crate::db::Database;

pub use host::PluginProcessManager;
pub use types::*;

/// Manages plugin connections and lifecycle.
#[derive(Clone)]
pub struct PluginManager {
    connections: Arc<DashMap<String, PluginConnection>>,
    database: Database,
    app_handle: Arc<std::sync::Mutex<Option<tauri::AppHandle>>>,
    /// Provider registry map for in-memory sync (register/confirm/disconnect update both DB + memory).
    /// Shared DashMap reference — the same map held by ProviderRegistry.
    providers: Arc<DashMap<String, crate::types::Provider>>,
}

impl PluginManager {
    pub fn new(
        database: Database,
        providers: Arc<DashMap<String, crate::types::Provider>>,
    ) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            database,
            app_handle: Arc::new(std::sync::Mutex::new(None)),
            providers,
        }
    }

    /// Set the Tauri AppHandle for emitting events to the frontend.
    /// Called from lib.rs setup() after Tauri is fully initialized.
    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        let mut h = self.app_handle.lock().unwrap();
        *h = Some(handle);
    }

    /// Emit a Tauri event to the frontend (no-op if AppHandle not set).
    fn emit_event(&self, event: &str, payload: serde_json::Value) {
        if let Some(handle) = self.app_handle.lock().unwrap().as_ref() {
            let _ = handle.emit(event, payload);
        }
    }

    // ---- DB helpers（私有，对 plugin 后代子模块可见）----

    /// 供管理 API 使用的公开记录查询（get_plugin_by_id 的 public 封装）。
    pub fn get_plugin_record(&self, id: &str) -> anyhow::Result<PluginRecord> {
        self.get_plugin_by_id(id)?
            .ok_or_else(|| anyhow::anyhow!("plugin not found: {}", id))
    }

    fn get_plugin_by_id(&self, id: &str) -> anyhow::Result<Option<PluginRecord>> {
        let conn = self.database.conn();
        let mut stmt = conn.prepare(
            "SELECT id, provider_id, status, last_heartbeat_at, work_dir, autostart, created_at, updated_at FROM plugins WHERE id = ?1"
        )?;
        let result = stmt.query_row(rusqlite::params![id], |row| {
            Ok(PluginRecord {
                id: row.get(0)?,
                provider_id: row.get(1)?,
                status: row.get(2)?,
                last_heartbeat_at: row.get(3)?,
                work_dir: row.get(4)?,
                autostart: row.get::<_, i64>(5)? != 0,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        });
        match result {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    #[allow(dead_code)]
    fn get_plugin_id_for_provider(&self, provider_id: &str) -> Option<String> {
        let conn = self.database.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM plugins WHERE provider_id = ?1"
        ).ok()?;
        stmt.query_row(rusqlite::params![provider_id], |row| row.get::<_, String>(0)).ok()
    }

    /// 保存插件记录。work_dir 仅在有值时覆盖（插件某次注册未带 workdir 不抹掉历史值）；
    /// **autostart 绝不出现在 UPSERT 中**——注册/重连不得复位用户的伴生启动开关
    /// （INSERT 时走列默认值 0）。
    fn save_plugin(
        &self,
        id: &str,
        provider_id: Option<&str>,
        status: &str,
        work_dir: Option<&str>,
        now: i64,
    ) -> anyhow::Result<()> {
        self.database.execute(
            "INSERT INTO plugins (id, provider_id, status, last_heartbeat_at, work_dir, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                provider_id=excluded.provider_id, status=excluded.status,
                last_heartbeat_at=excluded.last_heartbeat_at,
                work_dir=COALESCE(excluded.work_dir, plugins.work_dir),
                updated_at=excluded.updated_at",
            rusqlite::params![id, provider_id, status, now, work_dir, now, now],
        )?;
        Ok(())
    }

    /// 更新伴生启动开关（PATCH /api/plugins/:id 用）。
    pub fn set_plugin_autostart(&self, id: &str, autostart: bool) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.database.execute(
            "UPDATE plugins SET autostart = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, if autostart { 1 } else { 0 }, now],
        )?;
        Ok(())
    }

    /// 更新插件项目目录（PATCH /api/plugins/:id 用，None 表示清空）。
    pub fn set_plugin_workdir(&self, id: &str, work_dir: Option<&str>) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.database.execute(
            "UPDATE plugins SET work_dir = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, work_dir, now],
        )?;
        Ok(())
    }

    fn update_plugin_status(&self, id: &str, status: &str, provider_id: Option<&str>) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        if let Some(pid) = provider_id {
            self.database.execute(
                "UPDATE plugins SET status = ?1, provider_id = ?2, updated_at = ?3 WHERE id = ?4",
                rusqlite::params![status, pid, now, id],
            )?;
        } else {
            self.database.execute(
                "UPDATE plugins SET status = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![status, now, id],
            )?;
        }
        Ok(())
    }
}
