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
pub mod combos;
pub mod conversations;
pub mod local_models;
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
        // Create schema_version table if not exists + 读当前版本（需持锁，块内用完即放）。
        let current_version: i64 = {
            let conn = self.conn.lock().unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (
                    version INTEGER PRIMARY KEY,
                    applied_at INTEGER NOT NULL
                );"
            )?;
            current_version_of(&conn)?
        };

        if current_version >= schema::MIGRATIONS.len() as i64 {
            info!("Database schema is up to date (v{})", current_version);
            return Ok(());
        }

        info!(
            "Running database migrations from v{} to v{}...",
            current_version,
            schema::MIGRATIONS.len()
        );

        self.run_pending_migrations(&schema::MIGRATIONS[current_version as usize..])?;

        info!("Database migrations complete");
        Ok(())
    }

    /// 按事务逐条执行待应用的迁移（供 migrate() 使用，测试也直接驱动以验证回滚）。
    /// 每个迁移 + 其 schema_version 写入在同一事务内原子提交：若迁移中途失败则
    /// 整体回滚，不会出现"列已加、版本号没跟上"的半应用状态（历史上 V23 曾因
    /// IF NOT EXISTS 解析失败卡死在此，见 schema.rs）。
    /// execute_batch 不会自动开事务（autocommit 逐句提交），必须显式 BEGIN/COMMIT。
    fn run_pending_migrations(&self, migrations: &[&str]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut version = current_version_of(&conn)?;
        for migration in migrations {
            version += 1;

            let sql = format!(
                "BEGIN IMMEDIATE;
                 {migration}
                 INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES ({version}, {now});
                 COMMIT;",
                now = chrono::Utc::now().timestamp(),
            );
            if let Err(e) = conn.execute_batch(&sql) {
                // execute_batch 失败不会自动 ROLLBACK：显式回滚，避免同一连接上
                // 残留未提交事务（连接关闭时 SQLite 也会自动回滚，双保险）。
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }

            info!("  Migration v{} applied", version);
        }
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
}

/// 读取当前 schema 版本（schema_version 表内最大版本号，不存在则视为 0）。
fn current_version_of(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0))
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

        // V21：local_models 应有 thinking 列（思考模式开关，默认 1）。
        let lcols: Vec<String> = conn
            .prepare("PRAGMA table_info(local_models)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(lcols.contains(&"thinking".to_string()));
    }

    /// local_models 的 thinking 列 save→get 往返（守护 INSERT 列/参数对齐与
    /// upsert SET 列表——位置索引错配是运行时错误，编译期查不出来）。
    #[test]
    fn test_local_model_thinking_roundtrip() {
        use crate::types::LocalModel;
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();

        let mut m = LocalModel {
            id: "lm-test".to_string(),
            repo_id: "r/x".to_string(),
            filename: "x.gguf".to_string(),
            format: "gguf".to_string(),
            backend: "metal".to_string(),
            status: "downloaded".to_string(),
            model_id: "x".to_string(),
            ctx_size: 8192,
            n_gpu_layers: 99,
            autostart: 1,
            thinking: 0,
            file_size: None,
            local_path: "/tmp/x.gguf".to_string(),
            port: None,
            created_at: 1,
            updated_at: 1,
        };
        db.save_local_model(&m).unwrap();
        let got = db.get_local_model("lm-test").unwrap().unwrap();
        assert_eq!(got.thinking, 0, "thinking=0 应完整往返");

        // upsert 更新：同 id 再存一次，thinking 翻为 1
        m.thinking = 1;
        m.updated_at = 2;
        db.save_local_model(&m).unwrap();
        let got = db.get_local_model("lm-test").unwrap().unwrap();
        assert_eq!(got.thinking, 1, "upsert 应更新 thinking");

        // list 同样读得到
        let all = db.list_local_models().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].thinking, 1);
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

    /// 回归测试：真实升级路径 V22→V23。曾因 V23 误用 SQLite 不支持的
    /// ADD COLUMN IF NOT EXISTS（仅 CREATE 语句支持）在解析期直接失败，
    /// 应用永远卡在 V22。此处从 V22（含已填数据的 conversations 表）手动
    /// 升到最新版，验证 last_message 两列被正确补上且数据无损。
    #[test]
    fn test_upgrade_from_v22_preserves_conversations() {
        // 手工搭建一个 V22 库：执行 V1..V22 迁移，再塞一条对话。
        let db = Database::open_in_memory().unwrap();
        {
            let conn = db.conn();
            for (i, m) in super::schema::MIGRATIONS.iter().take(22).enumerate() {
                conn.execute_batch(m).unwrap();
                conn.execute(
                    "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                    rusqlite::params![(i + 1) as i64, 1],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO conversations (fingerprint, service_key_id, service_key_name,
                    messages, message_count, request_count, first_user_message, created_at, updated_at)
                 VALUES ('fp1', 'sk1', 'key', '[]', 3, 2, 'hello', 1, 1)",
                [],
            )
            .unwrap();
        }

        // 从 V22 一路升到最新（含此前必挂的 V23）。
        db.migrate().expect("V22 → latest must succeed");

        let conn = db.conn();
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(conversations)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(cols.contains(&"last_message".to_string()));
        assert!(cols.contains(&"last_message_raw".to_string()));

        // 既有数据不丢，新列回填默认空串。
        let (count, first_user, last): (i64, String, String) = conn
            .query_row(
                "SELECT message_count, first_user_message, last_message FROM conversations WHERE fingerprint='fp1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 3);
        assert_eq!(first_user, "hello");
        assert_eq!(last, "");

        // 重复 migrate 幂等（事务 + INSERT OR REPLACE 保证无副作用）。
        drop(conn);
        db.migrate().expect("re-migrate must be a no-op");
    }

    /// 回归测试：迁移中途失败必须整体回滚——任何语句失败时既不能留下
    /// 半应用的表结构，也不能留下已推进的 schema_version（历史上正因
    /// 非事务执行 + 失败不记账，重跑才撞 duplicate column）。
    #[test]
    fn test_migration_failure_rolls_back() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
             CREATE TABLE conversations (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (1, 1)",
            [],
        )
        .unwrap();
        let db = Database {
            conn: Arc::new(Mutex::new(conn)),
        };

        // 注入一条非法迁移，模拟历史上 V23 的解析失败。
        let bad = "CREATE TABLE t2 (id INTEGER); THIS IS NOT SQL;";
        let err = db.run_pending_migrations(&[bad]).unwrap_err();
        assert!(err.to_string().contains("syntax error"), "err = {err}");

        // 事务回滚：t2 不应存在。
        let conn = db.conn();
        let has_t2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='t2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_t2, 0, "failed migration must not leave t2 behind");

        // schema_version 停在 V1，可安全重跑。
        let max: i64 = conn
            .query_row("SELECT COALESCE(MAX(version),0) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(max, 1);
    }
}
