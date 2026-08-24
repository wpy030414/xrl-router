# Spec: 数据库 Schema 与迁移

## 目标

管理 SQLite 数据库的 Schema 定义和版本迁移，确保数据一致性和向后兼容。

## 迁移架构

所有 DDL 以 Rust 字符串内联在 `schema.rs` 的 `MIGRATIONS` 数组中，**没有**独立的 `.sql` 文件。版本由 `MIGRATIONS.len()` 动态得出，当前为 **18**。

```rust
// db/schema.rs
pub const MIGRATIONS: &[&str] = &[
    // V1: 基础 Schema
    r#"CREATE TABLE IF NOT EXISTS providers (...);
       CREATE TABLE IF NOT EXISTS models (...);
       ..."#,
    // V2: 新增 service_keys
    r#"CREATE TABLE IF NOT EXISTS service_keys (...);
       CREATE INDEX ...;"#,
    // ... V3-V13 依次追加
];
```

```rust
// db/mod.rs
pub fn migrate(db: &Database) -> Result<()> {
    let current_version = db.get_schema_version()?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        if current_version < version {
            db.execute_batch(sql)?;
            db.set_schema_version(version)?;
        }
    }
    Ok(())
}
```

## 迁移历史

| 版本 | 改动 |
|------|------|
| V1 | 基础 Schema：providers, models, api_keys, usage_log, schema_version + 索引 |
| V2 | 新增 service_keys 表 + idx_service_keys_hash 索引 |
| V3 | models 新增 capabilities 列；清理旧 keys 数据 |
| V4 | usage_log 新增 service_key_id 列 + 索引 |
| V5 | 密钥状态纯内存化：api_keys 新增 last_error/last_error_code/last_error_time 列，status/last_error 不再读写 |
| V6 | 新增 settings 表 |
| V7 | usage_log 新增 cache_read_input_tokens 列；新增价格列（后被 V9 删除） |
| V8 | 价格单位 1K → MTok（后被 V9 删除） |
| V9 | 删除所有价格相关列（cost_per_mtok_input/output/cache_read/cache_write + cost_estimate） |
| V10 | 删除 cache_creation_input_tokens，合并到 prompt_tokens |
| V11 | 新增 plugins 表 + 索引 |
| V12 | usage_log 自包含：添加快照字段（provider_name/model_display_name/key_name/service_key_name/key_masked/service_key_masked），移除所有外键约束（重建表） |
| V13 | providers 新增 sort_order 列 |
| V14 | service_keys 新增 quota_5h / quota_7d 列（滚动窗口 token 配额，0 = 不设限） |
| V15 | 统一 provider kind 命名：`openai` → `chat_completions`、`anthropic` → `messages`、`responses` 保持不变 |
| V16 | WebSearch 劫持开关迁移为 MCP 模式：`websearch_hijack` → `mcp_websearch`（旧键保留兼容） |
| V17 | MCP 视觉工具设置键：`mcp_vision` / `mcp_vision_provider` / `mcp_vision_model` 默认行 |
| V18 | 新增 combos / combo_members 表 + 索引（组合别名：多个模型别名按顺序捆绑，路由时依次尝试直到可用） |

## 当前表结构（V18 最终状态）

### providers

```sql
CREATE TABLE providers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,               -- "messages" | "chat_completions" | "responses"
    base_url TEXT NOT NULL,
    api_path TEXT DEFAULT '/chat/completions',
    enabled INTEGER DEFAULT 1,
    config_json TEXT NOT NULL DEFAULT '{}',
    sort_order INTEGER NOT NULL DEFAULT 0,  -- V13: 拖拽排序
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```

### api_keys

```sql
CREATE TABLE api_keys (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    key_hash TEXT NOT NULL,           -- AES-256-GCM 加密（列名历史遗留，实际是密文）
    key_masked TEXT NOT NULL,         -- 脱敏显示（sk-xxxx...xxxx）
    status TEXT NOT NULL DEFAULT 'unknown',  -- V5 后不再被读写，保留兼容
    last_error TEXT,
    last_error_code INTEGER,
    last_error_time INTEGER,
    last_used_at INTEGER,
    balance REAL,
    balance_updated_at INTEGER,
    total_requests INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE
);
```

### models

```sql
CREATE TABLE models (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,           -- 真实模型名（发给上游）
    display_name TEXT NOT NULL,       -- 别名（暴露给客户端）
    tier TEXT NOT NULL DEFAULT 'custom',  -- fable/opus/sonnet/haiku/custom
    context_window INTEGER DEFAULT 128000,
    max_output_tokens INTEGER DEFAULT 4096,
    capabilities TEXT NOT NULL DEFAULT '["text"]',  -- V3 新增
    enabled INTEGER DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(provider_id, model_id),
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE
);
```

