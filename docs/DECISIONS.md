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
**状态**: 已被取代  
**被取代**: ADR-028（本地搜索 + IR 注入）→ ADR-030（恢复 tool-calling loop + 无进展检测）

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
**状态**: 已接受（部分演进：ADR-027 引入 IR 后 forward.rs 从三路分支统一为单一 `forward_stream_ir`）

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

---

## ADR-025: Claude FM 播放下沉后端 + 系统媒体控制集成

**日期**: 2026-08-08  
**状态**: 已接受

### 背景

Claude FM 此前的架构链路：Rust 后端从 CDN 拉音频字节 → `broadcast::channel` → HTTP chunked `/fm/live` → 前端 `<audio>` 标签缓冲 2~5s → 实际播放。为补偿前端缓冲延迟，引入了"墙钟锚点 + `currentTime` 二分查找"的间接 caption 同步机制。但误差根源始终存在——后端字节流进度与前端播放位置不可消除的延迟。

### 决策

将音频解码与播放从前端 `<audio>` 标签迁移到 Rust 后端，直接输出到系统音频设备：

1. **rodio 0.20**：MP3 解码 + 系统音频设备输出，在专用 `std::thread` 中运行（rodio 需要稳定线程，不能用 tokio）
2. **souvlaki 0.8**：系统媒体控制集成（macOS Now Playing / 媒体键、Windows SMTC、Linux MPRIS），在主线程初始化，通过 `mpsc` channel 接收控制消息
3. **异步预加载（双缓冲）**：当前曲播放时 `tauri::async_runtime::spawn` 预下载下一曲（`tokio::sync::oneshot` 传递结果），切歌零等待
4. **删除 HTTP 直播流**：移除 `/fm/live`、`/fm/meta`、`/fm/schedule` 三个 HTTP 端点，FM 不再经过网络层
5. **前端退化为纯展示+控制层**：`src/fm/player.ts` 从 ~180 行简化为 ~100 行，删除 `<audio>` 单例、HTTP 请求、墙钟同步逻辑，仅通过 Tauri command（`fm_toggle` / `fm_play` / `fm_pause` / `fm_get_state`）和事件（`fm-meta` / `fm-ready` / `fm-state-changed`）与后端交互
6. **托盘 FM 菜单直接调引擎**：托盘点击不再 emit `fm-toggle` 事件绕前端中转，直接调用 `FmEngine.toggle()`

### 原因

1. **消除误差根因**：后端直接播放，播放位置即真相，不再需要墙钟锚点间接计算
2. **简化架构**：前端从 ~180 行减至 ~100 行，删除所有音频相关逻辑；后端不再需要 broadcast channel + HTTP chunked 流
3. **系统级集成**：macOS 控制中心、媒体键、锁屏 Now Playing widget 均可控制播放，用户体验显著提升
4. **切歌零等待**：双缓冲预加载使曲目切换无感知延迟

### 代价

- `std::thread` 与 `tokio` 异步运行时的桥接：CDN 下载通过 `tauri::async_runtime::block_on` 桥接，增加一层间接性
- souvlaki 在 macOS debug build 可能 panic（#77），release build 正常；Windows SMTC `set_metadata` 可能 hang（#39）——通过 `Option<MediaControls>` + 错误容错降级为 stub
- 首次播放延迟 1~3s（CDN 下载首曲），后续曲目因预加载而零等待

### 实现

```rust
// api/handlers/fm.rs
pub struct FmEngine {
    control_tx: Arc<mpsc::Sender<FmControl>>,
    control_rx: Arc<Mutex<Option<mpsc::Receiver<FmControl>>>>,
    state: Arc<Mutex<FmPlaybackState>>,
    http_client: reqwest::Client,
}

// lib.rs — souvlaki 初始化（主线程）
let platform_config = souvlaki::PlatformConfig { ... };
let media_controls = MediaControls::new(platform_config).ok();
// souvlaki 回调 → control_tx.send(FmControl::Toggle/Play/Pause)

// engine_loop（std::thread）
let sink = rodio::Sink::try_new(&stream_handle)?;
loop {
    let bytes = next_preload.blocking_recv()?;  // 双缓冲
    sink.append(rodio::Decoder::new(Cursor::new(bytes))?);
    controls.set_metadata(...);
    // 100ms 轮询 control_rx + sink.empty()
}
```

---

## ADR-026: message_start 携带 usage + 上下文超限预检

**日期**: 2026-08-08  
**状态**: 已接受  
**延伸**: ADR-022（自适应超时 + 请求体放宽）

### 背景

长对话场景下客户端（Claude Code）依赖 `message_start` 的 `input_tokens` 判断上下文占用与触发 AutoCompact。网关此前在 IR 渲染端**硬编码 0**：`est_input` 虽已按输入规模估算（handler.rs 注释「translation 路径 message_start 占位用」），但从未真正渲染进 `message_start`——passthrough 路径上游真实 usage 也被丢弃。客户端上下文条失真，AutoCompact 决策缺第一手信息。

另一侧：超长请求（估算输入 > 模型 `context_window`）直接发往上游，白等一轮（可能数分钟）后拿到超限错误，再经 SSE error 事件返回——用户看到「API returned an empty or malformed response (HTTP 200)」的**后半截成因**。

### 决策

1. **IR 层 `MessageStart` 携带 `usage: Option<IrUsage>`**：`from_messages` 带上游真实值（input + cache_read），`from_chat_completions` / `from_responses` 带估算占位（usage 在流末尾才给出）；渲染端（`to_messages` / `to_responses`）输出真实值，缺失时兜底 0
2. **上下文超限预检**：`route.rs` 的 `ResolvedRoute` 增补 `context_window`（读 `models.context_window` 列），`stream.rs` 路由解析后比对 `est_input > context_window` → 400 `invalid_request_error`（不发上游）
3. **README 交代仅流式语义**：客户端回退非流式重试会看到该报错，属设计使然

### 原因

1. **usage 是网关能提供给客户端的最关键决策输入**：AutoCompact / 上下文条 / prompt-cache 显示全部依赖它。估算口径（chars/4）保守，真实 token 通常 ≤ 估算，误杀概率低
2. **预检放同步段**：路由解析（~1ms）后立即判断，与 WebSearch 劫持同位置；成本是一次内存比较
3. **上游真实值优先**：passthrough 路径 `message_start` 自带真实 usage，直接透传比估算更准

### 代价

- 估算与真实值有偏差（尤其多模态 base64 图片，字符数高估 token），预检可能误拒「实际不超限」的请求——可接受，客户端重试成本低
- `MessageStart` 事件签名变更，影响 5 处测试构造点（已同步更新）

### 实现

```rust
// ir/types.rs
MessageStart { id, model, usage: Option<IrUsage> }

// route.rs
ResolvedRoute { ..., context_window: usize }

// stream.rs（路由解析后）
if max_input > 0 && est_input as usize > max_input {
    warn!(est_input, max_input, "context exceeds window, forwarding to upstream");
    // 不再返回 400，仅记录 warn 日志
}
```

---

## ADR-027: 引入 IR 中间表示层统一三协议转换

