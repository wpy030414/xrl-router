# Spec: LLM 代理处理器

## 目标

实现 `/v1/messages` 和 `/v1/chat/completions` 的代理转发，支持密钥轮换、协议转换、流式响应。

## 输入契约

### POST /v1/messages

```json
{
  "model": "claude-opus-4-8",
  "messages": [{"role": "user", "content": "Hello"}],
  "max_tokens": 4096,
  "stream": true
}
```

**必需头**:
- `x-api-key: sk-xxx` 或 `Authorization: Bearer sk-xxx`
- `Content-Type: application/json`

### POST /v1/chat/completions

```json
{
  "model": "gpt-4o",
  "messages": [{"role": "user", "content": "Hello"}],
  "stream": true
}
```

## 输出契约

### 成功响应（流式）

**Content-Type**: `text/event-stream`

**Anthropic 格式**:
```
data: {"type":"message_start","message":{"id":"msg_xxx",...}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

data: {"type":"message_stop"}
```

**OpenAI 格式**:
```
data: {"id":"chatcmpl-xxx","choices":[{"delta":{"content":"Hello"}}]}

data: [DONE]
```

### 错误响应

```json
{
  "error": {
    "type": "authentication_error",
    "message": "Invalid API key"
  }
}
```

**配额超限**（滚动窗口触顶）：

```json
{
  "error": {
    "type": "quota_error",
    "message": "Quota exceeded for this key (5h window). Resets in 2h31m."
  }
}
```

**状态码**:
- `400` 请求格式错误（含模型不存在 / 无路由候选）
- `401` API key 无效
- `403` 模型不在白名单
- `429` 速率限制，或 5h/7d 滚动窗口配额超限（`quota_error`，携带 `retry-after` 头）
- `500` 内部错误
- `502` 上游网络错误（全部候选失败）
- `503` 无可用密钥
- `504` 响应头超时（全部候选失败）

## 故障转移（Provider Failover）

`failover_enabled` 开关（设置页「路由」Tab，默认关闭）开启时，同一 `display_name` 下的全部 Provider 候选按序尝试：

- **候选来源**：`route.rs::resolve_route_candidates()`——同 display_name 全部 models JOIN providers 行，按 `sort_order ASC, created_at ASC` 排序、按 provider_id 去重、跳过插件离线的委托 provider；关闭时仅主 provider（`resolve_route`，行为与单 Provider 一致）
- **双层循环**：外层 provider 候选（冷却中直接跳过），内层 key 轮换；key 级 4xx 先耗尽当前 provider 的 key 才切 provider
- **冷却**：`failover.rs` 纯内存冷却表，provider 级失败（5xx/网络错误/响应头超时）标记 60s 冷却，2xx 成功立即清除
- **请求体**：Anthropic/OpenAI 两种骨架循环外预构造，循环内按候选类型选用并覆写 `model`（候选可混合协议类型）

## 关键约束

1. **强制 stream=true**: 即使客户端发送 `stream=false`，也会被静默覆写为 `true` 后继续处理（不返回 400）
2. **模型替换**: 将 `display_name` 替换为上游的 `model_id`
3. **配额检查**: 认证后先查 5h/7d 滚动窗口配额（`quota.rs::check_quota`），任一窗口触顶返回 429（`quota_error` + `retry-after`，message 含重置时间）
4. **密钥轮换**: 401/403 标红，402/429 标黄，自动切换下一个 key
5. **超时控制**: 连接 10s，响应头自适应（`header_timeout_for()`：≥100k token → 600s、≥50k → 480s、基准 300s），流间隔 120s。请求体上限 64MiB（`MAX_REQUEST_BODY_BYTES`）。
6. **重试边界**: 内层最多重试当前 provider 的 `key_count` 次；外层候选数由 `resolve_route_candidates` 决定，开关关闭时仅 1 个候选

## 错误处理

| 场景 | 行为 |
|------|------|
| API key 无效 | 返回 401，不重试 |
| 模型不存在 / 无路由候选 | 返回 400，不重试 |
| 配额超限 | 返回 429 `quota_error` + `retry-after`，不重试 |
| 上游 401/403 | 标红当前 key，切换下一个（内层），重试 |
| 上游 402/429 | 标黄当前 key，切换下一个（内层），重试 |
| 上游 5xx | 有后续候选 → 标记 provider 冷却（60s）+ 切 provider（外层）重试；无后续候选 → 透传上游失败响应 |
| 网络错误 | 有后续候选 → 切 provider；无后续候选 → 返回 502 |
| 响应头超时 | 有后续候选 → 切 provider；无后续候选 → 返回 504 |
| 全部 key 4xx 耗尽 | 透传最后一次上游失败响应 |
| 无可用 key | 返回 503 |

## 实现位置

- `src-tauri/src/api/proxy/handler.rs` - 薄入口层（认证 + 请求体准备）
- `src-tauri/src/api/proxy/stream.rs` - 流式引擎核心（路由解析 → 立即返回 Response → 后台 spawn 双循环）
- `src-tauri/src/api/proxy/forward.rs` - 流式转发分支（passthrough / O→A / A→O）
- `src-tauri/src/api/proxy/auth.rs` - 认证
- `src-tauri/src/api/proxy/quota.rs` - 配额检查
- `src-tauri/src/api/proxy/route.rs` - 路由解析
- `src-tauri/src/api/proxy/key_rotation.rs` - 密钥轮换
- `src-tauri/src/api/proxy/failover.rs` - 故障转移冷却表
- `src-tauri/src/api/proxy/translate/` - 协议转换

## 测试要求

1. **单元测试**: 认证、路由解析、密钥轮换逻辑
2. **集成测试**: 模拟上游 API，测试完整流程
3. **边界测试**: 所有 key 都 Red、上游超时、协议转换错误

## 完成标准

- [x] 支持 `/v1/messages` 和 `/v1/chat/completions`
- [x] 强制流式响应
- [x] 5h/7d 滚动窗口配额检查（429 `quota_error` + `retry-after`）
- [x] 密钥轮换（Red/Yellow/Green）
- [x] 协议转换（Anthropic ↔ OpenAI）
- [x] 超时控制
- [x] 错误处理
- [x] 记录 `usage_log`
- [x] failover（`resolve_route_candidates` + 双层循环 + 60s 冷却 + 开关）
- [x] 通过网关冒烟测试（含 failover 双假上游 E2E）