### service_keys

```sql
CREATE TABLE service_keys (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    key_hash TEXT NOT NULL,           -- Argon2 哈希
    key_masked TEXT NOT NULL,         -- 脱敏显示
    allowed_models TEXT NOT NULL DEFAULT '[]',  -- JSON 数组
    quota_5h INTEGER NOT NULL DEFAULT 0,  -- V14: 5h 滚动窗口 token 上限（0 = 不设限）
    quota_7d INTEGER NOT NULL DEFAULT 0,  -- V14: 7d 滚动窗口 token 上限（0 = 不设限）
    total_requests INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    last_used_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```

### routes

```sql
-- V1 创建，预留设计，当前未使用
CREATE TABLE routes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    model_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    priority INTEGER DEFAULT 100,
    weight REAL DEFAULT 1.0,
    enabled INTEGER DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (model_id) REFERENCES models(id) ON DELETE CASCADE,
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE
);
```

### combos / combo_members（V18：组合别名）

```sql
CREATE TABLE combos (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,          -- 组合别名（暴露给客户端），不得撞 models.display_name
    enabled INTEGER DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE combo_members (
    id TEXT PRIMARY KEY,
    combo_id TEXT NOT NULL,
    member_alias TEXT NOT NULL,         -- TEXT 软引用 models.display_name（display_name 非唯一，无法建硬 FK）
    position INTEGER NOT NULL DEFAULT 0, -- 尝试顺序（0 起）
    FOREIGN KEY (combo_id) REFERENCES combos(id) ON DELETE CASCADE,
    UNIQUE(combo_id, member_alias)
);

CREATE INDEX idx_combo_members_combo ON combo_members(combo_id);
```

**注意**: `member_alias` 是软引用——删除/禁用模型不影响组合结构，运行时跳过不可解析成员；`save_combo` 用事务 UPSERT 头 + DELETE 重插成员（不能用 INSERT OR REPLACE，会级联删成员）。

### usage_log

```sql
-- V12 重建：自包含快照，无外键约束
CREATE TABLE usage_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    provider_name TEXT NOT NULL DEFAULT '',
    model_id TEXT NOT NULL,
    model_display_name TEXT NOT NULL DEFAULT '',
    key_id TEXT,
    key_name TEXT NOT NULL DEFAULT '',
    key_masked TEXT NOT NULL DEFAULT '',
    service_key_id TEXT,
    service_key_name TEXT NOT NULL DEFAULT '',
    service_key_masked TEXT NOT NULL DEFAULT '',
    request_type TEXT NOT NULL,
    prompt_tokens INTEGER NOT NULL,
    completion_tokens INTEGER NOT NULL,
    latency_ms INTEGER NOT NULL,
    success INTEGER NOT NULL,
    error_message TEXT,
    cache_read_input_tokens INTEGER NOT NULL DEFAULT 0  -- V7: 缓存命中
);
```

**注意**: `usage_log` 无外键约束，所有名称字段在写入时快照。删除 Provider/Model/Key 不影响历史统计。

### settings