**日期**: 2026-08-09  
**状态**: 已接受

### 背景

项目最初只有 Anthropic Messages ↔ OpenAI Chat Completions 双向转换，实现在 `api/proxy/translate/` 目录（`to_openai.rs` / `to_anthropic.rs` / `common.rs`）。随着 OpenAI Responses API 支持需求出现，三协议互转的组合爆炸问题凸显：N 个协议两两转换需要 N×(N-1) 个转换模块，且每新增一个协议都要修改所有既有转换模块。

### 决策

1. **新建 `api/proxy/ir/` 模块**（Intermediate Representation，中间表示层），替代原 `api/proxy/translate/` 目录
2. **IR 以 Anthropic Messages 为骨架**：`IrContentBlock` 覆盖 Text/Image/Thinking/ToolUse/ToolResult 五种内容块（Anthropic 最丰富），并集覆盖三种格式的全部字段
3. **单向转换取代双向转换**：所有客户端格式 → IR（`from_messages.rs` / `from_chat_completions.rs` / `from_responses.rs`），IR → 所有客户端格式（`to_messages.rs` / `to_chat_completions.rs` / `to_responses.rs`）
4. **`IrStreamEvent` 6 种变体**：MessageStart → ContentBlockStart → ContentBlockDelta → ContentBlockStop → MessageDelta → MessageStop，覆盖所有协议的流式事件
5. **`IrUsage` 统一 token 统计**：input_tokens / output_tokens / cache_read_input_tokens / cache_creation_input_tokens / output_chars，所有协议共用同一结构
6. **删除 `api/proxy/translate/` 目录**：所有协议转换逻辑迁移到 `api/proxy/ir/`

### 原因

1. **组合爆炸收敛**：N 个协议只需 2N 个转换模块（N 个 `from_*` + N 个 `to_*`），而非 N×(N-1) 个双向模块
2. **内部工具解耦**：websearch 劫持、usage 追踪、错误构造等内部工具只操作 IR 类型，与具体协议无关
3. **扩展性**：新增协议只需新增一对 `from_*` / `to_*` 模块，无需修改既有转换
4. **可维护性**：IR 作为单一事实来源，避免多协议转换中的语义漂移

### 代价

- 转换路径变长：客户端格式 → IR → 客户端格式（两步），而非直接转换（一步）；但性能影响可忽略（微秒级）
- IR 类型定义需要并集覆盖所有协议字段，部分字段在特定协议中无意义（如 `IrThinkingConfig` 在 OpenAI 中不使用）

---

## ADR-028: WebSearch 劫持重构为本地搜索 + IR 注入

**日期**: 2026-08-09  
**状态**: 已被取代  
**取代**: ADR-012（WebSearch 劫持使用本地 Bing 搜索，tool-calling loop 模式）  
**被取代**: ADR-030（「清除 tools 后注入 IR system」方案在实测中不足，恢复 tool-calling loop + 无进展检测）

### 背景

ADR-012 的 WebSearch 劫持实现采用 tool-calling loop 模式：拦截包含 `web_search` 工具的请求 → 发送到上游（stream=false）→ 上游返回 tool_use → 本地 Bing 搜索 → 构造 tool_result → 追加到 messages → 继续下一轮（最多 5 轮）。问题：

1. **多一轮上游调用**：每轮都需要发送请求到上游并等待非流式响应，延迟累积
2. **key failover 不支持**：tool-calling loop 绕过了 `proxy_stream` 的双层重试循环，key 故障时无法自动切换
3. **Bing 搜索代理问题**：cn.bing.com 走代理会导致出口 IP 在海外，Bing 降级为"热门站点推荐"模式（返回今日头条/百度热搜等非相关结果）
4. **Cookie 污染**：全局 cookie 累积导致搜索结果质量下降

### 决策

1. **跳过 tool-calling loop**：代理自身提取搜索关键词（取最后一条 user 消息文本），本地 Bing 搜索后将结果作为 system block 注入 IR，清除 tools/tool_choice，交回 `proxy_stream` 正常流式转发给上游 LLM
2. **`enrich_ir_with_search` 函数**：替代原 `run_websearch_loop`，接收 `IrRequest`，返回修改后的 `IrRequest`（搜索结果注入 system + tools 清除）
3. **Bing 搜索绕过代理直连**：`search/bing.rs` 的 `build_http_client()` 传入 `no_proxy: true`，cn.bing.com 是国内站点，走代理反而导致结果降级
4. **独立 cookie 会话**：每次搜索创建新的 `reqwest::Client` + `cookie_store(true)`，避免全局 cookie 累积污染搜索结果
5. **key failover 由 proxy_stream 天然支持**：搜索结果注入 IR 后，请求走正常的流式转发路径，双层重试循环、key 轮换、故障转移全部生效

### 原因

1. **省掉一轮上游调用**：不再需要发送非流式请求到上游等待 tool_use，直接本地搜索 + 注入 IR + 流式转发
2. **key failover 支持**：请求走 `proxy_stream` 正常路径，双层重试循环天然支持 key 故障切换
3. **Bing 搜索质量提升**：绕过代理直连 cn.bing.com，避免出口 IP 在海外导致结果降级；独立 cookie 会话避免累积污染
4. **代码简化**：`websearch.rs` 从 706 行减至 115 行，删除 tool-calling loop、SSE 转换、多轮状态管理等复杂逻辑

### 代价

- 搜索结果以 system block 形式注入，而非 tool_result 形式；LLM 对搜索结果的引用方式可能略有差异（但实测影响可忽略）
- 每次搜索创建新的 `reqwest::Client`，无法复用 TCP 连接池（但搜索频率低，影响可忽略）

---

## ADR-029: usage 真实值覆盖估算占位 + Responses 增量口径

**日期**: 2026-08-09  
**状态**: 已接受

### 背景

`forward.rs` 在流式转发开始时预填估算的 `input_tokens`（基于 `chars/4`），供客户端上下文条感知。后续上游返回真实 usage 时，原实现采用 `max()` 合并策略：

```rust
state.usage.input_tokens = state.usage.input_tokens.max(usage.input_tokens);
```

问题：

1. **估算值偏大**：`chars/4` 是保守估算（中文/代码实际 token 数通常低于估算），`max()` 合并导致估算值永久压住真实值
2. **usage_log 污染**：写入数据库的 `prompt_tokens` 是偏大的估算值，而非真实值，统计数据失真
3. **客户端上下文条虚高**：客户端收到的 `input_tokens` 偏大，上下文条显示不准确
4. **Responses API 口径不一致**：Responses API 的 `input_tokens` 包含缓存命中部分，而 Chat Completions 的 `prompt_tokens` 不包含（已减去 `cached_tokens`），导致两种协议的 usage 口径不一致

### 决策

