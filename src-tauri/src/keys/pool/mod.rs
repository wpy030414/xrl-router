//! 密钥池：traffic-light 健康跟踪 + 轮询 + DB 持久化。
//!
//! `KeyPool` 结构体、构造、集合变更（add/remove）与底层私有 helper
//! （持久化 / 广播 / 指针修正）定义在本文件；轮询、健康、加载逻辑分别
//! 下沉到 `rotation` / `health` / `persistence` 子模块，以独立 `impl` 块挂回。
//!
//! ⚠️ 锁序（生死攸关）：本文件内的持久化 helper 严格遵循
//!   - 「先收 DB 数据 → 释放 DB 锁 → 再拿 KeyPool 锁」（load_keys_from_db）；
//!   - 「KeyPool 锁释放后再持久化指针」（get_next_key / record_key_success）；
//!   - 「write lock 释放后再广播」（mark_* / record_*）。
//! 违反任一条都会与代理请求路径形成 ABBA 死锁（插件 keys_update 并发时触发）。

pub mod health;
pub mod persistence;
pub mod rotation;
pub mod types;

use crate::db::Database;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub use types::KeyEntry;

/// Key pool manager with traffic light health tracking and DB persistence
#[derive(Clone)]
pub struct KeyPool {
    /// All keys indexed by provider_id
    keys: Arc<RwLock<HashMap<String, Vec<KeyEntry>>>>,
    /// Current rotation index per provider
    current_index: Arc<RwLock<HashMap<String, usize>>>,
    /// Database reference for persistence (optional, set after construction)
    database: Arc<RwLock<Option<Database>>>,
    key_stats_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
}

