# Spec: 密钥池管理

## 目标

管理多个 Provider 的多个 API Key，实现轮询、健康状态跟踪、持久化。

## 数据结构

### KeyPool

```rust
pub struct KeyPool {
    keys: Arc<RwLock<HashMap<String, Vec<KeyEntry>>>>,
    current_index: Arc<RwLock<HashMap<String, usize>>>,
    database: Arc<RwLock<Option<Database>>>,
    key_stats_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
}
```

### KeyEntry

```rust
pub struct KeyEntry {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    pub key_hash: String,
    pub key_masked: String,
    pub status: KeyStatus,
    pub last_error_time: Option<i64>,
    pub total_requests: u64,
    pub total_tokens: u64,
}
```

### KeyStatus

```rust
pub enum KeyStatus {
    Green,   // 正常
    Yellow,  // 配额低（402/429），5分钟冷却
    Red,     // 失效（401/403），永久跳过
    Unknown, // 未验证，视为 Green
}
```

## 输入契约

### 选取下一个 Key

```rust
pub fn get_next_key(&self, provider_id: &str) -> Result<KeyEntry>
```

**返回**:
- `Ok(KeyEntry)` 可用的 key
- `Err(NoAvailableKeys)` 所有 key 都不可用

### 更新健康状态

```rust
pub fn mark_key_invalid(&self, provider_id: &str, key_hash: &str) -> Result<()>
pub fn mark_key_low_quota(&self, provider_id: &str, key_hash: &str) -> Result<()>
pub fn record_key_success(&self, provider_id: &str, key_hash: &str, tokens: u64) -> Result<()>
```

**注意**: 参数名为 `key_hash`，但实际传入的是解密后的明文 key（用于匹配 KeyEntry）。

## 输出契约

### 轮询逻辑

1. 从 `current_index` 开始轮询
2. 跳过 `Red` 状态的 key
3. 跳过 `Yellow` 状态且冷却期未满的 key（5分钟）
4. 返回第一个 `Green` 或 `Unknown` 的 key
5. 更新 `current_index` 并持久化

### 持久化

```sql
-- settings 表
INSERT INTO settings (key, value) VALUES ('keypool_index_provider123', '5')
ON CONFLICT(key) DO UPDATE SET value = excluded.value;
```

## 关键约束

1. **纯内存状态**: `KeyStatus` 仅存内存，启动时全部初始化为 `Green`
2. **指针持久化**: `current_index` 持久化到 `settings` 表
3. **冷却期**: `Yellow` 状态 5 分钟后自动恢复
4. **轮询公平**: 所有 key 轮询一遍后才会重复
5. **锁顺序**: 先锁 `keys`，再锁 `current_index`，避免死锁

## 错误处理

| 场景 | 行为 |
|------|------|
| 所有 key 都 Red | 返回 `NoAvailableKeys` |
| 所有 key 都 Yellow 且冷却中 | 返回 `NoAvailableKeys` |
| Provider 不存在 | 返回 `NoAvailableKeys` |
| 持久化失败 | 记录 warn 日志，不影响运行 |

## 实现位置

- `src-tauri/src/keys/pool/mod.rs` - 结构体定义
- `src-tauri/src/keys/pool/types.rs` - 类型定义
- `src-tauri/src/keys/pool/rotation.rs` - 轮询逻辑
- `src-tauri/src/keys/pool/health.rs` - 健康状态管理
- `src-tauri/src/keys/pool/persistence.rs` - 持久化

## 测试要求

1. **单元测试**: 轮询逻辑、状态转换、冷却期
2. **集成测试**: 持久化 + 恢复、多 Provider 并发
3. **边界测试**: 所有 key 都 Red、指针越界、持久化失败

## 完成标准

- [x] 轮询选取 key（跳过 Red/Yellow）
- [x] 健康状态管理（Green/Yellow/Red）
- [x] 冷却期（Yellow 5分钟）
- [x] 指针持久化（重启后继续）
- [x] 锁顺序正确（无死锁）
- [x] 通过所有单元测试