1. **真实值覆盖估算占位**：上游返回真实 usage 时，直接覆盖估算值（不用 `max()`）——估算值是占位符，真实值到位后即替换
2. **Responses input_tokens 增量口径**：从 `response.usage` 提取 `input_tokens` 后，减去 `input_tokens_details.cached_tokens`，保持增量口径（与 Chat Completions `prompt_tokens - cached_tokens` 一致）
3. **message_delta 补全 input_tokens**：IR → Messages 渲染时，`message_delta.usage` 补上 `input_tokens`（此前缺失，只输出 `output_tokens` 和 `cache_read_input_tokens`）
4. **上下文超限预警（软警告）**：`stream.rs` 检测到上下文超限时，仅记录 warn 日志，不返回 400 错误——避免阻断客户端 auto-compact（`/compact` 自身也需走代理，硬拒绝会形成死锁）

### 原因

1. **估算值是占位符**：`chars/4` 是保守估算，真实 token 数通常 ≤ 估算；`max()` 合并导致估算值永久压住真实值，违背占位符语义
2. **usage_log 准确性**：写入数据库的 `prompt_tokens` 应该是真实值，而非偏大的估算值
3. **客户端上下文条准确性**：客户端收到的 `input_tokens` 应该是真实值，上下文条显示才准确
4. **口径一致性**：Responses 和 Chat Completions 的 `input_tokens` 应该采用相同的增量口径（减去缓存命中部分），便于跨协议统计
5. **避免 auto-compact 死锁**：上下文超限时硬拒绝（400）会阻断客户端 auto-compact（`/compact` 自身也需走代理），形成死锁；软警告让请求继续，由上游返回准确错误，客户端可据此 auto-compact

### 代价

- 估算值与真实值可能有短暂不一致窗口（估算值先输出，真实值后覆盖），但流式响应中客户端会收到多次 usage 更新，最终以真实值为准
- 上下文超限时不再提前拦截，可能浪费一次上游调用（但上游会返回准确错误，客户端可据此 auto-compact，代价可接受）

### 实现

```rust
// from_chat_completions.rs / from_responses.rs
if usage.input_tokens > 0 {
    state.usage.input_tokens = usage.input_tokens;  // 覆盖，不用 max()
}

// usage.rs (extract_responses_usage)
let cached = usage.input_tokens_details.cached_tokens.unwrap_or(0);
usage.input_tokens = usage.input_tokens.saturating_sub(cached);  // 增量口径

// to_messages.rs
MessageDelta {
    usage: IrUsage {
        input_tokens: state.usage.input_tokens,  // 补全（此前缺失）
        output_tokens: state.usage.output_tokens,
        ..
    },
}

// stream.rs
if max_input > 0 && est_input as usize > max_input {
    warn!(est_input, max_input, "context exceeds window, forwarding to upstream");
    // 不再返回 400
}
```

## ADR-030: WebSearch 工具调用循环恢复 + 轮数安全网与无进展检测

**日期**: 2026-08-11
**状态**: 已接受
**取代**: ADR-028 的部分决策（「清除 tools 后注入 IR system」方案在实测中不足）

### 背景

ADR-028 把 WebSearch 从 tool-calling loop 改为「本地搜索 + IR system 注入」。实测发现两个问题：

1. **注入式搜索无法应对多轮探索**：模型对争议性问题（如「张雪峰是否去世」）需要多次调整查询词交叉验证，注入式方案只做一次搜索，信息不足
2. **tool-calling loop 有死循环风险**：恢复 loop 模式后，若模型反复搜索（Bing 持续返回降级/矛盾信息时常见），3 轮硬上限会截断搜索不充分；但完全无上限会死循环，客户端永远等不到回答

### 决策

1. **恢复 tool-calling loop**（`execute_websearch_tool_loop`）：模型通过标准 tool-calling 自主决定搜索次数、查询词、何时收尾（`tool_choice = Auto`）
2. **轮数上限作为安全网而非默认路径**：`MAX_TOOL_ROUNDS = 10`，正常模型搜索几次就 `end_turn`，上限只防失控
3. **无进展检测**：连续 2 轮查询词相似（Levenshtein 编辑距离归一化 ≥ 0.6）→ 提前收尾。模型反复搜同一关键词通常意味着搜索结果无新信息（Bing 降级页症状），继续搜只是白耗时间
4. **耗尽/无进展收尾**：收集全部搜索结果文本 → 移除 messages 中的 tool_use/tool_result 痕迹 → 移除 web_search 工具 + `tool_choice = None` → 搜索结果合并为纯文本指令追加 → 强制一轮无搜索回答
   - **关键**：不清理工具痕迹的话，`to_chat_completions` 会从历史 tool_calls 补回 web_search 工具定义，上游仍能继续调用，最终轮永远返回 tool_use 而非文本（实测死循环 bug）
5. **IPC 远程域授权**：隐藏 WebView 搜索（bing.com 远程域）需在 capabilities 配置 `remote` context + app 命令权限（见 `capabilities/webview-search.json`），否则 Tauri 2 默认拒绝远程域调用 Rust 命令，搜索回传超时

### 代价

- 多轮 loop 期间客户端全程缓冲，延迟随轮数增加（每轮 ~10-20s）
- 查询相似度阈值（0.6）可能误判（部分重叠的查询被当作重复），但收尾逻辑保证结果仍可用
- Bing 降级页问题仍在（热点人物多词查询返回字典释义），靠模型多轮换查询缓解，非本 ADR 范围

### 实现位置

- `src-tauri/src/api/proxy/websearch.rs` — `MAX_TOOL_ROUNDS` / `NO_PROGRESS_ROUNDS` / `QUERY_SIMILARITY_THRESHOLD` / `query_similarity()` / 收尾清理
- `src-tauri/capabilities/webview-search.json` — 隐藏 WebView 远程域 IPC 授权（已废弃：ADR-031 的 HTTP 浏览器头方案取代了 WebView 方案）
- `src-tauri/build.rs` — `AppManifest::commands` 声明 app 命令权限（`search_result_callback` 等，已废弃）

---

## ADR-031: Bing 搜索升级为 HTTP 浏览器头策略 + 双域名 fallback

**日期**: 2026-08-11  
**状态**: 已接受  
**关联**: ADR-030（WebSearch tool-calling loop 恢复）

### 背景

ADR-028 时期 Bing 搜索使用 `http::build_http_client()` + `no_proxy: true` 直连 cn.bing.com。实测两个问题：

1. **代理出口 IP 降级**：Bing 对代理出口 IP（通常在海外）返回「热门站点推荐」模式结果（今日头条/百度热搜），而非正常搜索结果
2. **裸 HTTP 请求降级**：cn.bing.com 把没有浏览器特征头的请求识别为非浏览器请求，对中文查询返回字典释义页（实测查「张雪峰 2026」只返回「张」字的字典释义）

曾尝试用隐藏 WebView（WKWebView）执行搜索并 JS 回传 HTML，但实测发现**关键在完整浏览器头**（尤其 `sec-ch-ua` 系列 + UA + Accept-Language），而非 TLS 指纹或 JS 执行——reqwest 带完整浏览器头 + `cookie_store(true)` + 预热即可拿到与 WebView 完全相同的正常结果。WebView 方案被废弃，`capabilities/webview-search.json` 不再需要。

### 决策