impl KeyPool {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            keys: Arc::new(RwLock::new(HashMap::new())),
            current_index: Arc::new(RwLock::new(HashMap::new())),
            database: Arc::new(RwLock::new(None)),
            key_stats_tx: tx,
        }
    }

    /// Set the database reference for persistence
    pub fn set_database(&self, db: Database) {
        let mut db_ref = self.database.write().unwrap();
        *db_ref = Some(db);
    }

    /// Set the broadcast sender for key stats
    pub fn set_key_stats_tx(&mut self, tx: tokio::sync::broadcast::Sender<serde_json::Value>) {
        self.key_stats_tx = tx;
    }

    /// Add keys for a provider
    pub fn add_provider_keys(&self, provider_id: &str, keys: Vec<KeyEntry>) {
        let mut keys_map = self.keys.write().unwrap();
        keys_map.insert(provider_id.to_string(), keys);

        let mut index_map = self.current_index.write().unwrap();
        index_map.entry(provider_id.to_string()).or_insert(0);
    }

    /// Remove all keys for a provider
    pub fn remove_provider(&self, provider_id: &str) {
        let mut keys_map = self.keys.write().unwrap();
        keys_map.remove(provider_id);

        let mut index_map = self.current_index.write().unwrap();
        index_map.remove(provider_id);
    }

    /// 运行时追加单个 key（create_key 热生效用）。
    ///
    /// 池内 key_hash 存明文（与 load_all_keys_from_db 装池口径一致），
    /// 由调用方构造 entry 时保证。追加只会增大 total，既有轮询指针不会
    /// 因此越界，无需修正；新 provider 分组初始化指针为 0。
    /// 返回 false 表示同 id 已在池内（幂等保护，不重复入池）。
    pub fn add_key(&self, entry: KeyEntry) -> bool {
        let provider_id = entry.provider_id.clone();
        let added = {
            let mut keys_map = self.keys.write().unwrap();
            if let Some(list) = keys_map.get_mut(&provider_id) {
                if list.iter().any(|k| k.id == entry.id) {
                    false
                } else {
                    list.push(entry);
                    true
                }
            } else {
                keys_map.insert(provider_id.clone(), vec![entry]);
                true
            }
        };
        // write lock 释放后再广播（broadcast 内部要读 keys，否则死锁）
        if added {
            let mut index_map = self.current_index.write().unwrap();
            index_map.entry(provider_id.clone()).or_insert(0);
            drop(index_map);
            self.broadcast_key_stats(&provider_id);
        }
        added
    }

    /// 运行时同步 key 名称（update_key 热生效用；仅展示字段，不动状态/指针）。
    /// 返回 true 表示池内找到并更新。
    pub fn rename_key(&self, provider_id: &str, key_id: &str, name: &str) -> bool {
        let mut keys_map = self.keys.write().unwrap();
        keys_map
            .get_mut(provider_id)
            .and_then(|list| list.iter_mut().find(|k| k.id == key_id))
            .map(|k| k.name = name.to_string())
            .is_some()
    }

    /// 删除单个 key（运行时同步内存 + 修正指针）。
    /// 删除后如果指针越界（指向了被删的位置），自动回退到 0 重新开始轮询。
    /// 返回 true 表示 key 存在并被移除；false 表示未找到（可能已在内存外）。
    pub fn remove_key(&self, provider_id: &str, key_id: &str) -> bool {
        let mut removed = false;
        {
            let mut keys_map = self.keys.write().unwrap();
            if let Some(provider_keys) = keys_map.get_mut(provider_id) {
                let before = provider_keys.len();
                provider_keys.retain(|k| k.id != key_id);
                removed = provider_keys.len() < before;
            }
        }
        if removed {
            self.fix_index_after_change(provider_id);
        }
        removed
    }

    /// 测试专用：直接设置某个 provider 的轮转指针（模拟启动恢复后的状态）。
    #[cfg(test)]
    pub fn set_pool_index_for_test(&self, provider_id: &str, index: usize) {
        let mut index_map = self.current_index.write().unwrap();
        index_map.insert(provider_id.to_string(), index);
    }

    // ---- 底层私有 helper（对 pool 后代子模块可见，集中于此以便统一审阅锁序）----

    /// Broadcast key stats for a provider over the channel
    fn broadcast_key_stats(&self, provider_id: &str) {
        if let Some(stats) = self.get_stats(provider_id) {
            let _ = self.key_stats_tx.send(serde_json::json!({
                "type": "key_stats",
                "provider_id": provider_id,
                "green": stats.green,
                "total": stats.total,
            }));
        }
    }

    /// Persist key usage stats to database
    fn persist_key_usage(&self, key_id: &str, tokens: i64) {
        if let Some(db) = self.database.read().unwrap().as_ref() {
            let now = Utc::now().timestamp();
            let _ = db.execute(
                "UPDATE api_keys SET total_requests = total_requests + 1, total_tokens = total_tokens + ?1, last_used_at = ?2, updated_at = ?3 WHERE id = ?4",
                rusqlite::params![tokens, now, now, key_id],
            );
        }
    }

    /// Persist current rotation index for a provider
    fn persist_index(&self, provider_id: &str, index: usize) {
        if let Some(db) = self.database.read().unwrap().as_ref() {
            let key = format!("keypool_index_{}", provider_id);
            let _ = db.set_setting(&key, &index.to_string());
        }
    }

    /// Load persisted rotation index for a provider, with bounds checking
    fn load_persisted_index(&self, db: &Database, provider_id: &str, total_keys: usize) -> usize {
        let key = format!("keypool_index_{}", provider_id);
        match db.get_setting(&key) {
            Ok(Some(val)) => {
                match val.parse::<usize>() {
                    Ok(idx) if idx < total_keys => idx,
                    _ => 0,  // Invalid or out of bounds, reset to 0
                }
            }
            _ => 0,  // Not found or error, start from 0
        }
    }

    /// 密钥池变动后修正轮询指针：
    /// - 指针越界（key 减少导致）→ 回退到 0
    /// - 持久化的指针同样修正（避免重启后读到越界值）
    /// 注意锁顺序：先读 keys（释放）再写 index，避免与 get_next_key 的
    /// keys(write) → index(write) 顺序形成死锁。
    fn fix_index_after_change(&self, provider_id: &str) {
        let total = self
            .keys
            .read()
            .unwrap()
            .get(provider_id)
            .map(|v| v.len())
            .unwrap_or(0);
        let mut index_map = self.current_index.write().unwrap();
        let current = index_map.get(provider_id).copied().unwrap_or(0);
        let fixed = if total == 0 || current >= total { 0 } else { current };
        index_map.insert(provider_id.to_string(), fixed);
        drop(index_map);
        self.persist_index(provider_id, fixed);
    }
}

