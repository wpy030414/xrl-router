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
2. **性能**: Rust 后端处理高并发代理请求
3. **安全**: Rust 内存安全 + 系统级加密库原生支持
4. **跨平台**: 一套代码编译 macOS/Windows/Linux

### 代价

- 需要 Rust 工具链（学习曲线）
- WebView 渲染在某些系统可能不一致

---

## ADR-002: 仅支持流式代理（stream=true）

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

LLM API 支持流式（SSE）和非流式两种模式。完整支持需要两套代码路径。

### 决策

强制 `stream=true`。即使客户端发送 `stream=false`，也会被静默覆写为 `true`。

### 原因

1. **简化实现**: 只需一套流式处理逻辑
2. **用户体验**: Claude Code 等主流客户端都默认流式
3. **资源效率**: 流式可以边生成边传输，不需要缓存完整响应

### 代价

- 无法支持需要完整响应的场景（如某些 batch 处理）

---

## ADR-003: 密钥健康状态纯内存存储

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

密钥健康状态（green/yellow/red）需要持久化还是仅内存？

### 决策

健康状态仅存内存，启动时全部初始化为 green。只有轮询指针（`current_index`）持久化到 `settings` 表。

### 原因

1. **启动恢复**: 重启后从上次轮询位置继续
2. **减少 IO**: 每次健康状态变更都写 DB 会产生大量小事务
3. **语义合理**: 健康状态是运行时概念，重启后重新探测更合理

### 代价

- 重启后无法看到历史健康状态

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

### 代价

- 数据冗余（每条日志多存 ~100 字节）

---

## ADR-005: 管理 API 无认证，绑定 loopback

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

管理 API（`/api/providers`、`/api/keys` 等）是否需要认证？

### 决策

管理 API 无认证，仅通过绑定 loopback IP + CORS 白名单保护。

### 原因

1. **本地场景**: 桌面应用运行在用户本机，威胁模型是"本机其他进程"
2. **简化使用**: 无需登录/Token，打开应用即用
3. **CORS 保护**: 浏览器端恶意网页无法跨域调用

### 威胁模型

- **已防护**: 远程攻击（绑定 loopback）、浏览器跨域攻击（CORS）
- **未防护**: 本机恶意进程可以访问管理 API
- **接受风险**: 桌面应用场景，用户应保证本机安全

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

### 代价

- 主密钥文件丢失则所有 Provider Key 不可恢复

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

### 代价

- 验证需要逐条遍历所有 Service Key（无法索引查找）

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

### 职责分工

| 职责 | Router | Plugin |
|------|--------|--------|
| 密钥轮换 | ✅ | ❌ |
| 健康监控 | ✅ | ❌ |
| 用量统计 | ✅ | ❌ |
| 协议转换 | ❌ | ✅ |
| 业务头注入 | ❌ | ✅ |

---

## ADR-015: Token 配额用滚动窗口 + 按需聚合

**日期**: 2026-08-02  
**状态**: 已接受

### 背景

需求：每个 Service Key 可配置 5 小时 / 7 天内的 token 上限，触顶返回 429。

### 决策

1. **滚动窗口**：窗口按 Unix 时间对齐（`now % window_secs`），不是自然日/自然小时
2. **上限持久化、用量按需聚合**：`service_keys` 只存 `quota_5h/quota_7d`；已用量每次从 `usage_log` 条件聚合
3. **429 采用 quota_error 类型**：携带 `retry-after` 头 + 可读的重置时间

### 原因

1. **正确性**：滚动窗口平滑且与上游配额对齐
2. **简单**：单条 SQL 即得两窗口用量，无新增状态
3. **一致**：`/v1/user/balance` 与表格「限额」列共用同一聚合函数

### 代价

- 每个代理请求多一次 SQLite 条件聚合查询

---

## ADR-016: 统一 HTTP 客户端工厂 + 系统代理自动继承

**日期**: 2026-08-03  
**状态**: 已接受

### 背景

项目有多处出站 HTTP 请求，各自独立构建。国内网络下需走代理才能连通。

### 决策

新增 `http.rs` 模块作为唯一 HTTP 客户端工厂：

1. `system_proxy()`: 解析系统代理（环境变量 → Windows 注册表 → macOS scutil），OnceLock 缓存
2. `build_http_client() -> ClientBuilder`: 返回带系统代理的 builder
3. NO_PROXY 默认豁免 `localhost`、`127.0.0.1`、`[::1]`

所有出站 HTTP 请求必须使用工厂方法。

### 原因

1. **统一代理**：所有调用点自动继承系统代理
2. **零配置**：Windows 用户配 Clash 系统代理后自动继承
3. **性能**：OnceLock 缓存代理解析结果

### 代价

- 代理在应用运行期间不可变

---

## ADR-017: 单 listener + 路径级 IP 限制

**日期**: 2026-08-06  
**状态**: 已接受

### 背景

旧方案用两个 listener 分离：admin 绑 `127.0.0.1:19068`，public 绑 `0.0.0.0:19069`。

### 决策

合并为单 listener 绑 `0.0.0.0:19068`，通过路径级 IP 中间件控制访问权限：

| 路径类型 | 限制方式 | 路由 |
|----------|----------|------|
| 公开 | 不限 IP | `/health`、`/ws`、`/v1/*` 代理 |
| 管理 | `admin_ip_guard` 中间件限 loopback | `/api/*` CRUD |

### 原因

1. **单端口简化运维**：防火墙只需放行 19068
2. **IP 中间件而非双 listener**：路径级限制比端口级更灵活
3. **保留 loopback 安全模型**：`admin_ip_guard` 确保 `/api/*` 管理端点永不对外开放

### 代价