1. **SearchHttp 专用结构体**（`search/bing.rs`）：全局复用的搜索 HTTP 客户端，包含：
   - 完整浏览器头：`User-Agent`（Chrome 131 on macOS）、`Accept`（含 image/avif）、`Accept-Language`（zh-CN 优先）、`Upgrade-Insecure-Requests`、`sec-ch-ua` 系列 Client Hints
   - `cookie_store(true)`：cookie 会话持续复用，后续搜索不再降级
   - `prewarmed: AtomicBool` + `prewarm_lock: Mutex<()>`：懒预热（首次搜索前 GET 主页建 cookie），并发首搜只预热一次
   - **不走** `http::build_http_client()`——搜索必须直连，系统代理会触发 Bing 降级

2. **双域名 fallback 策略**：`www.bing.com`（国际版，质量更高）优先，空壳/失败/降级时 fallback `cn.bing.com`

3. **降级检测 + 简化重试**：
   - `is_degraded_results()`：结果标题/摘要中 < 30% 包含查询首词 → 判定降级（字典释义页特征）
   - `simplify_query()`：降级时取查询首词重搜（实测「张雪峰 高考志愿」降级，「张雪峰」单搜正常）

4. **ck/a 重定向解码**：Bing 结果链接可能是 `www.bing.com/ck/a?u=a1<base64url>`，`decode_ck_href()` 用 base64url 解码还原真实 URL（参考 SearXNG bing.py）

5. **AppState 集成**：`server.rs` 新增 `search_http: SearchHttp` 字段，`websearch.rs` 通过 `state.search_http` 全局复用

### 原因

1. **浏览器头而非 WebView**：Bing 风控靠请求头特征识别浏览器会话，`sec-ch-ua` Client Hints 是关键信号；reqwest + 完整头即可模拟，无需引入 WebView 的 IPC 复杂度
2. **cookie 会话复用**：首次预热后 cookie 持续有效，后续搜索不降级；独立 cookie 会话（ADR-028）会导致每次都重新预热
3. **双域名 fallback**：www.bing.com 国际版结果质量更高但可能返回空壳页（代理环境），cn.bing.com 更稳定但质量略低——两者互补
4. **降级检测兜底**：Bing 对热点人名+附加词的风控不可预测，检测+简化重试覆盖了最常见的降级场景
5. **SearchHttp 全局复用**：TCP 连接池 + cookie 会话双重收益，搜索频率低但每次复用价值高

### 代价

- SearchHttp 不走统一工厂，是 `http.rs` 规则的唯一例外——但 Bing 对代理出口 IP 的降级是硬约束，不可调和
- www.bing.com 优先策略增加一次额外请求（空壳时 fallback cn），但预热后 cookie 复用使得两次请求都很快
- 降级检测阈值（30% 首词命中率）是经验值，可能误判正常结果（但简化重试兜底了大多数情况）

### 实现位置

- `src-tauri/src/search/bing.rs` — `SearchHttp` / `search()` / `search_domain()` / `is_degraded_results()` / `simplify_query()` / `decode_ck_href()` / `parse_results()`（~465 行）
- `src-tauri/src/gateway/server.rs` — `AppState.search_http: SearchHttp`

---

## ADR-032: macOS 系统代理自动检测（scutil --proxy）

**日期**: 2026-08-11  
**状态**: 已接受  
**延伸**: ADR-016（统一 HTTP 客户端工厂 + 系统代理自动继承）

### 背景

ADR-016 的 `http.rs` 统一工厂支持环境变量 + Windows 注册表两种代理来源，但 macOS 用户通过「系统设置 → 网络 → Wi-Fi → 代理」配置的代理既不写环境变量也不写注册表，导致 Clash 等代理工具的 macOS 用户必须手动设置 `HTTPS_PROXY` 环境变量。

### 决策

1. **`resolve_macos_proxy()` 函数**：调用 `scutil --proxy`，解析输出中的 `HTTPEnable` / `HTTPProxy` / `HTTPPort`（或 `HTTPSEnable` / `HTTPSProxy` / `HTTPSPort`）
2. **跨平台代理解析链**：环境变量（全平台）→ Windows 注册表（`#[cfg(windows)]`）→ macOS scutil（`#[cfg(target_os = "macos")]`）
3. **scutil 而非 networksetup**：`networksetup -getwebproxy` 需要指定网络接口名（Wi-Fi / Ethernet），猜错就返回 None；`scutil --proxy` 输出**当前生效**的代理配置（系统按优先级自动选择接口），无需猜接口名

### 原因

1. **零配置体验**：macOS 用户在系统设置里配好 Clash 代理后，xrl-router 自动继承，无需额外设置环境变量
2. **与 ADR-016 哲学一致**：OnceLock 缓存 + 跨平台 fallback 链，只多一个 `#[cfg]` 分支
3. **scutil 是 macOS 标准工具**：系统自带，输出格式稳定，无需额外依赖

### 代价

- `scutil --proxy` 是子进程调用（~30ms），但 OnceLock 缓存后只调一次
- 仅支持 HTTP/HTTPS 代理，不支持 SOCKS 或 PAC（AutoConfigURL）——与 Windows 分支限制一致

### 实现

```rust
// http.rs
#[cfg(target_os = "macos")]
fn resolve_macos_proxy() -> Option<String> {
    let output = std::process::Command::new("scutil")
        .arg("--proxy")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // 解析 HTTPEnable=1 + HTTPProxy + HTTPPort
    // 或 HTTPSEnable=1 + HTTPSProxy + HTTPSPort
    // 返回 "http://proxy:port" 格式
}
```

---

## ADR-033: Install 页面从编译时静态 HTML 迁移为 Vue SPA + tower-http ServeDir

**日期**: 2026-08-11  
**状态**: 已接受  
**关联**: ADR-017（单 listener + 路径级 IP 限制）、ADR-018（国际化）

### 背景

原 install 页面（`src-tauri/assets/install.html`，370 行内联 JS）通过 `include_str!` 编译进二进制，`handlers/install.rs` 的 `serve_install_page()` 返回 `Html<&'static str>`。这种"单文件自包含"方案在早期简单直接，但随着需求膨胀暴露了问题：

1. **维护割裂**：页面逻辑用原生 JS 写在 HTML 里，无法复用前端的 Vue 组件（MD3 design tokens、MdiIcon、i18n 语言包）、无法访问 Pinia stores、无法共享 `api.ts` 的请求封装
2. **视觉不一致**：静态页内联 CSS 变量与前端 `global.css` 的 MD3 tokens 是两套独立定义，暗色模式下色差明显；语言切换也是页面内联字典，与前端 `src/i18n/` 的语言包重复维护
3. **功能受限**：无法使用 Vue Router 的导航守卫、无法接入 WebSocket 实时推送、无法共享主题色相（hue）设置——LAN 设备看到的 install 页面与主机应用风格完全脱节
4. **扩展困难**：新增消费端（ChatGPT/Codex）需要在纯 JS 里写复杂的表单交互和命令生成逻辑，继续堆砌内联代码

### 决策