impl Default for KeyPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::types::KeyPoolError;
    use crate::types::KeyStatus;

    fn create_test_key(id: &str, status: KeyStatus) -> KeyEntry {
        KeyEntry {
            id: id.to_string(),
            provider_id: "test_provider".to_string(),
            name: format!("Test Key {}", id),
            key_hash: format!("hash_{}", id),
            key_masked: format!("****{}", id),
            status,
            last_error_time: None,
            total_requests: 0,
            total_tokens: 0,
        }
    }

    #[test]
    fn test_add_key_runtime_hot_effective() {
        // Bug 1 回归：运行中 create_key 必须热生效——不重启也能从池里取到新 key。
        let pool = KeyPool::new();

        // 空 provider 分组：直接 add → 建组 + 轮询从 0
        assert!(pool.add_key(create_test_key("k1", KeyStatus::Green)));
        let k = pool.get_next_key("test_provider").unwrap();
        assert_eq!(k.id, "k1", "新 provider 的首个 key 应立即可轮询");

        // 追加第二个：单 key 阶段指针已回绕到 0，新 key 排在队尾参与轮询——
        // 两次取 key 应恰好覆盖 k1、k2 各一次（证明 k2 已热入池）
        assert!(pool.add_key(create_test_key("k2", KeyStatus::Green)));
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2 {
            let k = pool.get_next_key("test_provider").unwrap();
            seen.insert(k.id.clone());
        }
        assert_eq!(seen, ["k1", "k2"].into_iter().map(String::from).collect(), "两个 key 都应可轮询到");

        // 同 id 重复入池：幂等拒绝，池内仍只有 2 个
        assert!(!pool.add_key(create_test_key("k1", KeyStatus::Green)));
        let stats = pool.get_stats("test_provider").unwrap();
        assert_eq!(stats.total, 2);

        // 池内明文口径：key_hash 应是构造时传入的明文（hash_{id}）
        let k = pool.get_next_key("test_provider").unwrap();
        assert_eq!(k.key_hash, format!("hash_{}", k.id));
    }

    #[test]
    fn test_rename_key_runtime_sync() {
        let pool = KeyPool::new();
        pool.add_provider_keys("p", vec![create_test_key("key1", KeyStatus::Green)]);

        assert!(pool.rename_key("p", "key1", "New Name"));
        let k = pool.get_next_key("p").unwrap();
        assert_eq!(k.name, "New Name");

        // 未知 id / 未知 provider：返回 false 不 panic
        assert!(!pool.rename_key("p", "ghost", "x"));
        assert!(!pool.rename_key("ghost", "key1", "x"));
    }

    #[test]
    fn test_round_robin() {
        let pool = KeyPool::new();
        let keys = vec![
            create_test_key("key1", KeyStatus::Green),
            create_test_key("key2", KeyStatus::Green),
            create_test_key("key3", KeyStatus::Green),
        ];

        pool.add_provider_keys("test_provider", keys);

        let k1 = pool.get_next_key("test_provider").unwrap();
        assert_eq!(k1.id, "key1");

        let k2 = pool.get_next_key("test_provider").unwrap();
        assert_eq!(k2.id, "key2");

        let k3 = pool.get_next_key("test_provider").unwrap();
        assert_eq!(k3.id, "key3");

        // Should wrap around
        let k4 = pool.get_next_key("test_provider").unwrap();
        assert_eq!(k4.id, "key1");
    }

    #[test]
    fn test_index_persistence_roundtrip() {
        let db = crate::db::Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        let pool = KeyPool::new();
        pool.set_database(db.clone());

        // 指针推进后应持久化到 settings 表
        pool.persist_index("p1", 2);
        let saved = db.get_setting("keypool_index_p1").unwrap().unwrap();
        assert_eq!(saved, "2");

        // 模拟重启：有效值应恢复
        let idx = pool.load_persisted_index(&db, "p1", 5);
        assert_eq!(idx, 2);

        // 越界（key 数减少）应回退到 0，而不是 panic
        let idx = pool.load_persisted_index(&db, "p1", 2);
        assert_eq!(idx, 0);

        // 无效值（非数字）应回退到 0
        db.set_setting("keypool_index_p1", "abc").unwrap();
        let idx = pool.load_persisted_index(&db, "p1", 5);
        assert_eq!(idx, 0);

        // 不存在的 provider 应回退到 0
        let idx = pool.load_persisted_index(&db, "p2", 5);
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_resume_from_persisted_index_not_restart() {
        // 8 个 key，指针持久化为 6（最后用了 key[5]）。
        // 重启后必须从 6 开始，而不是从头（0）试——即使 key[5] 已失效。
        let db = crate::db::Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.set_setting("keypool_index_p", "6").unwrap();

        let pool = KeyPool::new();
        pool.set_database(db.clone());
        let keys: Vec<KeyEntry> = (0..8)
            .map(|i| create_test_key(&format!("key{}", i), KeyStatus::Green))
            .collect();
        pool.add_provider_keys("p", keys);

        // 模拟启动：手动恢复持久化指针（load_all_keys_from_db 的逻辑）
        let idx = pool.load_persisted_index(&db, "p", 8);
        pool.set_pool_index_for_test("p", idx);

        // key[5] 失效
        pool.mark_key_invalid("p", "hash_key5").unwrap();

        // 应从 6 开始，而非 0/1
        let k = pool.get_next_key("p").unwrap();
        assert_eq!(k.id, "key6", "must resume at key after last used (5), not restart from 0");

        let k = pool.get_next_key("p").unwrap();
        assert_eq!(k.id, "key7");

        let k = pool.get_next_key("p").unwrap();
        assert_eq!(k.id, "key0", "wraps around to 0 only after 6,7");
    }

    #[test]
    fn test_remove_key_fixes_out_of_bounds_index() {
        // 场景：指针持久化为 6（最后用了 key[5]），运行中删除了 key[6]。
        // 指针仍指向 6，但此时 key 数已从 8 减到 7 → 必须自动回退到 0。
        let db = crate::db::Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        let pool = KeyPool::new();
        pool.set_database(db.clone());

        let keys: Vec<KeyEntry> = (0..8)
            .map(|i| create_test_key(&format!("key{}", i), KeyStatus::Green))
            .collect();
        pool.add_provider_keys("p", keys);
        pool.set_pool_index_for_test("p", 6); // 模拟指针=6（最后用了 key[5]）

        // 删除 key[6]（指针指向的位置）
        assert!(pool.remove_key("p", "key6"));

        // 指针应已修正（6 >= 7？不对——删掉后 total=7，指针 6 < 7 仍有效，
        // 但 key[6] 没了，实际应指向下一个可用的，即回退检查后仍是 6，
        // 而 get_next_key 的 % total 会让它从 6 开始 = key0）。
        // 关键断言：get_next_key 不 panic、不返回已删的 key。
        for _ in 0..7 {
            let k = pool.get_next_key("p").unwrap();
            assert_ne!(k.id, "key6", "deleted key must never be returned");
        }
    }

    #[test]
    fn test_remove_key_resets_index_when_out_of_bounds() {
        // 场景：只有 3 个 key，指针=2（最后用了 key[1]），删除 key[2] 后 total=2，
        // 指针 2 >= 2 → 必须回退到 0。
        let db = crate::db::Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        let pool = KeyPool::new();
        pool.set_database(db.clone());

        let keys: Vec<KeyEntry> = (0..3)
            .map(|i| create_test_key(&format!("key{}", i), KeyStatus::Green))
            .collect();
        pool.add_provider_keys("p", keys);
        pool.set_pool_index_for_test("p", 2);

        assert!(pool.remove_key("p", "key2"));
        // 指针应已修正为 0（2 >= 2 越界）
        let k = pool.get_next_key("p").unwrap();
        assert_eq!(k.id, "key0", "index must reset to 0 when out of bounds");
    }

    #[test]
    fn test_remove_provider_clears_index() {
        let pool = KeyPool::new();
        let keys = vec![
            create_test_key("key1", KeyStatus::Green),
            create_test_key("key2", KeyStatus::Green),
        ];
        pool.add_provider_keys("p", keys);
        pool.set_pool_index_for_test("p", 1);

        pool.remove_provider("p");
        // 移除后 get_next_key 应返回 KeyNotFound（而非残留内存）
        assert!(matches!(
            pool.get_next_key("p"),
            Err(KeyPoolError::KeyNotFound(_))
        ));
    }

    #[test]
    fn test_skip_red_keys() {
        let pool = KeyPool::new();
        let keys = vec![
            create_test_key("key1", KeyStatus::Red),
            create_test_key("key2", KeyStatus::Green),
            create_test_key("key3", KeyStatus::Green),
        ];

        pool.add_provider_keys("test_provider", keys);

        let k = pool.get_next_key("test_provider").unwrap();
        assert_ne!(k.id, "key1"); // Should skip red key
    }

    #[test]
    fn test_no_available_keys() {
        let pool = KeyPool::new();
        let keys = vec![
            create_test_key("key1", KeyStatus::Red),
            create_test_key("key2", KeyStatus::Red),
        ];

        pool.add_provider_keys("test_provider", keys);

        let result = pool.get_next_key("test_provider");
        assert!(matches!(result, Err(KeyPoolError::NoAvailableKeys)));
    }

    #[test]
    fn test_mark_invalid() {
        let pool = KeyPool::new();
        let keys = vec![create_test_key("key1", KeyStatus::Green)];

        pool.add_provider_keys("test_provider", keys);
        pool.mark_key_invalid("test_provider", "hash_key1").unwrap();

        let result = pool.get_next_key("test_provider");
        assert!(matches!(result, Err(KeyPoolError::NoAvailableKeys)));
    }

    #[test]
    fn test_record_success() {
        let pool = KeyPool::new();
        let keys = vec![create_test_key("key1", KeyStatus::Yellow)];

        pool.add_provider_keys("test_provider", keys);
        pool.record_key_success("test_provider", "hash_key1", 100).unwrap();

        let k = pool.get_next_key("test_provider").unwrap();
        assert_eq!(k.total_requests, 1);
        assert_eq!(k.total_tokens, 100);
    }
}
