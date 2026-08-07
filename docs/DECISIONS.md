# Architecture Decision Records

设计背后的历史原因。防止架构漂移。

---

## ADR-001: 选择 Tauri 2 作为桌面框架

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

需要一个轻量级桌面应用来运行本地 LLM API 网关。考虑过 Electron、原生应用、纯 CLI。

### 决策

采用 Tauri 2：Rust 后端 + WebView 前端。

### 原因

1. **轻量**: 安装包 < 10MB，内存占用 < 100MB（Electron 通常 > 200MB）
2. **性能**: Rust 后端处理高并发代理请求，比 Node.js/Python 更高效
3. **安全**: Rust 内存安全 + 系统级加密库原生支持
4. **跨平台**: 一套代码编译 macOS/Windows/Linux
5. **现代化**: 前端用 Vue 3 + Material Web，用户体验好

### 代价

- 需要 Rust 工具链（学习曲线）
- WebView 渲染在某些系统可能不一致
- 调试比 Electron 复杂

---

## ADR-002: 仅支持流式代理（stream=true）

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

LLM API 支持流式（SSE）和非流式两种模式。完整支持需要两套代码路径。

### 决策

强制 `stream=true`。即使客户端发送 `stream=false`，也会被静默覆写为 `true` 后继续处理（不返回 400）。

### 原因

1. **简化实现**: 只需一套流式处理逻辑，代码量减少 40%
2. **用户体验**: Claude Code、ChatGPT 等主流客户端都默认流式，响应更快
3. **资源效率**: 流式可以边生成边传输，不需要缓存完整响应
4. **协议转换**: 流式转换可以逐 chunk 处理，内存占用低

### 代价

- 无法支持需要完整响应的场景（如某些 batch 处理）
- 客户端必须支持 SSE

### 替代方案

如果未来需要非流式，可以新增 `/v1/messages/sync` 端点，不影响现有流式逻辑。

---

## ADR-003: 密钥健康状态纯内存存储

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

密钥健康状态（green/yellow/red）需要持久化还是仅内存？

### 决策

健康状态仅存内存，启动时全部初始化为 green。只有轮询指针（`current_index`）持久化到 `settings` 表。

### 原因

1. **启动恢复**: 重启后从上次轮询位置继续，跳过已失效的 key
2. **减少 IO**: 每次健康状态变更都写 DB 会产生大量小事务
3. **语义合理**: 健康状态是运行时概念，重启后重新探测更合理
4. **指针持久化**: 避免每次都从 key[0] 开始轮询，提升效率

### 代价

- 重启后无法看到历史健康状态
- 需要用户手动观察哪些 key 失效

### 实现

```rust
// keys/pool/persistence.rs
pub fn persist_index(&self, provider_id: &str, index: usize) {
    let key = format!("keypool_index_{}", provider_id);
    settings::set(&key, &index.to_string())?;
}

pub fn load_persisted_index(&self, provider_id: &str) -> usize {
    let key = format!("keypool_index_{}", provider_id);
    settings::get(&key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}
```

---

## ADR-004: usage_log 自包含快照设计

**日期**: 2026-08-01  
**状态**: 已接受

### 背景

`usage_log` 表需要关联 `providers`、`models`、`api_keys`、`service_keys`。是否用外键？

### 决策

`usage_log` 不使用外键，而是存储快照字段：`provider_name`、`model_display_name`、`key_name`、`service_key_name` 等。

### 原因

1. **历史完整性**: 删除 Provider/Model/Key 后，历史统计仍然可见
2. **查询性能**: 统计查询不需要 JOIN 多张表
3. **数据独立**: 即使上游表结构变化，历史记录不受影响
4. **迁移安全**: V12 迁移可以安全地删除外键约束

### 代价

- 数据冗余（每条日志多存 ~100 字节）
- 无法通过外键约束保证一致性

### 实现

```sql
-- V12: usage_log 自包含
ALTER TABLE usage_log ADD COLUMN provider_name TEXT DEFAULT '';
ALTER TABLE usage_log ADD COLUMN model_display_name TEXT DEFAULT '';
ALTER TABLE usage_log ADD COLUMN key_name TEXT DEFAULT '';
ALTER TABLE usage_log ADD COLUMN service_key_name TEXT DEFAULT '';

-- 回填历史数据
UPDATE usage_log SET provider_name = (
  SELECT name FROM providers WHERE id = usage_log.provider_id
);
-- ... 其他字段类似

-- 删除外键约束（重建表）
```

---

## ADR-005: 管理 API 无认证，绑定 127.0.0.1

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

管理 API（`/api/providers`、`/api/keys` 等）是否需要认证？

### 决策

管理 API 无认证，仅通过绑定 `127.0.0.1` + CORS 白名单保护。

### 原因

1. **本地场景**: 桌面应用运行在用户本机，威胁模型是"本机其他进程"
2. **简化使用**: 无需登录/Token，打开应用即用
3. **Tauri 隔离**: WebView 与后端同进程，不需要跨域认证
4. **CORS 保护**: 浏览器端恶意网页无法跨域调用

### 代价

- 本机恶意进程可以访问管理 API
- 不适合多用户共享场景

### 威胁模型

- **已防护**: 远程攻击（绑定 127.0.0.1）、浏览器跨域攻击（CORS）
- **未防护**: 本机恶意进程读取密钥、修改配置
- **接受风险**: 桌面应用场景，用户应保证本机安全

### 未来改进

如需多用户或远程管理，可新增 `/api/auth/login` + JWT Token，管理 API 加 `Authorization` 头校验。

---

## ADR-006: 删除价格字段（V9 迁移）

**日期**: 2026-07-29  
**状态**: 已接受

### 背景

V7 添加了 `cost_per_mtok_input`、`cost_per_mtok_output` 等价格字段，但前端从未展示。

### 决策