1. **删除 `assets/install.html`**：移除编译进二进制的静态页面，`handlers/install.rs` 的 `serve_install_page()` 一并删除（仅保留 `get_local_ip()`）
2. **新增 `src/views/InstallView.vue`**：Vue SPA 组件，注册在 Vue Router 的 `/install` 路由，复用 MD3 组件（`md-outlined-segmented-button-set`、`md-outlined-select`、`md-circular-progress`）、MdiIcon、i18n 语言包
3. **后端 SPA fallback**：`api/router.rs` 新增 `spa_fallback()` + `tower_http::ServeDir`：
   - `/assets/*` → `ServeDir` 托管前端构建产物（`dist/assets/`）
   - 所有未匹配 axum 路由的 GET 请求 → fallback 返回 `dist/index.html`
   - Vue Router 接管 `/install` 路由，渲染 InstallView 组件
4. **新增 `/api/ui-settings` 公开端点**：返回管理端的 `theme`/`hue`/`locale` 设置，LAN install 页面加载时读取并应用，保持与主机应用一致的视觉风格
5. **UI 设置后端持久化**：`settings` 表新增 `theme`/`hue`/`locale` 键，前端 `theme.ts` 和 `i18n/index.ts` 在主题/语言切换时通过 `PUT /api/settings` 同步到后端
6. **动态 BASE_URL**：`src/api.ts` 的 `getBaseUrl()` 按 hostname 判断——Tauri/localhost 用 `http://127.0.0.1:19068`，LAN 浏览器用当前 origin（避免 CORS）。**用 `127.0.0.1` 而非 `localhost`**：Windows 上 `localhost` 优先解析为 `::1`（后端只绑 IPv4），且代理工具（Clash 等）的 bypass 规则通常覆盖 `127.0.0.1` 而未必覆盖 `localhost`——后者被代理劫持时响应是 HTML，前端 `JSON.parse` 会报 `Unexpected token '<'`。**必须同时命中 `tauri.localhost` hostname**：Tauri 2 在 Windows 生产模式页面地址是 `http://tauri.localhost`（macOS/Linux 是 `tauri://localhost` 协议），漏掉它 BASE_URL 会拼成 `http://tauri.localhost`，请求打到 asset protocol 返回 index.html（200 HTML），表现为「构建版保存 Provider 报 Unexpected token '<'，dev 模式正常」
7. **非 Tauri 兼容**：前端代码（`App.vue`、`theme.ts`、`fm/player.ts`）通过动态 `import()` 延迟加载 Tauri API（`@tauri-apps/api/*`），LAN 浏览器访问时不触发 Tauri 依赖报错
8. **多消费端支持**：InstallView 新增消费端选择（Claude Code / ChatGPT），按平台生成不同命令
9. **后端双栈监听**：`start_gateway` 对通配 host 用 socket2 绑 `[::]` + `IPV6_V6ONLY=false`（IPv6 不可用回退 `0.0.0.0`）——localhost 无论解析成 `::1` 还是 `127.0.0.1` 都能连上，不再依赖客户端 hostname 选择。**连带修正**：`admin_ip_guard` 的 loopback 判断需 `to_canonical()` 兼容 IPv4-mapped（`::ffff:127.0.0.1`），否则双栈下本机管理请求被误拒 403（纯 `::1` 由 `is_loopback()` 原生覆盖）

### 原因

1. **统一技术栈**：install 页面用 Vue 组件 + MD3 设计系统，与前端其他页面风格一致；i18n 复用同一套语言包，不再维护两套翻译
2. **视觉同步**：`/api/ui-settings` 让 LAN 页面自动继承主机应用的主题色、令牌色和语言设置，用户感知是"同一个应用"
3. **维护成本降低**：删除 370 行内联 JS + CSS，InstallView 用 Vue 模板 + TypeScript 实现，可复用现有组件和工具函数
4. **扩展性提升**：新增消费端只需在 InstallView 里加一个 `buildXxxCommand()` 函数 + 分段按钮，不需要在纯 JS 里手撸 DOM 操作
5. **ServeDir + SPA fallback 是标准模式**：axum + tower-http 的成熟方案，`DIST_DIR` 环境变量支持开发/生产环境切换；fallback 到 `index.html` 是所有 SPA 框架的标准做法

### 代价

- **二进制不再包含 install 页面**：`include_str!` 的零运行时依赖优势丢失，需要 `dist/` 目录存在才能访问 install 页面（开发环境 `pnpm dev` 自动构建，生产环境 `pnpm build` 输出到 `dist/`）
- **`/api/ui-settings` 公开暴露**：主题/语言设置不含敏感信息，但增加了一个公开端点——可接受，因为管理端点仍受 `admin_ip_guard` 保护
- **冒烟测试调整**：原测试断言 `/install` 返回包含"客户分发 / Client Deploy"的 HTML，改为断言 200 或 404（测试环境无 `dist/` 目录时 fallback 返回 404）

### 文件变更

| 文件 | 变化 |
|------|------|
| `src-tauri/assets/install.html` | **删除**（370 行内联 JS + CSS） |
| `src-tauri/src/api/handlers/install.rs` | 删除 `serve_install_page()`，仅保留 `get_local_ip()`（增加返回 `port`） |
| `src-tauri/src/api/handlers/stats.rs` | 新增 `get_ui_settings()`（公开端点）+ `get_settings()`/`update_settings()` 增加 `theme`/`hue`/`locale` 字段 |
| `src-tauri/src/api/router.rs` | 新增 `spa_fallback()`、`ServeDir` 托管 `/assets/*`、删除 `/install` 路由、新增 `/api/ui-settings` |
| `src/views/InstallView.vue` | **新建**：Vue SPA 组件（消费端选择 + 模型下拉 + 命令生成） |
| `src/api.ts` | 动态 `BASE_URL`（`getBaseUrl()`）+ 新增 `uiSettingsApi` + `settingsApi` 增加 `theme`/`hue`/`locale` + `installApi.localIp()` 返回 `port` |
| `src/theme.ts` | Tauri API 动态导入 + `syncThemeToBackend()` / `syncHueToBackend()` |
| `src/i18n/index.ts` | `syncLocaleToBackend()` 语言切换同步到后端 settings 表 |
| `src/App.vue` | `/install` 路由时隐藏 AppShell + ConnectionStatus + Tauri API 动态导入 |
| `src/fm/player.ts` | Tauri API 动态导入（`invoke`/`listen`） |
| `src/views/KeysView.vue` | `localPort` 从 `installApi.localIp()` 动态获取（不再硬编码 `19069`） |
| `src-tauri/Cargo.toml` | `tower-http` 增加 `fs` feature（`ServeDir` 依赖） |

---

## ADR-034: 上游 200 + SSE error event 视为密钥级错误换密钥重试

**日期**: 2026-08-21  
**状态**: 已接受  
**关联**: ADR-003（密钥健康状态）、ADR-027（IR 中间表示层）、ADR-030（WebSearch 工具调用循环）

### 背景

