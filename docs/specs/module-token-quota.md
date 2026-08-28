# Spec: Token 配额（5h/7d 滚动窗口）

## 目标

按 Service Key 限制用量：每个客户端密钥可设 5 小时 / 7 天两个滚动窗口的 token 上限，
任一窗口超限即在代理入口返回 429；同时向客户端暴露余额查询端点（`/v1/user/balance`），
供 Claude Code 等客户端展示剩余用量。对应 PRD §6「Token 配额规格」。

## 适用范围

- 检查发生在代理认证链（`api/proxy/quota.rs::check_quota`）：Service Key 验证通过后、
  路由/转发之前（见 module-auth 认证流程步骤 5）。
- `/v1/messages`、`/v1/chat/completions`、`/v1/responses` 均受控；
  `/v1/models`、`/v1/user/balance` 不计入也不受配额拦截。

## 输入契约

- **配额上限**：随 Service Key 创建/编辑设置（`service_keys.quota_5h` / `quota_7d`，
  单位 token；`0` 表示该窗口不设限）。上限是唯一持久化的配额数据。
- **已用量**：不维护计数器，检查时按需从 `usage_log` 聚合
  （`db::usage::get_service_key_usage`，窗口常量 5×3600 / 7×86400 秒）。

## 输出契约

### 超限响应（429）

- 头：`retry-after`（秒）。
- 体：`quota_error` JSON，指明超限窗口（`5h`/`7d`）与预计重置时间。
- 两窗口同时超限时取 7d（重置更晚，客户端重试参考更保守）。
- `resets_in` 读数格式 `XdYh` / `XhYm` / `Ym`（近似，最小 1m）。

### `GET /v1/user/balance`

- 与 `/v1/*` 相同鉴权（Bearer Service Key）。
- 返回各窗口已用/上限/剩余与预计重置时间，供客户端余额显示。

## 关键约束

| 约束 | 原因 |
|------|------|
| 只持久化上限，用量实时聚合 | 避免双写计数器与 usage_log 漂移 |
| 窗口口径与统计一致 | 聚合查询与 StatsView 共用同一窗口常量 |
| 检查先于上游调用 | 超限请求不消耗任何上游 key |
| 限流与配额分层 | 令牌桶限流（module-proxy-handler）管突发，配额管总量 |

## 错误处理

| 情况 | 行为 |
|------|------|
| 聚合查询失败 | `unwrap_or((0, 0))` 放行（不因统计故障拒绝服务） |
| 超限 | 429 + `retry-after` + `quota_error`（不进入转发） |

## 实现位置

- `src-tauri/src/api/proxy/quota.rs`（检查 + 余额端点）
- `src-tauri/src/db/usage.rs`（窗口聚合）
- 前端创建密钥时设置上限：`src/views/KeysView.tsx`

## 完成标准

- [x] 5h/7d 双窗口检查，超限 429 带 `retry-after` 与 `quota_error`
- [x] `quota=0` 不限该窗口
- [x] `/v1/user/balance` 返回可读余额
- [x] 窗口常量与用量统计口径一致
