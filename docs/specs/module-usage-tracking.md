# Spec: 用量统计

## 目标

记录每次 LLM 请求的用量信息，提供聚合统计查询，支持前端图表展示。

## Usage 语义

### 真实值覆盖估算占位

`forward.rs` 在流式转发开始时预填 `chars/4` 估算值（偏大），上游返回真实 usage 时**直接覆盖**（不用 `max()`）：

```rust
// from_chat_completions.rs / from_responses.rs
if usage.input_tokens > 0 {
    state.usage.input_tokens = usage.input_tokens;  // 覆盖，不用 max()
}
if usage.output_tokens > 0 {
    state.usage.output_tokens = usage.output_tokens;
}
if usage.cache_read_input_tokens > 0 {
    state.usage.cache_read_input_tokens = usage.cache_read_input_tokens;
}
```

**为什么不用 max()**：`chars/4` 估算偏保守（中文/代码实际 token 通常低于估算），`max()` 会让估算值永久压住真实值，污染 usage_log 与客户端上下文条。

### Responses 增量口径

Responses API 的 `input_tokens` 包含缓存命中部分，需减去以保持增量口径：

```rust
// usage.rs (extract_responses_usage)
let cached = usage.input_tokens_details.cached_tokens.unwrap_or(0);
usage.input_tokens = usage.input_tokens.saturating_sub(cached);
```

与 Chat Completions 的 `prompt_tokens`（已减去 `cached_tokens`）语义一致。

### message_start 携带 usage

IR → Messages 渲染时：
- `message_start.usage` 携带 `input_tokens` / `cache_read_input_tokens`（上游真实值或估算占位）
- `message_delta.usage` 补上 `input_tokens`（此前缺失）

### 上下文超限预警

`stream.rs` 检测到上下文超限时仅记录 warn 日志，**不返回 400**——避免阻断客户端 auto-compact（`/compact` 自身也需走代理，硬拒绝会形成死锁）。

## 数据结构

### usage_log 表

```sql
CREATE TABLE usage_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    model_id TEXT NOT NULL,
    model_display_name TEXT NOT NULL,
    key_id TEXT,
    key_name TEXT,
    key_masked TEXT,
    service_key_id TEXT,
    service_key_name TEXT,
    service_key_masked TEXT,
    request_type TEXT NOT NULL,
    prompt_tokens INTEGER NOT NULL,
    completion_tokens INTEGER NOT NULL,
    cache_read_input_tokens INTEGER NOT NULL DEFAULT 0,
    latency_ms INTEGER NOT NULL,
    success INTEGER NOT NULL,
    error_message TEXT
);
```

**设计要点**:
- **自包含快照**: 写入时保存名称，不依赖外键
- **删除安全**: 删除 Provider/Model/Key 不影响历史统计
- **缓存追踪**: `cache_read_input_tokens` 记录缓存命中
- **真实值**: `prompt_tokens` 为上游真实值（覆盖估算占位后）

## 输入契约

### 记录用量

```rust
pub fn insert_usage_log(
    timestamp: i64,
    provider_id: &str,
    provider_name: &str,
    model_id: &str,
    model_display_name: &str,
    key_id: Option<&str>,
    key_name: &str,
    key_masked: &str,
    service_key_id: Option<&str>,
    service_key_name: &str,
    service_key_masked: &str,
    request_type: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_read_input_tokens: i64,
    latency_ms: i64,
    success: bool,
    error_message: Option<&str>,
) -> anyhow::Result<()>
```

### 查询统计

```rust
pub fn get_usage_by_day_and_key(
    from_ts: i64,
    to_ts: i64,
    bucket_seconds: i64,  // 3600 (hour) | 86400 (day)
    tz_offset: i32,
) -> anyhow::Result<Vec<serde_json::Value>>
```

### 请求日志分页（2026-08 新增）

```rust
pub fn get_usage_log_page(page: i64, page_size: i64) -> anyhow::Result<PagedRows>
```