部分上游（API 中转站/解析器）在密钥欠费/限流时，不返回 400/402/429 等 HTTP 错误码，而是返回 **HTTP 200 + SSE 流，流中只有 error 事件**，例如 Anthropic 格式的 `{"type":"error","error":{"type":"insufficient_quota",...}}`。三个 IR 解析器（`messages_chunk_to_ir` / `chat_completions_chunk_to_ir` / `responses_chunk_to_ir`）都会把这类事件解析为 **0 个 IR 事件**，客户端收到的是空流：

> API Error: API returned an empty or malformed response (HTTP 200) ... 0 stream events received

HTTP 级密钥轮换（400 quota / 401/402/403/429）已在双循环中实现，但 200 + 流内错误完全绕过这些检查，错误直接透传给客户端，密钥不轮换——欠费密钥会持续被打。

### 决策

1. **流内错误检测**：`forward.rs` 新增 `extract_stream_error()`——按 provider_kind 检测三种格式的 error 事件（messages `type=="error"`、responses `response.failed`/`error`、chat 顶层 `error` 对象），`classify_error()` 按关键词推断 HTTP 语义：429（rate_limit 等）、402（quota/insufficient/billing/credit 等）、401（authentication/invalid_api_key 等）、403（permission/forbidden 等），不命中为 400
2. **转发函数返回 `ForwardOutcome`**：`forward_stream_ir` 返回 `Completed` / `UpstreamKeyError { status, message }`（密钥级错误且未向客户端发送任何内容）/ `ErrorDelivered`（已透传）。密钥级错误由双循环 `continue` 换下一把密钥重试，全部耗尽才透传（新增 `ProviderFailure::StreamError` 兜底）
3. **200 + 非 SSE 纯 JSON 错误体**（无 `\n\n` 帧）同样检测：仅当「流结束且从未出现任何 SSE 帧（chunk_count==0）+ 原始体含顶层 error 对象」时触发；内容审核空流（`{"choices":[...finish_reason":"content_filter"]}`）无 error 键，不触发，零误伤
4. **WebSearch 路径同步**：`execute_websearch_tool_loop` 返回 `ForwardOutcome`，round 0 缓冲发现密钥级错误（此时未发送任何内容）→ 返回给双循环换密钥重试；后续轮次（已发 brand/进度）→ 透传

### 原因

1. **欠费/限流的表达方式不可靠**：上游中转站在密钥欠费时可能 200 + error 流、400 + quota body、429 三种形态混用，HTTP 级检测无法覆盖 200 分支
2. **客户端无法消化空流**：Claude Code 对 200 + 0 事件的响应报 "empty or malformed response"，透传等于把噪声丢给客户端，且不修复密钥健康度
3. **复用既有语义**：推断的 401/402/403/429 直接喂给 `update_key_health`（红/黄），与 HTTP 级轮换完全一致，无新健康度分支
4. **顺带修复双循环 last-wins 缺陷**：原 2xx 分支 `break` 只退出内层循环，failover 开启多 provider 候选时会继续向后续 provider 发多余请求（可能覆盖 winner、上游双重计费）。转发移入 2xx 分支后改为 `break 'provider`（first-wins），单候选场景行为无差异

### 代价

- **转发调用移入双循环**：`forward_stream_ir` 的调用点从「循环后统一转发」变为「2xx 分支内转发」，循环后段删除 `response.unwrap()` 等 dead code——代码结构变化较大，但行为对单 provider（默认）场景不变
- **WebSearch 路径请求体 clone**：`execute_websearch_tool_loop` 按值取走并变异 `ir_request`，重试需要原始体，调用处 `clone()` 一次（每 attempt 一次，成本可忽略）
- **预存缺口（未修复）**：WebSearch 路径 HTTP 级密钥错误（`send_upstream_request` 非 2xx）仍不轮换；WebSearch 模式下双循环首个请求是纯浪费（循环内部另发请求）——记录为后续项

### 重新考虑的条件

- 上游修复返回正确的 HTTP 状态码后，可收紧检测（但关键词分类在可预见的未来仍然适用）
- 若出现「200 + error 流但换密钥无济于事」的上游（如账号级限额），可考虑加冷却/熔断，而不是简单轮换

---

## ADR-035: WebSearch/WebFetch 从 server-side 劫持迁移到本地 MCP 端点

**日期**: 2026-08-24  
**状态**: 已接受  
**取代**: ADR-028、ADR-030 的劫持循环方案（Bing HTTP 策略本身保留，见 ADR-031）  
**关联**: ADR-031（Bing HTTP 浏览器头策略）、ADR-017（单 listener 架构）

### 背景

旧方案（ADR-028/030）由代理跑 server-side 劫持循环：注入 `web_search` 工具 → 缓冲所有中间搜索轮次 → 本地 Bing 搜索回传 → 收尾合成响应。实测体验与工程问题：

1. **体验差**：所有中间轮全缓冲，客户端在搜索期间看不到任何进展，首字节延迟 = 全部搜索轮次 + 最终回答的总时长。
2. **代码复杂脆弱**：~1650 行循环 + 收尾逻辑（轮数上限、无进展检测、工具痕迹清理、`to_chat_completions` 回填 bug 的规避），还有一整套 Claude Code 搜索卡片合成（`server_tool_use` / `web_search_tool_result`）——只有 Messages 客户端能享受卡片，Chat/Responses 客户端只有「缓冲 + 最终回答」。
3. **前置条件苛刻**：Claude Code 第三方 base_url 默认禁用 WebSearch，必须靠 `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL=true` 强制开启，配置负担转嫁给主人。

### 决策

1. **删除劫持循环**，改为网关内置 **MCP（Streamable HTTP）端点 `/mcp`**，提供 `web_search`（复用 ADR-031 的 Bing HTTP 搜索）与 `web_fetch`（见 ADR-037）两个工具。客户端注册该 MCP 后，模型通过标准 MCP tool-calling 直接调用，工具调用过程对客户端完全可见。
2. **开关语义替换**：`websearch_hijack` → `mcp_websearch`（V16 迁移值）。ON = `/mcp` 提供 `web_search` + 代理剔除请求自带搜索工具（防上游官方搜索生效）；新增 `mcp_webfetch` 控制 `web_fetch`。OFF = 完全不碰工具定义。
3. **鉴权复用 Service Key**（`Authorization: Bearer`，argon2），安全模型与 `/v1/*` 一致；端点挂公开区（客户端可能在局域网）。
4. 代理侧只保留「剔除」逻辑（`strip_search_tools`，含 `tool_choice` → Auto 改写），不再注入工具、不再跑循环、不再合成卡片。

### 原因

1. **标准协议 > 私有序列**：MCP 是客户端原生支持的协议，工具调用可见、可审批、可组合；劫持循环是黑盒，客户端只能看到一段被合成的流。
2. **性能**：模型直接调用工具，无中间轮缓冲；每个工具调用独立往返，延迟摊薄。
3. **删除大于新增**：净删 ~1600 行（`websearch.rs` 全删 + `forward_stream_ir_to_buffer` + `accumulate_ir_events` + 卡片渲染），新增 ~600 行（`mcp/` 三文件）。
4. **开关全关时协议仍健康**：`tools/list` 返回空数组而非 404，已注册客户端不断连。