V9 迁移删除所有价格相关字段：
- `models.cost_per_mtok_input`
- `models.cost_per_mtok_output`
- `models.cost_per_mtok_cache_read`
- `models.cost_per_mtok_cache_write`
- `usage_log.cost_estimate`

### 原因

1. **未使用**: 前端从未读取或展示价格数据
2. **复杂度**: 价格计算需要考虑缓存、不同供应商定价策略
3. **维护成本**: 需要定期更新价格表
4. **简化 schema**: 减少不必要的字段

### 代价

- 未来如需成本统计，需要重新添加字段 + 迁移
- 用户无法在本地查看 API 调用成本

### 替代方案

如需成本统计，可以：
1. 导出 `usage_log` 到 CSV，用 Excel 计算
2. 新增独立的价格表（不嵌入 models 表）

---

## ADR-007: 缓存概念纠正（V10 迁移）

**日期**: 2026-07-30  
**状态**: 已接受

### 背景

V7 引入了 `cache_creation_input_tokens` 和 `cache_read_input_tokens` 两个字段。但"写缓存"本质上是首次处理的输入，不应单独计数。

### 决策

V10 迁移：
- 删除 `cache_creation_input_tokens`
- 将历史数据合并到 `prompt_tokens`
- 只保留 `cache_read_input_tokens`（真正的缓存命中）

### 原因

1. **概念清晰**: "写缓存"只是首次处理输入，本质是输入 token
2. **简化统计**: 总输入 = `prompt_tokens`（含写缓存）+ `cache_read_input_tokens`（缓存命中）
3. **对齐上游**: OpenAI 的 `prompt_tokens` 已包含所有输入（含写缓存）

### 代价

- 历史数据需要迁移（`prompt_tokens = prompt_tokens + cache_creation`）
- 无法区分"首次处理的输入"和"缓存命中的输入"

### 实现

```sql
-- V10: 缓存概念纠正
UPDATE usage_log
SET prompt_tokens = prompt_tokens + cache_creation_input_tokens
WHERE cache_creation_input_tokens > 0;

ALTER TABLE usage_log DROP COLUMN cache_creation_input_tokens;
```

---

## ADR-008: Provider API Key 使用 AES-256-GCM 加密

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

Provider API Key 需要持久化存储。如何保护？

### 决策

使用 AES-256-GCM 对称加密，主密钥存储在 `master.key` 文件（权限 0600）。

### 原因

1. **可逆**: 需要解密后发送给上游 API（不能用哈希）
2. **强加密**: AES-256-GCM 是 NIST 标准，抗已知攻击
3. **认证加密**: GCM 模式提供完整性校验，防篡改
4. **简单部署**: 单个主密钥文件，易于备份

### 实现

```rust
// crypto/mod.rs
pub fn encrypt(plaintext: &str, master_key: &[u8]) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(master_key)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, plaintext.as_ref())?;
    
    // nonce || ciphertext
    let mut result = nonce.to_vec();
    result.extend_from_slice(&ciphertext);
    Ok(BASE64.encode(&result))
}
```

### 代价

- 主密钥文件丢失则所有 Provider Key 不可恢复
- 需要保护 `master.key` 文件权限

### 替代方案

- **硬件密钥**: YubiKey/TPM，但增加部署复杂度
- **密钥管理服务**: AWS KMS/Vault，但需要网络连接

---

## ADR-009: Service Key 使用 Argon2 哈希

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

Service Key（客户端访问令牌）如何存储？

### 决策

使用 Argon2id 哈希算法，随机 salt，存储在 `service_keys.key_hash`。

### 原因

1. **不可逆**: Service Key 不需要解密，只需验证
2. **抗暴力破解**: Argon2 是内存硬算法，GPU/ASIC 攻击成本高
3. **OWASP 推荐**: Password Storage Cheat Sheet 首选 Argon2id
4. **随机 salt**: 防止彩虹表攻击

### 实现

```rust
// crypto/mod.rs
pub fn hash_service_key(raw_key: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2.hash_password(raw_key.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

pub fn verify_service_key(raw_key: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(raw_key.as_bytes(), &parsed)
        .is_ok())
}
```

### 代价

- 验证需要逐条遍历所有 Service Key（无法索引查找）
- 哈希计算比 SHA-256 慢（故意设计，抗暴力破解）

---

## ADR-010: 数据库 UPSERT 使用 ON CONFLICT DO UPDATE

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

`save_provider`、`save_model` 等方法需要"存在则更新，不存在则插入"。

### 决策

使用 `INSERT ... ON CONFLICT DO UPDATE`，不使用 `INSERT OR REPLACE`。

### 原因

1. **避免级联删除**: `INSERT OR REPLACE` 会触发 `ON DELETE CASCADE`，误删子表数据
2. **语义明确**: `ON CONFLICT DO UPDATE` 明确表示"冲突时更新"
3. **可控更新**: 可以指定哪些字段更新，哪些保留

### 代价

- SQL 语法更复杂
- 需要为每个表定义冲突处理逻辑

### 实现

```rust
// db/providers.rs
pub fn save_provider(provider: &Provider) -> Result<()> {
    db().execute(
        "INSERT INTO providers (id, name, ...) VALUES (?1, ?2, ...)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           ...",
        params![provider.id, provider.name, ...],
    )?;
    Ok(())
}
```

### 回归测试

```rust
#[test]
fn test_upsert_no_cascade_delete() {
    // 1. 插入 provider
    // 2. 插入子表数据（api_keys, models）
    // 3. 更新 provider
    // 4. 验证子表数据未被删除
}
```

---

## ADR-011: 协议转换显式处理不兼容特性

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

Anthropic 和 OpenAI API 有语义差异（如 `thinking`、`tool_choice`）。如何处理？

### 决策

显式转换不兼容特性，记录 warn 日志，不静默丢弃。

### 原因

1. **可调试**: warn 日志让用户知道哪些特性被转换/丢弃
2. **可预测**: 明确的行为比隐式丢弃更容易理解
3. **可改进**: 日志帮助识别需要支持的常见特性