- **排序**: `timestamp DESC, id DESC`（同秒按 id 逆序，保证稳定分页）
- **分页**: page 默认 1，page_size 默认 10、clamp 1–100；`COUNT(*)` 总数 + 当页行
- **行字段**: `provider_name` / `model_display_name` / `service_key_name` / `key_masked` / `prompt_tokens` / `completion_tokens` / `latency_ms` / `success` / `error_message`
- **HTTP 端点**: `GET /api/stats/requests?page=N&page_size=M` → `{total, page, page_size, data}`（`api/handlers/stats.rs::get_stats_requests`）

## 输出契约

统计查询返回 `Vec<serde_json::Value>`（使用 `json!({})` 宏构建，无强类型结构体）。

### 统计维度

**按 Service Key + 时间桶分组**:
```sql
SELECT
    service_key_id,
    service_key_name,
    SUM(prompt_tokens) as prompt_tokens,
    SUM(completion_tokens) as completion_tokens,
    SUM(cache_read_input_tokens) as cache_read_input_tokens,
    SUM(prompt_tokens + completion_tokens) as total_tokens,
    COUNT(*) as requests,
    CAST((timestamp + ?) / ? AS INTEGER) as bucket
FROM usage_log
WHERE timestamp >= ? AND timestamp < ?
GROUP BY service_key_id, bucket
ORDER BY bucket ASC
```

**时间桶格式**: `"h{bucket}"` 或 `"d{bucket}"`，其中 bucket = `floor((timestamp + tz_offset) / bucket_seconds)`

**Top Model**:
```sql
SELECT
    model_id,
    model_display_name as model_name,
    COUNT(*) as requests
FROM usage_log
WHERE timestamp >= ? AND timestamp < ?
GROUP BY model_id
ORDER BY requests DESC
LIMIT 1
```

## 关键约束

1. **写入性能**: 每次请求都写一条记录，需要高效插入
2. **查询性能**: 大量历史数据时，聚合查询需要索引
3. **时区处理**: 使用 `tz_offset` 参数调整时区
4. **粒度支持**: `hour` 和 `day` 两种粒度
5. **无外键**: 删除 Provider/Model/Key 不影响统计
6. **真实值覆盖**: `prompt_tokens` 记录上游真实值，不记录偏大的 `chars/4` 估算值

## 索引

```sql
CREATE INDEX idx_usage_timestamp ON usage_log(timestamp);
CREATE INDEX idx_usage_provider ON usage_log(provider_id);
CREATE INDEX idx_usage_service_key ON usage_log(service_key_id);
```

## 错误处理

| 场景 | 行为 |
|------|------|
| 插入失败 | 记录 warn 日志，不影响请求响应 |
| 查询失败 | 返回空结果 |
| 无数据 | 返回空数组，`top_model` 为 null |

## 实现位置

- `src-tauri/src/db/usage.rs` - 插入和查询逻辑
- `src-tauri/src/api/handlers/stats.rs` - HTTP API 处理
- `src-tauri/src/api/proxy/stream.rs` - 异步记录用量（真实值覆盖后）
- `src-tauri/src/api/proxy/ir/usage.rs` - usage 提取 + 增量口径

## 测试要求

1. **单元测试**: 插入逻辑、查询逻辑
2. **集成测试**: 完整统计流程（写入 + 查询）
3. **性能测试**: 大量数据插入和查询的性能
4. **边界测试**: 空数据、时区边界、粒度切换
5. **usage 语义测试**: 真实值覆盖估算、Responses 增量口径、无缓存/全缓存场景

## 完成标准

- [x] 每次请求记录 `usage_log`
- [x] 按 Service Key 分组统计
- [x] 按小时/天粒度聚合
- [x] Top Model 统计
- [x] 时区偏移支持
- [x] 索引优化查询性能
- [x] 异步写入（不影响请求延迟）
- [x] 请求日志分页（`get_usage_log_page` + `GET /api/stats/requests`，含分页单元测试）
- [x] usage 真实值覆盖估算占位（不用 max）
- [x] Responses input_tokens 增量口径（减去 cached_tokens）
- [x] message_delta 补全 input_tokens
- [x] 上下文超限预警（warn 日志，不阻断）
- [x] 通过所有单元测试
