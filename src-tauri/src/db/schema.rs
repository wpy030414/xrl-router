/// Database schema migrations.
/// Each element in the array is a complete migration SQL statement.
pub const MIGRATIONS: &[&str] = &[
    // V1: Initial schema
    r#"
CREATE TABLE IF NOT EXISTS providers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_path TEXT DEFAULT '/chat/completions',
    enabled INTEGER DEFAULT 1,
    config_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS models (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    tier TEXT NOT NULL DEFAULT 'custom',
    context_window INTEGER DEFAULT 128000,
    max_output_tokens INTEGER DEFAULT 4096,
    cost_per_1k_input REAL DEFAULT 0.0,
    cost_per_1k_output REAL DEFAULT 0.0,
    enabled INTEGER DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE,
    UNIQUE(provider_id, model_id)
);

CREATE TABLE IF NOT EXISTS api_keys (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    key_hash TEXT NOT NULL,
    key_masked TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'unknown',
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

CREATE TABLE IF NOT EXISTS routes (
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

CREATE TABLE IF NOT EXISTS usage_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    key_id TEXT,
    request_type TEXT NOT NULL,
    prompt_tokens INTEGER NOT NULL,
    completion_tokens INTEGER NOT NULL,
    latency_ms INTEGER NOT NULL,
    success INTEGER NOT NULL,
    error_message TEXT,
    cost_estimate REAL,
    FOREIGN KEY (provider_id) REFERENCES providers(id),
    FOREIGN KEY (model_id) REFERENCES models(id),
    FOREIGN KEY (key_id) REFERENCES api_keys(id)
);

CREATE INDEX IF NOT EXISTS idx_models_provider ON models(provider_id);
CREATE INDEX IF NOT EXISTS idx_keys_provider ON api_keys(provider_id);
CREATE INDEX IF NOT EXISTS idx_keys_status ON api_keys(status);
CREATE INDEX IF NOT EXISTS idx_routes_model ON routes(model_id);
CREATE INDEX IF NOT EXISTS idx_routes_provider ON routes(provider_id);
CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_log(timestamp);
CREATE INDEX IF NOT EXISTS idx_usage_provider ON usage_log(provider_id);

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
"#,
    // V2: Provider config (endpoint_type, key/model sources) + service_keys
    r#"
CREATE TABLE IF NOT EXISTS service_keys (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    key_hash TEXT NOT NULL,
    key_masked TEXT NOT NULL,
    allowed_models TEXT NOT NULL DEFAULT '[]',
    total_requests INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    last_used_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_service_keys_hash ON service_keys(key_hash);
"#,
    // V3: models.capabilities + clear legacy keys (encryption/hashing format change) + drop custom/deap providers
    r#"
ALTER TABLE models ADD COLUMN capabilities TEXT NOT NULL DEFAULT '["text"]';
DELETE FROM api_keys;
DELETE FROM service_keys;
DELETE FROM providers WHERE kind IN ('custom', 'deap');
"#,
    // V4: usage_log.service_key_id — record which service key (client-facing)
    // made each request, so stats can group by service key instead of the
    // internal round-robin provider key.
    r#"
ALTER TABLE usage_log ADD COLUMN service_key_id TEXT REFERENCES service_keys(id);
CREATE INDEX IF NOT EXISTS idx_usage_service_key ON usage_log(service_key_id);
"#,
    // V5: 可用性不再持久化到 DB —— 清掉之前持久化时代留下的 status/last_error 残留。
    // 运行时状态纯内存（启动全 green），DB 的 status 列从此不再被读写。
    r#"
UPDATE api_keys SET status = 'green', last_error = NULL, last_error_code = NULL, last_error_time = NULL;
"#,
    // V6: 通用应用设置表（key-value），如 websearch_hijack 开关。
    r#"
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#,
    // V7: 缓存 token 追踪 + 缓存价格字段。
    // Anthropic prompt caching 会产生 cache_read_input_tokens 和
    // cache_creation_input_tokens，之前这两类 token 完全没被记录，
    // 导致应用内统计与 CCSwitch 等外部工具严重不对齐。
    // models 表新增 cache 价格列（Anthropic: cache write = 1.25× input,
    // cache read = 0.1× input），用于准确的费用估算。
    r#"
ALTER TABLE usage_log ADD COLUMN cache_read_input_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usage_log ADD COLUMN cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE models ADD COLUMN cost_per_1k_cache_read REAL DEFAULT 0.0;
ALTER TABLE models ADD COLUMN cost_per_1k_cache_write REAL DEFAULT 0.0;
"#,
    // V8: 价格单位从 per 1K token 改为 per MTok（每百万 token），
    // 与 Anthropic / OpenAI 官方定价一致。
    r#"
ALTER TABLE models RENAME COLUMN cost_per_1k_input TO cost_per_mtok_input;
ALTER TABLE models RENAME COLUMN cost_per_1k_output TO cost_per_mtok_output;
ALTER TABLE models RENAME COLUMN cost_per_1k_cache_read TO cost_per_mtok_cache_read;
ALTER TABLE models RENAME COLUMN cost_per_1k_cache_write TO cost_per_mtok_cache_write;
UPDATE models SET cost_per_mtok_input = cost_per_mtok_input * 1000.0;
UPDATE models SET cost_per_mtok_output = cost_per_mtok_output * 1000.0;
UPDATE models SET cost_per_mtok_cache_read = cost_per_mtok_cache_read * 1000.0;
UPDATE models SET cost_per_mtok_cache_write = cost_per_mtok_cache_write * 1000.0;
"#,
    // V9: 移除无用的模型价格列和 usage_log.cost_estimate。
    // 历史上价格相关字段从未被 UI 展示或使用，属于死代码；清理掉以简化 schema。
    r#"
ALTER TABLE models DROP COLUMN cost_per_mtok_input;
ALTER TABLE models DROP COLUMN cost_per_mtok_output;
ALTER TABLE models DROP COLUMN cost_per_mtok_cache_read;
ALTER TABLE models DROP COLUMN cost_per_mtok_cache_write;
ALTER TABLE usage_log DROP COLUMN cost_estimate;
"#,
    // V10: 概念纠正——缓存只有「读」（命中复用），「写缓存」只是首次处理输入，
    // 属于输入的一部分，不该单列。把历史的 cache_creation 并入 prompt_tokens
    // 后删除该列；之后 input_tokens（含写缓存）+ cache_read 即完整口径。
    r#"
UPDATE usage_log SET prompt_tokens = prompt_tokens + COALESCE(cache_creation_input_tokens, 0)
    WHERE cache_creation_input_tokens IS NOT NULL AND cache_creation_input_tokens > 0;
ALTER TABLE usage_log DROP COLUMN cache_creation_input_tokens;
"#,
    // V11: Plugin system — tracks registered plugins and their associated providers.
    // Each plugin (e.g., xrl-router-plugin-wukong) connects via WebSocket and acts as
    // a "delegated provider": the plugin handles protocol translation + DEAP header injection,
    // while Router manages key rotation and request routing.
    r#"
CREATE TABLE IF NOT EXISTS plugins (
    id TEXT PRIMARY KEY,
    provider_id TEXT REFERENCES providers(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    last_heartbeat_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plugins_provider ON plugins(provider_id);
CREATE INDEX IF NOT EXISTS idx_plugins_status ON plugins(status);
"#,
    // V12: usage_log 自包含 — 统计不再依赖外键。
    // 写入时快照 provider_name / model_display_name / key_name / key_masked /
    // service_key_name / service_key_masked，统计查询不再 JOIN 父表。
    // 同时重建表以去除 FK 约束，确保删除模型/密钥/provider 不影响历史统计。
    r#"
ALTER TABLE usage_log ADD COLUMN provider_name TEXT NOT NULL DEFAULT '';
ALTER TABLE usage_log ADD COLUMN model_display_name TEXT NOT NULL DEFAULT '';
ALTER TABLE usage_log ADD COLUMN key_name TEXT NOT NULL DEFAULT '';
ALTER TABLE usage_log ADD COLUMN key_masked TEXT NOT NULL DEFAULT '';
ALTER TABLE usage_log ADD COLUMN service_key_name TEXT NOT NULL DEFAULT '';
ALTER TABLE usage_log ADD COLUMN service_key_masked TEXT NOT NULL DEFAULT '';

UPDATE usage_log SET provider_name = COALESCE((SELECT p.name FROM providers p WHERE p.id = usage_log.provider_id), '');
UPDATE usage_log SET model_display_name = COALESCE((SELECT m.display_name FROM models m WHERE m.id = usage_log.model_id), '');
UPDATE usage_log SET key_name = COALESCE((SELECT k.name FROM api_keys k WHERE k.id = usage_log.key_id), '');
UPDATE usage_log SET key_masked = COALESCE((SELECT k.key_masked FROM api_keys k WHERE k.id = usage_log.key_id), '');
UPDATE usage_log SET service_key_name = COALESCE((SELECT s.name FROM service_keys s WHERE s.id = usage_log.service_key_id), '');
UPDATE usage_log SET service_key_masked = COALESCE((SELECT s.key_masked FROM service_keys s WHERE s.id = usage_log.service_key_id), '');

-- 重建 usage_log：去掉所有 FK 约束，保留索引。
CREATE TABLE usage_log_new (
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
    cache_read_input_tokens INTEGER NOT NULL DEFAULT 0
);

INSERT INTO usage_log_new
    (id, timestamp, provider_id, provider_name, model_id, model_display_name,
     key_id, key_name, key_masked, service_key_id, service_key_name, service_key_masked,
     request_type, prompt_tokens, completion_tokens, latency_ms, success, error_message,
     cache_read_input_tokens)
SELECT id, timestamp, provider_id, provider_name, model_id, model_display_name,
       key_id, key_name, key_masked, service_key_id, service_key_name, service_key_masked,
       request_type, prompt_tokens, completion_tokens, latency_ms, success, error_message,
       cache_read_input_tokens
FROM usage_log;

DROP TABLE usage_log;
ALTER TABLE usage_log_new RENAME TO usage_log;

CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON usage_log(timestamp);
CREATE INDEX IF NOT EXISTS idx_usage_provider ON usage_log(provider_id);
CREATE INDEX IF NOT EXISTS idx_usage_service_key ON usage_log(service_key_id);
"#,
    // V13: providers.sort_order — 供应商手动排序（拖拽）。数值越小优先级越高；
    // 模型撞名时 resolve_route 优先取 sort_order 更小的供应商。历史行默认 0，
    // 新创建行由 handler 分配 max+1。CREATE TABLE IF NOT EXISTS 不会补列，
    // 必须用 ALTER TABLE。
    r#"
ALTER TABLE providers ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
"#,
    // V14: service_keys 滚动窗口 token 配额（5h / 7d）。0 表示不设限。
    // 用量从 usage_log 条件聚合得出（见 usage::get_service_key_usage），
    // 不在本表持久化，避免写路径额外同步计数。
    r#"
ALTER TABLE service_keys ADD COLUMN quota_5h INTEGER NOT NULL DEFAULT 0;
ALTER TABLE service_keys ADD COLUMN quota_7d INTEGER NOT NULL DEFAULT 0;
"#,
    // V15: 统一 provider kind 命名规范。
    // openai → chat_completions
    // anthropic → messages
    // responses 保持不变
    r#"
UPDATE providers SET kind = 'chat_completions' WHERE kind = 'openai';
UPDATE providers SET kind = 'messages' WHERE kind = 'anthropic';
"#,
];