### 实现

```rust
// api/proxy/translate/to_openai.rs
pub fn anthropic_req_to_anthropic(req: &AnthropicRequest) -> OpenAIRequest {
    if let Some(thinking) = &req.thinking {
        warn!("thinking 特性转换为 reasoning_content（非官方字段）");
        // 转换为 OpenAI 的 reasoning_content
    }
    
    match req.tool_choice {
        ToolChoice::Any => {
            // Anthropic "any" → OpenAI "required"
            "required"
        }
        // ...
    }
}
```

### 已知不兼容

- `thinking` → `reasoning_content`（非官方）
- `tool_choice.any` → `tool_choice.required`
- `stop_reason.end_turn` → `finish_reason.stop`

---

## ADR-012: WebSearch 劫持使用本地 Bing 搜索

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

上游 LLM API 的 `web_search` 工具需要付费，且结果质量不可控。

### 决策

提供可选的 WebSearch 劫持：拦截包含 `web_search` 工具的请求，用本地 Bing 搜索替代。

### 原因

1. **成本节约**: 避免上游 web_search 费用
2. **可控性**: 本地搜索可以定制（如使用 cn.bing.com）
3. **隐私**: 搜索请求不经过上游 API

### 实现

```rust
// api/proxy/websearch.rs
pub fn run_websearch_loop(/* ... */) -> Response {
    let mut messages = initial_messages.clone();
    
    for _ in 0..5 {
        // 1. 发送给上游（stream=false）
        let resp = send_to_upstream(&messages)?;
        
        // 2. 检查是否需要搜索
        if let Some(tool_calls) = extract_tool_calls(&resp) {
            for call in tool_calls {
                if call.name == "web_search" {
                    // 3. 本地 Bing 搜索
                    let results = bing::search(&call.query)?;
                    
                    // 4. 构造 tool_result
                    messages.push(Message::ToolResult {
                        tool_call_id: call.id,
                        content: format_results(&results),
                    });
                }
            }
        } else {
            // 5. 无工具调用，返回最终响应
            return resp;
        }
    }
}
```

### 代价

- 需要维护 Bing 搜索 scraper（反爬策略变化）
- 最多 5 轮 tool-calling loop，延迟增加
- 搜索结果质量可能不如上游 API

### 开关

`settings.websearch_hijack` 控制是否启用，默认关闭。

---

## ADR-013: 插件系统采用 WebSocket 注册

**日期**: 2026-08-01  
**状态**: 已接受

### 背景

需要支持"委托供应商"（如钉钉 DEAP），插件负责协议转换 + 业务头注入。

### 决策

插件通过 WebSocket 连接 `/ws/plugin`，发送注册/心跳/密钥同步消息。

### 原因

1. **实时通信**: WebSocket 支持双向消息，适合心跳 + 密钥同步
2. **生命周期管理**: 连接断开自动检测（90s 无心跳标记离线）
3. **解耦**: 插件是独立进程，崩溃不影响 Router

### 协议

```typescript
// 插件 → Router
{ type: "register", plugin_id: "wukong", provider: { kind: "deap", base_url: "http://...", api_path: "/v1/..." }, models: [...], keys: [...] }
{ type: "heartbeat" }
{ type: "keys_update", provider_id: "...", keys: ["sk-xxx", "sk-yyy"] }

// Router → 插件
{ type: "registered", plugin_id: "wukong" }
```

插件状态使用纯字符串（非枚举），取值为 `"pending"`（等待确认）、`"active"`（已确认）、`"offline"`（心跳超时）。

### 职责分工

| 职责 | Router | Plugin |
|------|--------|--------|
| 密钥轮换 | ✅ | ❌ |
| 健康监控 | ✅ | ❌ |
| 用量统计 | ✅ | ❌ |
| 协议转换 | ❌ | ✅ |
| 业务头注入 | ❌ | ✅ |

### 代价

- 需要维护 WebSocket 连接状态
- 插件离线时委托供应商不可用

---

## ADR-014: 模型撞名按 sort_order + created_at 排序

**日期**: 2026-08-01  
**状态**: 已接受

### 背景

多个 Provider 提供相同 `display_name` 的模型（如 `claude-opus-4-8`）。如何选择？

### 决策

路由解析时按 `sort_order ASC, created_at ASC` 排序，取第一条。

### 原因

1. **可预测**: 用户可以通过拖拽排序控制优先级
2. **公平**: 相同优先级时，先创建的优先
3. **简单**: 不需要复杂的负载均衡算法

### 实现

```rust
// api/proxy/route.rs
pub fn resolve_route(state: &AppState, display_name: &str) -> Option<ResolvedRoute> {
    let model = db().query_row(
        "SELECT m.*, p.* FROM models m
         JOIN providers p ON m.provider_id = p.id
         WHERE m.display_name = ?1 AND m.enabled = 1 AND p.enabled = 1
         ORDER BY p.sort_order ASC, p.created_at ASC
         LIMIT 1",
        params![display_name],
        |row| Ok(ModelProvider { /* ... */ }),
    ).ok()?;
    
    // ...
}
```

### 代价

- 无法实现加权负载均衡（如 70% 流量到 Provider A，30% 到 B）
- 主 Provider 故障时，需要等所有 key 都 Red 才会切换到备用

### 未来改进

如需负载均衡，可以新增 `routes` 表（已预留），支持 `weight` 字段。

---

## ADR-015: Token 配额用滚动窗口 + 按需聚合（V14）

**日期**: 2026-08-02  
**状态**: 已接受

### 背景

需求：每个 Service Key 可配置 5 小时 / 7 天内的 token 上限，触顶返回 429。需要决定窗口口径与用量来源。

### 决策