### 代价

- 客户端需一次性注册 MCP（设置页提供可复制的 `claude mcp add` 命令），之后一劳永逸。
- 不再合成 Claude Code 搜索卡片——但 MCP 工具调用本身在客户端有原生展示。
- `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL` 前置条件不再需要（客户端自己的 WebSearch 工具与 MCP 并存，开关开启时代理剔除前者）。

### 重新考虑的条件

- 若主流客户端停止支持 Streamable HTTP MCP（可能性极低），再评估退回或换传输。

---

## ADR-036: MCP 端点用 rmcp handle() 嵌入 axum 0.7（不升级 0.8）

**日期**: 2026-08-24  
**状态**: 已接受  
**关联**: ADR-035、ADR-017（单 listener）

### 背景

rmcp（官方 Rust MCP SDK）的 `StreamableHttpService` 示例都基于 axum 0.8（`any_service` 挂载），而网关是 axum 0.7.9。升级整个网关的 axum 主版本只为挂一个端点，爆炸半径不值得。

### 决策

1. 用 `StreamableHttpService::handle<B>(&self, Request<B>) -> Response<BoxBody<Bytes, Infallible>>`——它接受**任意** `http_body::Body`、返回可转换的 `BoxBody`，与 axum 版本无关。
2. 在 axum 0.7 写一个普通 `any` handler：鉴权后调 `handle()`，`Response::from_parts(parts, Body::new(body))` 转回 axum Body。
3. 无会话模式：`NeverSessionManager` + `legacy_session_mode = false`——工具只读、无服务端推送，会话纯属开销。
4. 手写 `ServerHandler`（不用 `#[tool_router]` 宏）：`tools/list` 必须按运行时开关动态过滤，宏生成的静态列表做不到；且只有两个工具，手写最直接。
5. `ServerHandler` 深处拿不到 axum State → `mcp::init()` 启动时注入全局 `OnceLock<Arc<AppState>>`（Tauri 单实例，无风险）。

### 原因

1. `handle()` 是 rmcp 公开的稳定 API，泛型于请求体类型，天然适配任何 axum 版本——升级 0.8 的唯一动机（`any_service` 适配器）消失。
2. axum 0.7 → 0.8 涉及 handler 签名、中间件、extractor 多处变更，全网关回归成本高，与「单端点」收益严重不匹配。

### 代价

- 多一层手工 body 转换（~5 行）。
- 全局 OnceLock 注入是小小的不优雅，换取 handler 签名简单。

### 重新考虑的条件

- 若未来升级 axum 0.8（因其他原因），可改回 `any_service` 直接挂载。

---

## ADR-037: WebFetch 复用本机 Chrome/Edge headless 渲染（不自动下载）

**日期**: 2026-08-24  
**状态**: 已接受（渲染层已被 ADR-038 取代，见下）  
**关联**: ADR-035、ADR-016（统一 HTTP 客户端工厂）

### 背景

`web_fetch` 要求「执行页面 JS 后拿到完整内容」，纯静态抓取（reqwest）对 SPA/动态页面无效。需要一个真实的渲染引擎。

### 决策

1. **复用本机浏览器**：探测本地 Chrome/Edge（Windows 优先系统自带 Edge，其次 Chrome；含协议补全），`headless_chrome` crate 经 CDP 无头渲染（JS 执行），取渲染后 HTML → `htmd` 转 Markdown → 截断（约 60K 字符）。
2. **不自动下载浏览器**（不做 Chrome for Testing 下载兜底）：Windows 自带 Edge，探测命中率基本 100%；探测失败回退静态抓取（`http::build_http_client()`，继承系统代理）并在输出开头注明「可能不含 JS 渲染结果」。
3. **生命周期**：进程级懒启动 + 保活复用（`OnceLock<Mutex<Option<Browser>>>`），每次抓取新 Tab 用完即关；单用户场景 Mutex 串行（一次渲染一页）。
4. 同步 `headless_chrome` API 进 `spawn_blocking`（项目既有模式，同 rusqlite）。

### 原因

1. **零下载零磁盘负担**：自动下载 ~150MB Chrome 首次体验差、占磁盘、还要处理版本管理；本机浏览器探测几乎必然命中（目标平台是桌面开发者机器）。
2. **降级路径明确**：探测不到 → 静态抓取 + 明确告知，而非报错，工具仍可用。
3. **选 `headless_chrome` 而非 `chromiumoxide`**：LaunchOptions 的探测/保活模型成熟，同步 + `spawn_blocking` 与项目同步代码模式一致。

### 代价

- 渲染时占用本机浏览器资源（单页、串行，影响有限）。
- 极端环境（无任何 Chrome 系浏览器）退化为静态抓取，失去 JS 渲染能力——已明确告知。

### 重新考虑的条件

- 若出现大量「无本机浏览器」的目标环境，再评估 Chrome for Testing 自动下载兜底。

---

## ADR-038: WebFetch 渲染改用 Tauri 内置 WebView（移除 headless_chrome）

**日期**: 2026-08-24  
**状态**: 已接受  
**关联**: ADR-037（被取代）、ADR-035

### 背景

ADR-037 的「重新考虑条件」被触发：实际目标环境（macOS 开发机）无 Chrome/Edge/Chromium，`web_fetch` 退化为静态抓取，SPA/JS 页面拿不到渲染内容。且探测到的浏览器要额外拉起独立进程（CDP 启动 ~500ms + 常驻开销）。

### 决策

1. **渲染引擎改用 Tauri 内置 WebView**：懒创建隐藏窗口（label `fetcher`）→ `navigate` → 轮询 `eval_with_callback` 等 `readyState == "complete"` 且资源计数稳定 → eval 取渲染后 HTML → `htmd` 转 Markdown。
2. **删除 headless_chrome 依赖与浏览器探测代码**（探测候选列表、LaunchOptions、Tab 生命周期全删）。
3. **保留静态抓取回退**：WebView 创建失败/超时/eval 异常 → 静态抓取 + 开头注记「可能不含 JS 渲染结果」。

### 原因

1. **可用性 ≈ 应用可用性**：网关进程即 Tauri 进程（窗口关闭只隐藏到托盘），WebView 三端全覆盖（macOS WKWebView / Windows WebView2 / Linux WebKitGTK），不再依赖任何第三方浏览器安装。
2. **零额外进程**：无 CDP 启动开销与浏览器常驻，渲染就在应用内。
3. **安全模型不变**：隐藏窗口不给远程域配 IPC capability（无 `dangerousRemoteDomainIpcAccess`），HTML 提取纯 Rust 单向 eval——远程页面接触不到 Tauri IPC，与 ADR-031 废弃的 WebView 搜索方案（远程域需授权）完全不同。
4. **Windows 上 WebView2 本身就是 Chromium**，渲染行为与旧方案基本一致。

### 代价