- `0.0.0.0:19068` 向局域网暴露：任何人可调 `/v1/*`（需有效 key）

---

## ADR-019: 故障转移（Provider Failover）

**日期**: 2026-08-03  
**状态**: 已接受

### 背景

同一模型别名配置在多个 Provider（官方 + 代理镜像）时，上游故障会让整个会话失败。

### 决策

1. **候选解析**：`resolve_route_candidates()` 返回同 `display_name` 下全部候选（`sort_order ASC, created_at ASC`，按 provider_id 去重）
2. **双层循环**：外层遍历 provider 候选，内层遍历 key 池
3. **冷却表纯内存**：失败 provider 冷却 60s
4. **开关默认关闭**：`failover_enabled` 存 settings 表
5. **错误码语义**：网络错误 502、响应头超时 504、key 4xx 耗尽透传最后一次上游失败响应、无可用 key 503

### 原因

1. 按 provider_id 去重避免同一上游被重复尝试
2. 冷却 60s 而非指数退避：场景是「上游暂时不可用」，固定冷却足够
3. 开关默认关闭符合「不主动改变既有行为」原则

### 代价

- 双循环复杂度集中在 handler.rs

---

## ADR-027: 引入 IR 中间表示层统一三协议转换

**日期**: 2026-08-09  
**状态**: 已接受

### 背景

项目最初只有 Anthropic Messages ↔ OpenAI Chat Completions 双向转换。随着 OpenAI Responses API 支持需求出现，三协议互转的组合爆炸问题凸显。

### 决策

1. **新建 `api/proxy/ir/` 模块**（Intermediate Representation，中间表示层）
2. **IR 以 Anthropic Messages 为骨架**：`IrContentBlock` 覆盖 Text/Image/Thinking/ToolUse/ToolResult 五种内容块
3. **单向转换取代双向转换**：所有客户端格式 → IR（`from_*`），IR → 所有客户端格式（`to_*`）
4. **`IrStreamEvent` 6 种变体**：MessageStart → ContentBlockStart → ContentBlockDelta → ContentBlockStop → MessageDelta → MessageStop
5. **`IrUsage` 统一 token 统计**：input_tokens / output_tokens / cache_read_input_tokens 等
6. **usage 真实值覆盖估算占位**：上游返回真实 usage 时直接覆盖估算值（不用 `max()`）
7. **上下文超限预警（软警告）**：仅记录 warn 日志，不返回 400 错误（避免阻断客户端 auto-compact）

### 原因

1. **组合爆炸收敛**：N 个协议只需 2N 个转换模块，而非 N×(N-1) 个双向模块
2. **内部工具解耦**：websearch 劫持、usage 追踪等内部工具只操作 IR 类型
3. **扩展性**：新增协议只需新增一对 `from_*` / `to_*` 模块

### 代价

- 转换路径变长：客户端格式 → IR → 客户端格式（两步）

---

## ADR-035: WebSearch/WebFetch 迁移到本地 MCP 端点

**日期**: 2026-08-24  
**状态**: 已接受

### 背景

旧方案由代理跑 server-side 劫持循环：注入 `web_search` 工具 → 缓冲所有中间搜索轮次 → 本地 Bing 搜索回传 → 收尾合成响应。

### 决策

1. **删除劫持循环**，改为网关内置 **MCP（Streamable HTTP）端点 `/mcp`**，提供 `web_search`、`web_fetch`、`web_vision` 三个工具
2. **Bing 搜索策略**：HTTP 浏览器头 + cookie 复用 + 懒预热 + 双域名 fallback + 绕过代理直连
3. **WebFetch 渲染**：Tauri 内置 WebView（懒创建隐藏窗口）+ 静态抓取回退
4. **web_vision**：设置页指定「视觉专用模型」，网关取图后调该模型生成描述
5. **开关语义**：`mcp_websearch` / `mcp_webfetch` / `mcp_vision` 三个开关独立控制
6. **代理侧只保留「剔除」逻辑**：开关开启时剔除请求自带搜索工具（防上游官方搜索生效）

### 原因

1. **标准协议 > 私有序列**：MCP 是客户端原生支持的协议，工具调用可见、可审批
2. **性能**：模型直接调用工具，无中间轮缓冲
3. **删除大于新增**：净删 ~1600 行，新增 ~600 行

### 代价

- 客户端需一次性注册 MCP（设置页提供可复制的命令）

---

## ADR-040: 组合别名（Combo）

**日期**: 2026-08-24  
**状态**: 已接受

### 背景

用户需要把多个模型别名捆绑成一个新别名：客户端用组合名连接时，路由「不断尝试列表中的所有模型直到找到可用模型」。

### 决策

1. **组合 = 解析层展开**：`resolve_combo` 把组合按成员 `position` 逐个展开成候选列表，交给现有双循环执行
2. **组合强制回退**：组合命中后 `failover = global_failover || is_combo`——成员间回退不受全局开关影响
3. **仅供应商级失败换成员**：网络错误、头超时、5xx、401/402/403/429 → 换下一个成员；普通 400（非配额）立即透传
4. **成员只能是叶子模型别名**：不允许嵌套组合 → 天然无环
5. **命名双向冲突校验**：组合名不得撞 `models.display_name`
6. **白名单按组合名授予**：授予组合名 = 授予全部成员
7. **统计归因到实际成员**：`usage_log` 零改动

### 原因

1. **复用双循环**：组合的全部回退语义自动继承
2. **叶子成员最简**：嵌套组合需要运行时环检测 + UI 复杂度翻倍
3. **400 透传优于全量尝试**：请求本身非法时换模型是白跑延迟

### 代价

- 组合解析是 N+1 查询（1 次组合 + 每成员 1 次候选查询）