1. **滚动窗口而非固定时段**：窗口按 Unix 时间对齐（`now % window_secs`），不是自然日/自然小时。与上游计费（Anthropic 5h、OpenAI 类似滚动周期）语义一致，实现只依赖 `usage_log.timestamp` 单列。
2. **上限持久化、用量按需聚合**：`service_keys` 只存 `quota_5h/quota_7d`（0 = 不设限）；已用量每次从 `usage_log` 条件聚合（`SUM(prompt + completion + cache_read)`）。不维护额外计数器，避免写路径多一次同步、且重启后天然一致。
3. **429 采用 quota_error 类型**：模拟 Anthropic 错误体风格，携带 `retry-after` 头（剩余秒数）；`message` 内含可读的重置时间（`Resets in 2h31m.`）。

### 原因

1. **正确性**：固定时段在窗口边界会瞬时放行大量请求（月初/日初全额重置），滚动窗口平滑且与上游配额对齐
2. **简单**：单条 SQL 即得两窗口用量，无新增状态
3. **一致**：`/v1/user/balance` 与表格「限额」列共用同一聚合函数，展示与判定永不分叉

### 代价

- 每个代理请求多一次 SQLite 条件聚合查询（有 `idx_usage_service_key` + `idx_usage_timestamp` 索引，单用户本地规模无感）
- 聚合统计的是「已写库」的用量，正在流式传输的请求有 ≤ 5 分钟延迟才计入（可接受：流式请求的 token 是渐进消耗的）

### 未来改进

如未来需要更细粒度（按模型、按分钟），可在同一聚合函数上加条件扩展。

---

## ADR-016: 统一 HTTP 客户端工厂 + 系统代理自动继承

**日期**: 2026-08-03  
**状态**: 已接受

### 背景

项目有 6 处出站 HTTP 请求（代理转发、WebSearch Bing 搜索、Provider 适配器、上游模型拉取），各自用 `reqwest::Client::new()` 或 `Client::builder()` 独立构建。国内网络下钉钉 DEAP 等上游需走 Clash 等代理才能连通，但散落构建无法统一注入代理。

### 决策

新增 `http.rs` 模块作为唯一 HTTP 客户端工厂：

1. `system_proxy()`: 解析系统代理，OnceLock 缓存（代理在运行期间几乎不变）
   - 优先读环境变量（`HTTPS_PROXY` > `HTTP_PROXY` > `ALL_PROXY`，大小写兼容）
   - Windows 回退到注册表 `HKCU\...\Internet Settings`（ProxyEnable + ProxyServer）
   - 跳过 PAC（AutoConfigURL）
2. `build_http_client() -> ClientBuilder`: 返回带系统代理的 builder，调用方可继续链式覆盖 timeout / cookie_store
3. `http_client() -> Client`: 便捷方法，默认构建
4. NO_PROXY 默认豁免 `localhost`、`127.0.0.1`、`[::1]`（插件系统上游在本机），并附加环境变量 `NO_PROXY` 的额外项

所有出站 HTTP 请求必须使用工厂方法，不允许直接 `reqwest::Client::new()`。

### 原因

1. **统一代理**：6 处调用点只需改一行就全部接入代理，未来新增出站请求也不会遗漏
2. **零配置**：Windows 用户配 Clash 系统代理后，xrl-router 自动继承，无需在应用内手动设置
3. **性能**：OnceLock 缓存代理解析结果，只读一次注册表（`reg query` 调用 ~50ms）
4. **可测试**：工厂方法返回 builder 而非 final client，调用方可覆盖 timeout 等参数

### 代价

- 代理在应用运行期间不可变（Clash 端口固定，实际无影响）
- Windows 注册表解析依赖 `reg query` 子进程（仅首次调用，失败时静默回退到无代理）
- 非 Windows 系统只支持环境变量（无注册表回退，但跨平台标准做法）

### 迁移

6 处调用点已全部替换：
- `api/proxy/handler.rs` (2 处): `reqwest::Client::builder()` → `crate::http::build_http_client()`
- `api/proxy/websearch.rs` (1 处): `reqwest::Client::builder()` → `crate::http::build_http_client()`
- `api/handlers/models.rs` (1 处): `reqwest::Client::new()` → `crate::http::http_client()`
- `providers/anthropic.rs` (1 处): `Client::new()` → `crate::http::http_client()`
- `providers/openai.rs` (1 处): `Client::new()` → `crate::http::http_client()`
- `search/bing.rs` (1 处): `reqwest::Client::builder()` → `crate::http::build_http_client()`

---

## ADR-017: 单 listener + 路径级 IP 限制（局域网分发）

**日期**: 2026-08-06  
**状态**: 已接受  
**取代**: ADR-017 旧版（双 listener 分离监听，2026-08-03）

### 背景

旧方案（ADR-017 旧版）用两个 listener 分离：admin 绑 `127.0.0.1:19068`，public 绑 `0.0.0.0:19069`。虽然安全隔离清晰，但增加了设计复杂度——两个端口维护、两处挂 `/v1/*` 路由、防火墙要放行两个端口、CORS 两套策略。实际运行中管理端点已有 CORS 白名单保护，且本机进程访问是接受的代价。

### 决策

合并为单 listener 绑 `0.0.0.0:19068`，通过路径级 IP 中间件控制访问权限：

| 路径类型 | 限制方式 | 路由 |
|----------|----------|------|
| 公开 | 不限 IP | `/health`、`/ws`、`/ws/plugin`、`/install` 静态页、`/v1/*` 代理 |
| 管理 | `admin_ip_guard` 中间件限 loopback | `/api/*` CRUD、`/api/install/local-ip`、`/api/data/*` |

- `router.rs` 统一为 `build_router(state)`，`/api/*` 子路由挂 `middleware::from_fn(admin_ip_guard)` 层
- `admin_ip_guard` 用 `ConnectInfo<SocketAddr>` 提取客户端 IP，非 loopback（`127.0.0.1` / `::1`）返回 403
- `server.rs` 使用 `into_make_service_with_connect_info::<SocketAddr>()` 启用 IP 提取
- `Config` 删除 `public_host` / `public_port` / `enable_public` 字段，`host` 默认值改为 `0.0.0.0`
- CORS 统一使用 origin 白名单（`Config.cors_origins`，7 个）