```sql
-- V6: 通用应用设置表
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

**用途**: `mcp_websearch` / `mcp_webfetch` / `failover_enabled` 开关 + 主题/色相/语言 + 密钥轮询指针（`keypool_index_{provider_id}`）。旧 `websearch_hijack` 键由 V16 迁移复制到 `mcp_websearch` 后保留（不再读取）。

### plugins

```sql
-- V11: 插件系统
CREATE TABLE plugins (
    id TEXT PRIMARY KEY,
    provider_id TEXT REFERENCES providers(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    last_heartbeat_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```

**注意**: `provider_id` 使用 `ON DELETE SET NULL`（非 CASCADE），删除 provider 时插件记录保留但 provider_id 置空。

### schema_version

```sql
CREATE TABLE schema_version (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
```

### 索引

```sql
-- V1
CREATE INDEX idx_models_provider ON models(provider_id);
CREATE INDEX idx_keys_provider ON api_keys(provider_id);
CREATE INDEX idx_keys_status ON api_keys(status);
CREATE INDEX idx_routes_model ON routes(model_id);
CREATE INDEX idx_routes_provider ON routes(provider_id);
CREATE INDEX idx_usage_timestamp ON usage_log(timestamp);
CREATE INDEX idx_usage_provider ON usage_log(provider_id);
-- V2
CREATE INDEX idx_service_keys_hash ON service_keys(key_hash);
-- V4
CREATE INDEX idx_usage_service_key ON usage_log(service_key_id);
-- V11
CREATE INDEX idx_plugins_provider ON plugins(provider_id);
CREATE INDEX idx_plugins_status ON plugins(status);
```

## 输入契约

### 添加新迁移

```rust
// db/schema.rs — 追加到 MIGRATIONS 数组末尾
pub const MIGRATIONS: &[&str] = &[
    // ... V1-V14 已有迁移 ...
    // V15 (新迁移)
    r#"ALTER TABLE providers ADD COLUMN ..."#,
];
```

### UPSERT 操作

```rust
// 必须使用 ON CONFLICT DO UPDATE，不能用 INSERT OR REPLACE
db.execute(
    "INSERT INTO providers (id, name, ...) VALUES (?1, ?2, ...)
     ON CONFLICT(id) DO UPDATE SET
       name = excluded.name,
       updated_at = excluded.updated_at",
    params![...],
)?;
```

## 关键约束

1. **顺序执行**: MIGRATIONS 按数组索引顺序执行，每个元素是一条完整 SQL
2. **幂等性**: V1 使用 `CREATE TABLE IF NOT EXISTS`，可重复执行
3. **事务安全**: 每个迁移在独立事务中执行
4. **UPSERT 语义**: 使用 `ON CONFLICT DO UPDATE`，避免 `INSERT OR REPLACE`（会触发 CASCADE DELETE）
5. **不可变历史**: 新增迁移只追加到数组末尾，**不要**修改已有迁移
6. **向后兼容**: 新迁移不能破坏旧版本代码

## 错误处理

| 场景 | 行为 |
|------|------|
| SQL 语法错误 | 启动失败，记录 error 日志 |
| 迁移执行失败 | 回滚当前事务，启动失败 |
| 版本号冲突 | 跳过已执行的迁移 |
| 数据库锁超时 | 重试 3 次，仍失败则启动失败 |

## 数据导出 / 导入 / 重置（2026-08 新增，`db/settings.rs`）

管理 API：`GET /api/data/export`、`POST /api/data/import`（body `{sql}`）、`POST /api/data/reset`（`api/handlers/data.rs`）。

- **`export_sql()`**: 覆盖 providers / models / api_keys / service_keys / plugins / usage_log / settings / combos / combo_members **九张表**，DROP + CREATE + INSERT，事务包裹，字符串转义单引号。**新增数据表时必须同步表清单**（combos 在 combo_members 前，FK 顺序）
- **`import_sql()`**: 直接 `execute_batch`（替换式导入，天然跨版本迁移）
- **`reset_all_data()`**: 按固定表序 DELETE（usage_log / plugins / service_keys / api_keys / models / combo_members / combos / providers / settings），保留 schema_version

## 实现位置

- `src-tauri/src/db/schema.rs` - MIGRATIONS 数组（所有 DDL 内联）
- `src-tauri/src/db/mod.rs` - 迁移执行逻辑 + Database 结构体
- `src-tauri/src/db/providers.rs` - Provider CRUD
- `src-tauri/src/db/models.rs` - Model CRUD
- `src-tauri/src/db/api_keys.rs` - API Key CRUD
- `src-tauri/src/db/service_keys.rs` - Service Key CRUD
- `src-tauri/src/db/usage.rs` - Usage Log 查询 + 请求日志分页
- `src-tauri/src/db/settings.rs` - Settings CRUD + 导出/导入/重置
- `src-tauri/src/db/combos.rs` - Combo CRUD（V18）

## 测试要求

1. **单元测试**: 从空库执行 migrate() 到最新版本
2. **回归测试**: UPSERT 不触发 CASCADE DELETE（`db/mod.rs` 有 `test_save_does_not_cascade_delete_children`）
3. **性能测试**: 大量数据插入和查询的性能

## 完成标准

- [x] 18 版增量迁移（V1→V18）
- [x] 迁移按序执行，跳过已应用的版本
- [x] UPSERT 使用 `ON CONFLICT DO UPDATE`
- [x] `usage_log` 自包含快照（无外键）
- [x] `settings` 表支持运行时配置（failover_enabled / locale / 轮询指针）
- [x] 数据导出/导入/重置（`export_sql` / `import_sql` / `reset_all_data`，表清单含 combos / combo_members）
- [x] 通过所有单元测试
