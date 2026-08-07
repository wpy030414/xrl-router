//! SQLite 数据访问层。
//!
//! `Database` 结构体与基础设施方法（连接、迁移、锁）定义在本文件；
//! 各实体的 CRUD 分散到 `providers`/`api_keys`/`service_keys`/`models`/
//! `usage`/`settings` 子模块，以独立 `impl Database` 块挂回。
//! 对外所有方法签名与调用方式（`db.save_provider(...)`）保持不变。

use rusqlite::{Connection, Result};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::info;

pub mod api_keys;
pub mod models;
pub mod providers;
pub mod schema;
pub mod service_keys;
pub mod settings;
pub mod usage;

/// Database wrapper for SQLite operations with thread-safe access.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open a new database connection with WAL mode enabled for better concurrency.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = NORMAL;"
        )?;
        info!("SQLite WAL mode enabled");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open an in-memory database (for tests).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run all pending migrations.
    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        // Create schema_version table if not exists
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );"
        )?;

        // Get current schema version
        let current_version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current_version >= schema::MIGRATIONS.len() as i64 {
            info!("Database schema is up to date (v{})", current_version);
            return Ok(());
        }

        info!(
            "Running database migrations from v{} to v{}...",
            current_version,
            schema::MIGRATIONS.len()
        );

        // Run pending migrations
        for (i, migration) in schema::MIGRATIONS.iter().enumerate().skip(current_version as usize) {
            let version = (i + 1) as i64;

            conn.execute_batch(migration)?;

            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (?, ?)",
                rusqlite::params![version, chrono::Utc::now().timestamp()],
            )?;

            info!("  Migration v{} applied", version);
        }

        info!("Database migrations complete");
        Ok(())
    }

    /// Get a lock on the database connection.
    pub fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    /// Test database connectivity.
    pub fn test_connection(&self) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute_batch("SELECT 1")?;
        Ok(())
    }

    /// Execute a query and return affected rows count.
    pub fn execute(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute(sql, params)
    }

    /// Execute a batch of SQL statements.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(sql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApiKey, Model, Provider, ProviderKind};

    /// 全新数据库应能从 V1 一路迁移到最新版本；价格相关列在 V9 被移除，
    /// usage_log 保留 cache token 列用于统计。
    #[test]
    fn test_full_migration_drops_cost_columns() {
        let db = Database::open_in_memory().expect("open in-memory db");
        db.migrate().expect("migrate from scratch");

        let conn = db.conn();
        // 价格相关列应全部被移除。
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(models)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            !cols.iter().any(|c| c.starts_with("cost_")),
            "cost columns must be dropped: {:?}",
            cols
        );

        // usage_log 应有 cache 列（V7），不应再有 cost_estimate（V9 移除）。
        let ucols: Vec<String> = conn
            .prepare("PRAGMA table_info(usage_log)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(ucols.contains(&"cache_read_input_tokens".to_string()));
        assert!(!ucols.contains(&"cache_creation_input_tokens".to_string()));
        assert!(!ucols.contains(&"cost_estimate".to_string()));
    }

    /// 回归测试：save_provider/save_api_key/save_model 必须用 UPSERT。
    /// 若用 INSERT OR REPLACE，REPLACE 会触发子表的 ON DELETE CASCADE，
    /// 更新 provider 时会把 models/api_keys 全部清空。
    #[test]
    fn test_save_does_not_cascade_delete_children() {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();

        let provider = Provider {
            id: "p1".to_string(),
            name: "P".to_string(),
            kind: ProviderKind::ChatCompletions,
            base_url: "https://example.com".to_string(),
            api_path: "/v1/chat/completions".to_string(),
            config: serde_json::json!({}),
            enabled: true,
            created_at: 1,
            updated_at: 1,
            sort_order: 0,
        };
        db.save_provider(&provider).unwrap();

        // 插入一个 model + 一个 key
        db.save_model(&Model {
            id: "m1".to_string(),
            provider_id: "p1".to_string(),
            model_id: "gpt-x".to_string(),
            display_name: "gpt-x".to_string(),
            tier: "custom".to_string(),
            context_window: 128000,
            max_output_tokens: 4096,
            capabilities: "[\"text\"]".to_string(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
        db.save_api_key(&ApiKey {
            id: "k1".to_string(),
            provider_id: "p1".to_string(),
            name: "K".to_string(),
            key_hash: "h".to_string(),
            key_masked: "m".to_string(),
            key_plain: None,
            status: "green".to_string(),
            last_error: None,
            last_error_code: None,
            last_error_time: None,
            last_used_at: None,
            balance: None,
            balance_updated_at: None,
            total_requests: 0,
            total_tokens: 0,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();

        // 更新 provider（模拟维护供应商保存）
        let mut updated = provider.clone();
        updated.name = "P2".to_string();
        updated.updated_at = 2;
        db.save_provider(&updated).unwrap();

        // 子表必须完好（conn 锁必须在块内释放，Mutex 不可重入）
        let (models, keys): (i64, i64) = {
            let conn = db.conn();
            let models: i64 = conn
                .query_row("SELECT COUNT(*) FROM models WHERE provider_id='p1'", [], |r| r.get(0))
                .unwrap();
            let keys: i64 = conn
                .query_row("SELECT COUNT(*) FROM api_keys WHERE provider_id='p1'", [], |r| r.get(0))
                .unwrap();
            (models, keys)
        };
        assert_eq!(models, 1, "update must not cascade-delete models");
        assert_eq!(keys, 1, "update must not cascade-delete api_keys");

        // 更新 model 也不得触发 usage_log 问题（这里至少保证不丢行）
        let mut mu = db.get_model("m1").unwrap().unwrap();
        mu.display_name = "gpt-y".to_string();
        db.save_model(&mu).unwrap();
        let m2 = db.get_model("m1").unwrap().unwrap();
        assert_eq!(m2.display_name, "gpt-y");
    }
}