### 原因

1. **单端口简化运维**：防火墙只需放行 19068，前端/客户端只需配一个端口
2. **IP 中间件而非双 listener**：axum 的 `ConnectInfo` 提供可靠的客户端 IP 提取，路径级限制比端口级更灵活（新增管理端点自动受保护，无需记得"两个 router 都挂"）
3. **保留 loopback 安全模型**：`admin_ip_guard` 确保 `/api/*` 管理端点永不对外开放，与旧方案的安全等级一致
4. **统一 CORS**：不再需要 public 的全开 CORS（install 页面同源，CLI 无 origin 约束），白名单对所有路径统一生效

### 代价

- `0.0.0.0:19068` 向局域网暴露：任何人可调 `/v1/*`（需有效 key）、可看 `/install`（无 key 仅提示页）。密钥明文嵌在分发 URL 里，局域网嗅探可见——接受此风险，分发 key 即普通 service key，撤销即在密钥列表删除
- 依赖 `ConnectInfo` 正确提取 IP（axum + tokio TCP listener 已验证可靠）；若未来加反向代理，需改用 `X-Forwarded-For` 或 `X-Real-IP`

### 未来改进

- 若需公网访问（不在设计内），应做 TLS + 反向代理 + `X-Forwarded-For` 提取，而不是改绑定地址
- 若需更细粒度权限（如只读/只写分离），可在 `/api/*` 子路由上加多层 IP guard 或引入 role-based middleware


---

## ADR-018: 国际化（zh-CN / en）与数据导出导入

**日期**: 2026-08-03  
**状态**: 已接受

### 背景

AGENTS.md 原 Non-Goal「不做国际化，中文即可」，2026-08 用户主动要求实现 i18n 并扩展到全应用，同时要求数据导入/导出/重置（原 Non-Goal「不做数据导出/报表」被推翻）。两条 Non-Goal 已同步更新为 ✅ 状态。

### 决策

1. **前端 i18n**：`src/i18n/` 极简实现（`t(key)` + `setLocale()` + `reactive` 响应式 + localStorage 持久化），不引入 vue-i18n；翻译 key 按模块前缀（`nav.*`、`keys.*`、`stats.*` 等），语言包为 `zh-CN.ts` / `en.ts`
2. **主题增加「跟随系统」**：`Theme` 扩展为 `light | dark | system`，监听 `prefers-color-scheme` 变化自动切换
3. **开机静默启动**：`tauri-plugin-autostart`（MacosLauncher::LaunchAgent + `--minimized` 参数）
4. **数据管理**：`GET /api/data/export`（SQL dump）、`POST /api/data/import`、`POST /api/data/reset`，走 `db/settings.rs` 的 `export_sql` / `import_sql` / `reset_all_data`；前端用 `tauri-plugin-dialog` + `tauri-plugin-fs` 读写 .sql 文件
5. **后端文本同步**：托盘菜单（显示窗口/退出）语言存 DB `settings.locale`，`set_locale` Tauri command 动态更新菜单文本；`install.html` 分发页内联双语字典，按浏览器语言 + 手动切换

### 原因

1. 极简 i18n 避免引入重依赖；设置页先行，全应用跟进，key 前缀模式保证可维护
2. 数据导出用 SQL 而非 JSON：SQLite 原生 dump 保真度高，且导入即执行，天然支持跨版本迁移

### 代价

- 新增/修改 UI 文本时必须维护两个语言包（AGENTS.md 已注明约束）
- 后端托盘菜单文本依赖前端调用 `set_locale` 同步，纯后端改动场景下保持上次语言

---

## ADR-019: 故障转移（Provider Failover）——同别名多 Provider 候选 + 60s 冷却

**日期**: 2026-08-03  
**状态**: 已接受

### 背景

同一模型别名配置在多个 Provider（官方 + 代理镜像）时，上游故障会让整个会话失败。PRD 原「指数退避重试」（F-39）停留在计划中——重试同一上游没有意义，真正的问题是选错上游。方案升级为「换上游」而非「重试同一上游」。

### 决策

1. **候选解析**：`route.rs` 新增 `resolve_route_candidates()`，返回同 `display_name` 下全部候选（`sort_order ASC, created_at ASC`，按 provider_id 去重，跳过插件离线的委托 provider）；原 `resolve_route()` 保留，供开关关闭时使用
2. **双层循环**：`handler.rs` 两个入口对称重写——外层遍历 provider 候选，内层遍历 key 池；key 级 4xx（401/402/403/429）先耗尽当前 provider 的 key，provider 级失败（5xx/网络错误/响应头超时）才切 provider
3. **冷却表纯内存**：`failover.rs` 的 `provider_cooldowns`（provider_id → 到期时间，60s），与密钥健康同一哲学——不持久化、不广播；2xx 成功立即清除冷却
4. **开关默认关闭**：`failover_enabled` 存 settings 表 + `Arc<AtomicBool>` 运行时切换（设置页「路由」Tab）；关闭时 `resolve_route` 包成单元素 vec，行为与历史完全一致
5. **请求体预构造**：循环外构建 Anthropic/OpenAI 两种 body 骨架，循环内按候选类型选用并覆写 model——支持候选混合 OpenAI/Anthropic 类型（mixed-kind failover）
6. **错误码语义收窄**：网络错误 502、响应头超时 504、key 4xx 耗尽透传最后一次上游失败响应、无可用 key 503

### 原因

1. 按 provider_id 去重是必要的：同一 provider 多行同别名（如多个模型行指向同一 provider）去重后按 sort_order 取最前，避免同一上游被重复尝试
2. 冷却 60s 而非指数退避：场景是「上游暂时不可用」，固定冷却足够且行为可预期；PRD 的指数退避重试（重试同一上游）已无意义，F-46 注明
3. 开关默认关闭符合 AGENTS.md「不主动改变既有行为」原则——failover 是增益特性，不是修复