- WKWebView/WebKitGTK 的 UA 与 Chrome 不同，个别站点渲染有差异（可接受，与静态回退同样在输出注记）。
- macOS 隐藏窗口存在 JS 定时器节流风险（实测后如受影响，改用 1×1 离屏可见窗口）。
- WebView2 `ExecuteScriptWithResult` 回传结果有体积限制 → JS 侧先截断（~1.5MB）再回传。
- 导航失败事件无法直接获取（WKWebView 不暴露给 eval）→ 以 readyState 停滞超时兜底，触发静态回退。

---

## ADR-039: 视觉模型经本地 MCP 工具外发（web_vision）

**日期**: 2026-08-24  
**状态**: 已接受  
**关联**: ADR-035（MCP 端点）、ADR-038（WebView 渲染层）

### 背景

对话主模型可能不具备视觉能力（或用户希望集中管理图片理解）。在既有 `/mcp` 端点（web_search / web_fetch）旁新增 `web_vision`：用户把某供应商的一个模型指定为「视觉专用模型」，其他模型经 MCP 传入图片 URL/本地路径，网关取图后调该模型生成描述文本。

### 决策

1. **单个视觉模型全局配置**：settings 键 `mcp_vision`（开关）+ `mcp_vision_provider` / `mcp_vision_model`（存上游真实 `model_id`，V17 迁移插默认行）。调用时实时解析（ProviderRegistry + DB 直读），管理页删改立即生效，不做内存缓存。
2. **图片由网关获取**（`/mcp` 请求体 2MiB 上限）：http(s) URL 经共享 client 下载（继承系统代理，Content-Length 预检 + 8MiB 上限），本地绝对路径 / `file://` 直接读文件；media_type 按 Content-Type / 扩展名白名单推断（png/jpeg/gif/webp/bmp）。
3. **统一 base64 上送**：Anthropic 图片 source 只收 base64；OpenAI 系转 `data:{mime};base64,{data}` data URI。按 ProviderKind（Messages / ChatCompletions / Responses）构造非流式请求（`stream: false`），key 池轮换取明文 key。
4. **不计配额**：与 web_search/web_fetch 一致，不触碰 usage 统计与服务 key 配额；单次调用不重试，上游错误文本透传。
5. **前端**：设置页路由 Tab 新增开关 + 供应商/模型级联下拉（md-outlined-select），变更即保存；切换供应商先清空模型键，杜绝「新供应商 + 旧模型」不一致。

### 原因

1. **单一配置最简**：需求是「一个视觉专用模型」，不做 per-service-key 绑定与多模型轮换；存上游 `model_id` 而非本地 UUID——模型行被删/重建不影响调用，最健壮。
2. **图片不进 MCP 帧**：2MiB 限制下 base64 传图不可行；网关出站取图顺带统一了 media_type 推断与大小控制。
3. **直连上游而非走代理层**：代理层强制 stream=true 且带配额/统计/剔除逻辑，识图调用不需要这些语义；复用 key 池与共享 client 已足够。

### 代价

- SSRF 类暴露：图片 URL 可指向内网地址（本机下载后外发供应商），与 web_fetch 既有边界一致（用户自己的供应商与 key，风险自担），spec 注明。
- 上游模型可能不支持视觉：400/404 透传并附提示，不硬编码模型能力（capabilities 默认值不可信，不做前端过滤）。
- Anthropic 上游约 5MB 图片限制：8MiB 白名单留余量，超上游限制时错误透传。

## ADR-040: 组合别名（Combo）——成员按序回退的显式退路链

**日期**: 2026-08-24  
**状态**: 已接受  
**关联**: ADR-019（故障转移：同别名多 Provider 候选 + 60s 冷却）、spec-combos.md（详细契约）

### 背景

用户需要把多个模型别名捆绑成一个新别名：客户端用组合名连接时，路由「不断尝试列表中的所有模型直到找到可用模型」。现有 failover 机制（`stream.rs` 双循环：外层 provider 候选 × 内层 key 轮换）已实现「同别名多 provider 依次尝试」，组合别名本质是把「尝试列表」从「同别名跨 provider」扩展为「显式成员序列」。

### 决策

1. **组合 = 解析层展开，零新增重试逻辑**：`resolve_combo` 把组合按成员 `position` 逐个调 `resolve_route_candidates` 展开成候选列表（跨成员按 `(provider_id, real_model_id)` 去重保序，**不能**按 provider_id 去重——同 provider 的不同成员是不同尝试），交给现有双循环执行。组合不存在/被禁用返回 `None`，调用方回落普通别名解析（向后兼容）。
2. **组合强制回退**：组合命中后 `failover = global_failover || is_combo`——成员间回退不受全局「故障转移」开关影响。组合本身就是用户显式构建的退路链，开关关掉时组合功能不应失效；普通别名行为不变。
3. **仅供应商级失败换成员**：网络错误、头超时、5xx、401/402/403/429、配额（400+quota）、空流、流内密钥错误 → 换下一个成员；**普通 400（非配额）立即透传**（`break 'provider`），请求级错误换模型也白搭。400 分支顺带清掉陈旧的 `last_resp`（既有 bug：401/5xx 残留会让兜底链透传错内容）。
4. **成员只能是叶子模型别名**：不允许嵌套组合 → 天然无环，无需运行时环检测。成员以 `display_name` TEXT 软引用（非唯一，无法建硬 FK）：成员被删/禁用 → 运行时跳过，组合结构不受影响。
5. **命名双向冲突校验**：组合名不得撞 `models.display_name`（两方向 handler 校验），否则解析歧义；大小写敏感（SQLite BINARY），TOCTOU 由 `combos.name UNIQUE` 兜底（2067 → 400）。
6. **白名单按组合名授予**：`allowed_models` 纯字符串比对，授予组合名 = 授予全部成员；只授成员名而调组合 → 403（避免「任一成员在白名单即放行」的越权风险）。`/v1/models` 列出 enabled 组合条目（`owned_by:"combo"`）供客户端发现。
7. **统计归因到实际成员**：`usage_log` 零改动——`model_display_name` = 客户端用的组合名，`model_id` = 实际命中的成员行，top_model 按成员聚合。

### 原因

1. **复用双循环**：组合的全部回退语义（冷却跳过、key 轮换、失败标记）自动继承，改动面最小（解析段 + 一个分支），风险可控。
2. **叶子成员最简**：嵌套组合需要运行时环检测 + UI 复杂度翻倍，当前需求（「模型别名捆绑」）没有嵌套场景。
3. **400 透传优于全量尝试**：请求本身非法时换模型是白跑延迟；「可用模型」语义 = 能正常响应，而非能接受请求。
4. **TEXT 软引用优于硬 FK**：display_name 非唯一无法建 FK；软引用让删除模型不影响组合结构，与 usage_log 自包含快照的设计哲学一致。

### 代价

- 组合解析是 N+1 查询（1 次组合 + 每成员 1 次候选查询），成员数小可接受；未做缓存（保持 ~1ms 级解析延迟）。
- 同 provider 的后续成员在 provider 级失败后会被 60s 冷却跳过（provider 是失败单元）——期望语义，已写入 spec 防止被当 bug「修复」。
- `combo_members` 无级联清理孤儿行（成员别名被删后残留），运行时跳过即可，不引入清理任务。
