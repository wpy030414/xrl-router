# Spec: MCP 工具端点（/mcp）

> 取代 `spec-websearch-hijack.md`（server-side 劫持 + tool-calling loop 已删除）。

## 概述

网关在现有单 listener（`0.0.0.0:19068`）上暴露一个 **MCP（Model Context Protocol）Streamable HTTP 端点** `/mcp`，对外提供两个工具。客户端（如 Claude Code）注册该端点后，模型通过标准 MCP tool-calling 调用本地搜索与网页抓取能力——不再由代理跑 server-side 劫持循环。

设计动机与选型见 `docs/DECISIONS.md`（「从 server-side 劫持迁移到本地 MCP」等三条决策）。

## 端点契约

- **路径**：`/mcp`（公开区，不走 `admin_ip_guard`；挂 Service Key 鉴权，安全模型同 `/v1/*`）。
- **传输**：Streamable HTTP（`rmcp` 官方 SDK 的 `StreamableHttpService`），无会话模式（`NeverSessionManager` + `legacy_session_mode = false`）——工具只读、无服务端推送，无需 MCP 会话。
- **方法**：`any` 路由覆盖 POST（JSON-RPC）/ GET / DELETE；rmcp 负责协议内其余请求的 405/400 处理。
- **鉴权**：`Authorization: Bearer <service-key>`，复用 `api/proxy/auth.rs::verify_service_key`（argon2）。缺失或无效 → 401 JSON。
- **开关全关时**：协议请求仍正常应答（initialize 成功、`tools/list` 返回空数组），保证已注册客户端连接不报错。

## 工具定义

### `web_search`（受 `mcp_websearch` 开关控制）

- **参数**：`{ "query": string }`（required）。
- **实现**：复用 `crate::search::bing::search`（AppState 的 `SearchHttp`：完整浏览器头 + cookie 复用 + 懒预热 + 双域名 fallback + 绕过代理直连）。
- **输出格式**（`[n] title\nurl\n摘要: snippet` 多条拼接，与旧劫持循环同款）：

```
[1] 标题1
URL1
摘要: 摘要1

[2] 标题2
URL2
摘要: 摘要2
```

- **错误处理**：空结果 → "No web search results found for: {query}"；搜索失败 → "Web search unavailable: {error}. Do NOT make up information."（工具级错误，客户端可见）。

### `web_fetch`（受 `mcp_webfetch` 开关控制）

- **参数**：`{ "url": string }`（required）。
- **实现**：`mcp/fetch.rs`——探测本机 Chrome/Edge（Windows 优先 Edge 系统自带，其次 Chrome；含 `https://` 协议补全），`headless_chrome` CDP 无头渲染（JS 执行，SPA 内容可得），取渲染后 HTML → `htmd` 转 Markdown → 截断（约 60K 字符）。
- **回退**：探测不到本机浏览器 → 静态抓取（`crate::http::build_http_client()`，继承系统代理），输出开头附注「未检测到本机浏览器，可能不含 JS 渲染结果」。
- **浏览器生命周期**：进程级懒启动 + 保活复用（`OnceLock<Mutex<Option<Browser>>>`）；每次抓取新 Tab、用完即关；单用户场景 Mutex 串行（一次渲染一页）。
- **错误处理**：导航失败/超时 → 工具级错误文本。

## 开关语义（设置页「路由」Tab）

| 设置键 | 默认 | 效果 |
|--------|------|------|
| `mcp_websearch` | `false`（V16 迁移自 `websearch_hijack`） | ON：`/mcp` 提供 `web_search` + **代理剔除请求自带的搜索类工具**；OFF：不碰工具定义 |
| `mcp_webfetch` | `false` | ON：`/mcp` 提供 `web_fetch`；OFF：不提供 |

持久化：`settings` 表（`mcp_websearch` / `mcp_webfetch`），AppState 原子量运行时读写，`/api/settings` GET/PUT。

## 代理侧剔除逻辑（`stream.rs`）

`proxy_stream()` 入口：`mcp_websearch` ON 时调 `strip_search_tools(&mut ir_request)`：

1. 移除所有搜索类工具（`is_search_tool_name`：`web_search*` 前缀 + 大小写不敏感 `WebSearch`）。
2. `tool_choice` 若指向被移除的搜索工具 → 改写为 `Auto`（代理不再注入工具，无可改写目标）。

> 与旧劫持的区别：只剔除、不注入、不跑循环。模型联网搜索走客户端注册的 MCP 工具。

server-side 工具归一化（`from_messages.rs` / `from_responses.rs`：`web_search_*` 无 name → `name="web_search"`）保留，剔除检测依赖它。

## 实现位置

- `src-tauri/src/mcp/mod.rs` — `/mcp` handler（鉴权 + 委托）+ 全局服务单例 + `init()` 注入 AppState。
- `src-tauri/src/mcp/tools.rs` — `ServerHandler` 实现（`list_tools` 按开关动态过滤 / `call_tool` 分发）+ 工具 schema。
- `src-tauri/src/mcp/fetch.rs` — 浏览器探测 + headless 渲染 + 静态回退 + HTML→Markdown。
- `src-tauri/src/api/proxy/stream.rs` — `strip_search_tools` + 入口调用。
- `src-tauri/src/api/router.rs` — `/mcp` 路由注册。
- `src/views/SettingsView.vue` — 两个开关 + MCP 接入信息卡（端点 + 注册命令 + 复制）。

## 依赖

- `rmcp`（MCP Rust SDK）：`server` + `macros` + `transport-streamable-http-server`。
- `headless_chrome`（CDP 无头浏览器驱动）。
- `htmd`（HTML → Markdown）。

> rmcp 的 `StreamableHttpService` 泛型接受任意 `http_body::Body`，返回 `BoxBody`，可直接嵌入 axum 0.7 handler，无需升级 0.8。

## 测试要求

1. **单元测试**：工具按开关过滤（00/10/01/11）、工具 schema 字段、搜索结果格式化、URL 归一化、输出截断、HTML→Markdown、浏览器候选列表非空。
2. **集成**：`/mcp` 鉴权（无/错误 key → 401）、initialize / tools/list / tools/call 往返（手工 JSON-RPC POST）。

## 完成标准

- [x] `/mcp` Streamable HTTP 端点 + Service Key 鉴权
- [x] `web_search` / `web_fetch` 工具，按开关动态过滤 `tools/list`
- [x] WebFetch 本机浏览器渲染（探测 + 回退静态抓取）
- [x] 代理侧搜索工具剔除（`mcp_websearch` ON）
- [x] 删除旧劫持循环（`websearch.rs` + `forward_stream_ir_to_buffer` + `accumulate_ir_events` + 卡片渲染）
- [x] 设置开关 + DB 迁移 V16 + 前端 UI（两开关 + 接入信息卡）
- [x] 文档同步（本 spec + ARCHITECTURE + PRD + DECISIONS + AGENTS + README）