### 代价

- 双循环复杂度集中在一个文件（handler.rs 两个入口对称改，约 +800 行）；winner 选定后需重绑定 resolved/provider_id/real_model_id 等变量，流式段与 usage_log 字段才自洽
- 开关关闭路径与历史行为一致，但新增了 `ProviderFailure` 枚举与失败上下文传递，后续改动需注意两个循环的对称性

---

## ADR-020: handler.rs 拆分 + 流式引擎独立 + SSE 即时响应

**日期**: 2026-08-04  
**状态**: 已接受

### 背景

`handler.rs` 膨胀到 1371 行，`proxy_anthropic_messages` 和 `proxy_openai_chat` 两个函数各 ~630 行，90% 逻辑完全相同（认证→路由→密钥轮换→双循环→错误处理→流式转发）。唯一的差异是客户端格式（Anthropic vs OpenAI）决定的请求体准备和响应翻译方向。

同时存在两个运行时问题：
1. **Subagent 超时**：handler 在返回 SSE Response 之前需要同步完成认证→路由→密钥→上游连接→等待响应头（正常约 8 秒），期间客户端收不到任何字节。Claude Code 判定连接无响应，subagent 被放弃转为自己执行。
2. **输出非逐 token 流式**：passthrough 路径缺少 `Cache-Control: no-cache` 和 `Connection: keep-alive` 响应头，且无 keepalive 心跳；每次请求新建 `reqwest::Client`，增加首次响应延迟。

### 决策

1. **handler.rs 拆为薄入口层**（~250 行）：每个 handler 只负责提取 API key + 调用 `authenticate_and_stream()` + 委托 `stream::proxy_stream()`。`proxy_list_models` 保留不变（独立逻辑）。
2. **新建 stream.rs 作为流式引擎核心**（~550 行）：`proxy_stream()` 接收已认证的 `StreamContext`，完成路由解析→WebSearch 劫持→立即返回 Response（含 keepalive）→后台 spawn 双循环重试→错误处理→委托 forward.rs 流式转发。
3. **新建 forward.rs 作为流式转发分支**（~350 行）：passthrough / O→A / A→O 三种流转发模式，在 spawn 内调用。
4. **SSE 即时响应**：路由解析后立即返回 Response（含 `:keepalive\n\n` 初始字节），后台 `tokio::spawn` 处理上游连接+密钥轮换+流式转发。响应头补全 `Cache-Control: no-cache`、`Connection: keep-alive`、`X-Accel-Buffering: no`，并每 15 秒发送 keepalive 心跳。上游错误通过 SSE error event 传达（而非 HTTP status code）。
5. **共享 HTTP client**：`AppState` 新增 `http_client: reqwest::Client` 字段，handler 使用 `state.http_client.clone()`（只复制 Arc，零成本），复用连接池和 TLS 缓存。

### 原因

1. **消除重复**：两个 handler 的 90% 逻辑合并为 `proxy_stream()`，后续修改只需改一处
2. **修复超时**：客户端在毫秒级内收到首字节（keepalive 注释），不再因 8 秒上游等待而超时断开
3. **修复缓冲**：正确的 SSE 响应头防止中间代理/客户端缓冲数据，token 逐字显示
4. **减少延迟**：共享 HTTP client 复用连接池，同一上游的后续请求无需重新 TCP+TLS 握手
5. **可维护性**：handler.rs 从 1371 行降到 ~250 行，stream.rs ~550 行 + forward.rs ~350 行，职责清晰

### 代价

- `StreamContext` 结构体需要携带两种格式的请求体（body_anthropic + body_openai），即使 passthrough 路径只用一种
- stream.rs 的 4 个流式分支函数各有大量 clone 参数（日志字段），可进一步用宏或结构体减少样板代码

### 文件变更

| 文件 | 变化 |
|------|------|
| `gateway/server.rs` | AppState 添加 `http_client: reqwest::Client` |
| `api/proxy/stream.rs` | **新建**：流式引擎核心（路由解析 → 立即返回 Response → 后台 spawn 双循环 + 错误处理） |
| `api/proxy/forward.rs` | **新建**：流式转发分支（passthrough / O→A / A→O） |
| `api/proxy/handler.rs` | 1371→~250 行：薄入口 + authenticate_and_stream() |
| `api/proxy/upstream.rs` | **删除**：上游错误改为 SSE error event 传达 |
| `api/proxy/mod.rs` | 添加 `pub mod stream;` + `pub mod forward;`，移除 `pub mod upstream;` |

---

## ADR-021: keepalive 心跳改用 oneshot 取消信号，修复流式响应永不结束

**日期**: 2026-08-05  
**状态**: 已接受

### 背景

ADR-020 的 SSE 即时响应里，keepalive 心跳任务持有 `tx`（mpsc 发送端）的 clone。问题：主任务（流式转发）结束后，keepalive 任务仍持有 `tx` clone，导致 mpsc 的 `rx` 侧永远等不到所有 sender drop，`Response<Body>` 的 stream 永不结束——客户端连接挂住，Claude Code 收到的是「永不收尾的 SSE」，表现为流式响应卡死。

### 决策

keepalive 任务**只持有取消信号**（`tokio::sync::oneshot`），不持有 `tx` clone：

1. 主任务创建 `oneshot::channel::<()>()`，把 `cancel_tx` 包进 `CancelOnDrop` guard
2. keepalive 任务持有 `keepalive_tx = tx.clone()` 与 `cancel_rx`，循环 `select!` 每 15s 发心跳 / 收到 cancel 即 break
3. 主任务任何路径结束（正常收尾 / 上游错误 / 超时）→ Drop `CancelOnDrop` → 触发 cancel → keepalive 任务退出
4. `tx` 唯一属于主任务，主任务 drop 后 `rx` 侧的 stream 自然收尾

### 原因

1. **mpsc 语义**：`rx` 的 `next()` 在所有 sender drop 后返回 `None`，stream 才结束。keepalive 持有 clone 就打破了「主任务结束 = stream 结束」的对应关系
2. **取消信号单职责**：keepalive 只需知道「该停了」，不需要发送业务数据；oneshot 足够且零开销
3. **Drop 兜底**：`CancelOnDrop` 保证任何 panic / 提前 return 路径都能触发取消，不会泄漏 keepalive 任务

### 代价

- 多一个 `oneshot` channel 与 `CancelOnDrop` guard 的样板代码
- keepalive 任务不再能独立于主任务存活（本就是期望行为）

### 实现

```rust
// api/proxy/stream.rs
let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
let keepalive_tx = tx.clone();
let keepalive_handle = tokio::spawn(async move {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(KEEPALIVE_INTERVAL) => {
                if keepalive_tx.send(Ok(Bytes::from(":keepalive\n\n"))).await.is_err() {
                    break;
                }
            }
            _ = &mut cancel_rx => break,
        }
    }
});
struct CancelOnDrop(Option<oneshot::Sender<()>>);
impl Drop for CancelOnDrop { fn drop(&mut self) { ... } }
```

---

## ADR-022: 自适应上游头超时 + 放宽请求体限制

**日期**: 2026-08-06  
**状态**: 已接受

### 背景

ADR-020 后头超时固定 60s。两个新问题在大输入长等待场景暴露：

1. **头超时过短**：大上下文（~80k token 缓存输入）+ 上游排队时，首字节常超 60s。网关提前放弃并发 SSE error event 断流 → Claude Code 把「流中断」当可回退错误，切换非流式重试 → 网关强制 `stream=true`，回退请求收到 SSE 无法解析为 Message JSON → 用户看到「API returned an empty or malformed response (HTTP 200)」
2. **请求体 413**：axum 默认 `DefaultBodyLimit` 只放行 2MiB，超长会话（多轮历史 + 工具结果 + base64 截图）被 413 直接拒绝——「输入太大」报错的另一半成因

### 决策

1. **基准头超时提到 300s**（`UPSTREAM_HEADER_TIMEOUT_SECS`）：对齐 Claude Code 的 `API_TIMEOUT_MS` 默认值与 CLI 侧 SSE 空闲看门狗 90s（等待头期间网关每 15s 的 keepalive 足以维持客户端连接）
2. **按估算输入规模自适应放宽** `header_timeout_for(est_input_tokens)`：≥100k token → 600s；≥50k → 480s；其余 → 基准 300s
3. **请求体上限放宽到 64MiB**（`MAX_REQUEST_BODY_BYTES`）：覆盖多模态大会话（Anthropic 对 base64 图片本身上限 5MB/张，64MiB 足够）；`proxy_routes()` 套 `DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES)`

### 原因

1. **头超时本质是兜底**：上游建连后挂起不响应才该放弃，正常大输入慢但会响应。固定 60s 把「慢但正常」误判成「挂死」
2. **输入规模可估算**：输入 token 数与上游处理时间强相关，分档放宽既覆盖大输入又不让小请求白等 600s
3. **300s 基准对齐客户端**：Claude Code 自身超时就是 300s 量级，网关比客户端早放弃毫无意义
4. **64MiB 覆盖现实峰值**：2MiB 默认值对多模态不现实，64MiB 远超单张图片上限又有余量

### 代价

- 大输入挂起场景要等更久（300s 起）才判定失败——可接受，因为 keepalive 维持连接、客户端不会先断
- 64MiB 请求体占内存：单用户本地场景并发低，无内存压力

### 实现

```rust
// api/proxy/mod.rs
pub(crate) const UPSTREAM_HEADER_TIMEOUT_SECS: u64 = 300;
pub(crate) const UPSTREAM_CHUNK_TIMEOUT_SECS: u64 = 120;
pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn header_timeout_for(est_input_tokens: u64) -> u64 {
    if est_input_tokens >= 100_000 { 600 }
    else if est_input_tokens >= 50_000 { 480 }
    else { UPSTREAM_HEADER_TIMEOUT_SECS }
}

// api/router.rs
fn proxy_routes(state: &Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/messages", post(proxy::proxy_anthropic_messages))
        // ...
        .layer(DefaultBodyLimit::max(super::proxy::MAX_REQUEST_BODY_BYTES))
}
```

---

## ADR-023: WebSearch 劫持路径统一为 SSE 即时响应 + oneshot 取消信号

**日期**: 2026-08-07  
**状态**: 已接受  
**延伸**: ADR-020（SSE 即时响应）、ADR-021（keepalive oneshot 取消信号）

### 背景

ADR-020/021 把主代理路径（`proxy_stream`）改造为「路由解析后立即返回 Response（含 `:keepalive` 首字节）+ 后台 spawn 处理上游连接与流式转发」，并用 oneshot 取消信号驱动 15s keepalive 心跳。但 WebSearch 劫持路径（`run_websearch_loop`）仍是同步阻塞：跑完多轮 tool-calling loop（本地 Bing 搜索，最多 5 轮上游往返）后才用 `Sse::new(stream).keep_alive(KeepAlive::default())` 返回响应。`stream.rs` 里还留着 `// TODO: websearch_loop 也有上游阻塞，后续可改为同样的 spawn 模式`。

问题：多轮 tool-calling 期间客户端收不到任何字节，与主路径修复前的 subagent 超时是同一类隐患。上游错误则返回 `(StatusCode, Json)` HTTP 4xx/5xx，与主路径「上游错误通过 SSE error event 传达」的契约不一致。

### 决策

1. **WebSearch 路径与主路径同构**：`run_websearch_loop` 立即创建 mpsc channel + 发 `:keepalive` 首字节 + 返回 `stream::sse_response(rx)`，hijack loop 在 `tokio::spawn` 内完成
2. **复用主路径的取消信号模式**：`oneshot::channel::<()>()` + `CancelOnDrop` guard + keepalive 任务 `select!`（tick 发心跳 / cancel 即 break），与 ADR-021 实现完全对称
3. **上游错误改走 SSE error event**：`hijack_anthropic` / `hijack_openai` 的错误返回类型从 `Result<Option<Response>, (StatusCode, HeaderMap, Json<Value>)>` 改为 `Result<(), (String, String)>`（error_type + message），由 `stream::send_error_event` 发送给客户端，不再返回 HTTP 4xx/5xx
4. **响应头集合抽取共用**：`stream::sse_response(rx)` 与 `stream::send_error_event` / `SSE_KEEPALIVE_SECS` 提为 `pub(super)`，两条路径响应头永远一致
5. **`client_format` 硬编码为 `ClientFormat::Anthropic`**：WebSearch 劫持实质只经 `/v1/messages` 入口触发

### 原因

1. **兑现 ADR-020 的 TODO**：主路径已验证的「立即响应 + 后台 spawn」模式搬到 websearch，消除多轮 loop 期间的客户端超时隐患，行为可预期
2. **错误传达契约统一**：主路径上游错误走 SSE error event，websearch 跟进一致，避免「一条路径返回 HTTP 4xx、另一条返回 SSE error」的分叉
3. **`has_websearch_tool` 只匹配 Anthropic 风格**：检测 `tools[].type` 以 `web_search` 开头；OpenAI 客户端的 `tools` 是 `{"type":"function","function":{...}}`，不会命中，故 WebSearch 劫持实质只经 Anthropic 入口（`/v1/messages`）触发。`build_sse_bytes` 只产 Anthropic SSE 事件序列与之一致——这是基线既有行为，非本次引入
4. **取消信号复用**：oneshot + `CancelOnDrop` 的兜底语义（panic / 提前 return 都触发取消）对 websearch 多轮 loop 同样必要

### 代价

- websearch 多一个 mpsc channel + `CancelOnDrop` guard 的样板代码（与主路径重复）
- `hijack_anthropic` / `hijack_openai` 错误返回从带 `StatusCode` 的 Response 收窄为 `(error_type, message)`，丢失了上游原始 JSON body 的透传——但主路径本就不透传上游错误 body（走 SSE error event），一致性优先

### 实现

```rust
// api/proxy/websearch.rs
pub(super) async fn run_websearch_loop(
    state: Arc<AppState>, body: Value, resolved: ResolvedRoute,
    provider_is_anthropic: bool, trace_id: String, service_key: ServiceKeyInfo,
) -> Result<Response, (StatusCode, HeaderMap, Json<Value>)> {
    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);
    let _ = tx.send(Ok(Bytes::from(":keepalive\n\n"))).await;
    tokio::spawn(async move {
        // oneshot 取消信号 + CancelOnDrop（与 stream.rs 主路径同构）
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let keepalive_handle = tokio::spawn(async move { /* select! tick / cancel */ });
        let _cancel_guard = CancelOnDrop(Some(cancel_tx));
        // ... hijack loop，错误走 send_error_event，正常走 build_sse_bytes
    });
    Ok(super::stream::sse_response(rx))
}
```

---

## ADR-024: 密钥解密失败不再回退密文，改为告警并跳过

**日期**: 2026-08-07  
**状态**: 已接受  
**关联**: ADR-008（AES-256-GCM 加密 Provider Key）、ADR-003（健康状态纯内存）

### 背景

`keys/pool/persistence.rs` 的 `load_provider_keys` / `load_all_keys_from_db` 在解密 `api_keys.key_hash` 失败时用 `crypto::decrypt(&cipher, master_key).unwrap_or_else(|_| cipher.clone())` 回退——把**数据库密文当作明文 key** 装进 KeyPool。这是与 ADR-008 加密哲学直接矛盾的安全 bug：解密失败的 key 会被 round-robin 选中，以密文字符串作为 API key 发给上游。

回退注释写「legacy plaintext」但 V9 之后所有 key 一律 AES-256-GCM 加密入库，legacy 明文已不可能存在，回退路径无正当用例。

### 决策

解密失败时 `tracing::warn!` 告警并 `skip`（不加入 KeyPool），不再回退到密文。SQL 同步移除对 `status` / `last_error_time` 两列的 SELECT（AGENTS.md 已注明这两列「保留但不再读写」，原代码读后立刻用 `KeyStatus::Green` 覆盖，是死读）。

### 原因

1. **加密必须能解密，否则跳过**：把密文当明文 key 发给上游是安全 bug，跳过该 key 才符合 ADR-008
2. **与 ADR-003 自洽**：跳过的 key 不入池，不会被轮到；启动时其余 key 全 green，行为与「健康状态纯内存、启动全 green」一致
3. **死读清理**：`status` / `last_error_time` 既不再读写，SELECT 它们是纯噪音；列保留仅为不触发 schema 迁移
4. **锁序不变**：闭包内只取原始字段、conn 锁在块内释放后再解密，ABBA 死锁规避规则未被破坏

### 代价

- 若 `master.key` 丢失或损坏，所有 key 解密失败 → KeyPool 为空 → 代理返回 503（No available upstream keys）。这是正确行为：主密钥丢失本就不可恢复（ADR-008 已注明），回退密文只会让上游用错误 key 报 401，制造假象
- 用户需在日志里观察 `decrypt failed, skipping key` 警告，主动检查 master.key 完整性

### 实现

```rust
// keys/pool/persistence.rs
let keys: Vec<KeyEntry> = raw.into_iter().filter_map(|(.., cipher, ..)| {
    let plain = match crypto::decrypt(&cipher, master_key) {
        Ok(p) => p,
        Err(e) => { tracing::warn!(key_id = %id, error = %e, "decrypt failed, skipping key"); return None; }
    };
    Some(KeyEntry { key_hash: plain, status: KeyStatus::Green, .. })
}).collect();
```
