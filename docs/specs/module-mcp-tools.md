# Spec: MCP 工具端点（/mcp）

> 取代 `spec-websearch-hijack.md`（server-side 劫持 + tool-calling loop 已删除）。

## 概述

网关在现有单 listener（`0.0.0.0:19068`）上暴露一个 **MCP（Model Context Protocol）Streamable HTTP 端点** `/mcp`，对外提供三个工具。客户端（如 Claude Code）注册该端点后，模型通过标准 MCP tool-calling 调用本地搜索与网页抓取能力——不再由代理跑 server-side 劫持循环。

设计动机与选型见 `docs/DECISIONS.md`（「从 server-side 劫持迁移到本地 MCP」等三条决策）。

## 端点契约

- **路径**：`/mcp`（公开区，不走 `admin_ip_guard`；挂 Service Key 鉴权，安全模型同 `/v1/*`）。
- **传输**：Streamable HTTP（`rmcp` 官方 SDK 的 `StreamableHttpService`），无会话模式（`NeverSessionManager` + `legacy_session_mode = false`）——工具只读、无服务端推送，无需 MCP 会话。
- **方法**：`any` 路由覆盖 POST（JSON-RPC）/ GET / DELETE；rmcp 负责协议内其余请求的 405/400 处理。
- **鉴权**：`Authorization: Bearer <service-key>`，复用 `api/proxy/auth.rs::verify_service_key`（argon2）。缺失或无效 → 401 JSON。
- **开关全关时**：协议请求仍正常应答（initialize 成功、`tools/list` 返回空数组），保证已注册客户端连接不报错。
- **缓存提示（SEP-2549）**：协议修订版 `2026-07-28` 把 `ttlMs` / `cacheScope` 列为列表结果（`tools/list` 等）的**必填**字段。rmcp 3.x 仅在 `#[tool_router]` 宏生成的路径上补发，本项目为按开关动态过滤而手写 `ServerHandler`，必须在 `list_tools` 里按协商版本条件补发，否则严格校验该修订版的客户端（Claude Code ≥2.1.x）判定结果非法（`Invalid result for tools/list`），内置工具全部不可用。取值固定 `ttlMs = 0` + `cacheScope = private`（工具列表受运行时开关控制，不可缓存；与 `DiscoverResult::new` 表达「不可缓存」的方式一致）。

## 工具定义

### `web_search`（受 `mcp_websearch` 开关控制）

- **参数**：`{ "query": string, "ori_question"?: string }`（`query` required；`ori_question` optional——多轮对话的原始意图，注入后参与重排打分与摘要句子选取，对齐 DeepSeek Search API 同名字段语义）。
- **实现**：`crate::search::bing::search`（AppState 的 `SearchHttp`：完整浏览器头 + cookie 复用 + 懒预热 + 双域名 fallback + 绕过代理直连，取 ≤12 条候选）→ `crate::content::enrich_search_results` 内容增强：
  1. 黑名单过滤（文档倒卖/内容农场站）+ URL 去重 + 零成本预排（title/snippet 覆盖度 + 域名权威度）选出 top 5 抓取候选；
  2. 并发静态抓取候选页（复用 `AppState.http_client` 统一工厂 + 浏览器 UA，单页 8s 超时、整体 10s 预算，deadline 下放到 join 循环保留部分结果）；
  3. readability 提取正文 → 抽取式 `summary`（600~800 字，查询词覆盖引导）+ `rerank_score`（0~3，相关性 × 域名权威 × 新鲜度 × 内容质量）；
  4. 按 `rerank_score` 稳定降序，裁剪到 ≤10 条输出。
  - 慢页/失败页降级为仅 snippet；TTL 提取缓存（容量 32、15min）只服务重复搜索，**web_fetch 不读**（防静态降级内容污染 WebView 渲染路径）。
- **输出格式**（`[n] title\nURL:\n元数据行\n摘要:` 多条拼接）：

```
[1] 标题1
URL: https://example.com/1
日期: 2026-03-12 | 站点: Example Blog | 权威度: 3 | 相关度: 2.6/3
摘要: 长摘要1（抓到正文时为 600~800 字抽取式摘要；未抓到时退化为 Bing snippet）

[2] 标题2
URL: https://example.com/2
日期: 未知 | 站点: 未知 | 权威度: 1 | 相关度: 0.3/3
摘要: Bing snippet2
```

  - `相关度 ≥1.5` 视为相关（工具描述中已告知 LLM 该约定）；缺失元数据显示「未知」。
- **错误处理**：空结果 → "No web search results found for: {query}"；搜索失败 → "Web search unavailable: {error}. Do NOT make up information."（工具级错误，客户端可见）。enrichment 阶段永不失败——全部抓取失败时退化为纯 snippet 输出。

### `web_fetch`（受 `mcp_webfetch` 开关控制）

- **参数**：`{ "url": string }`（required）。
- **实现**：`mcp/fetch.rs`——**Tauri 内置 WebView 渲染**（macOS WKWebView / Windows WebView2 / Linux WebKitGTK）：懒创建隐藏窗口（label `fetcher`，进程级保活）→ `navigate` → 轮询 `eval_with_callback` 等 `readyState == "complete"` 且资源计数稳定（SPA 渲染完成信号）→ eval 取渲染后 HTML（JS 侧先截断 ~1.5MB，防 WebView2 回传体积限制）→ **`content::readability` 提取正文子树**（剥离 nav/header/footer/aside/script 与 cookie/banner/sidebar/comment 等噪声节点，选出正文容器；提取在 `spawn_blocking` 执行，`scraper::Html` 非 Send）→ `htmd` 转 Markdown → 加 `# {title}` 头 → 截断（约 60K 字符）。
  - **提取降级链**：quality < 0.35 或正文转出的 Markdown < 80 字符 → 退回整页 `htmd::convert`（现状行为兜底），htmd 失败再退化为原始 HTML 截断。
- **回退**：WebView 创建失败/超时/eval 异常 → 静态抓取（`crate::http::build_http_client()`，继承系统代理），输出开头附注「WebView 渲染不可用：{原因}。以下为静态抓取内容，可能不含 JS 渲染结果」。
- **并发**：`tokio::sync::Mutex` 串行（一次渲染一页，跨 await 持有）。
- **错误处理**：导航失败/超时 → 工具级错误文本。

## 开关语义（设置页「路由」Tab）

| 设置键 | 默认 | 效果 |
|--------|------|------|
| `mcp_websearch` | `false`（V16 迁移自 `websearch_hijack`） | ON：`/mcp` 提供 `web_search` + **代理剔除请求自带的搜索类工具**；OFF：不碰工具定义 |
| `mcp_webfetch` | `false` | ON：`/mcp` 提供 `web_fetch`；OFF：不提供 |
| `mcp_notify` | `false` | ON：`/mcp` 提供 `notify`（系统桌面通知）；OFF：不提供 |

持久化：`settings` 表（`mcp_websearch` / `mcp_webfetch`），AppState 原子量运行时读写，`/api/settings` GET/PUT。

## 代理侧剔除逻辑（`stream.rs`）

`proxy_stream()` 入口：`mcp_websearch` ON 时调 `strip_search_tools(&mut ir_request)`：

1. 移除所有搜索类工具（`is_search_tool_name`：`web_search*` 前缀 + 大小写不敏感 `WebSearch`）。
2. `tool_choice` 若指向被移除的搜索工具 → 改写为 `Auto`（代理不再注入工具，无可改写目标）。

> 与旧劫持的区别：只剔除、不注入、不跑循环。模型联网搜索走客户端注册的 MCP 工具。

server-side 工具归一化（`from_messages.rs` / `from_responses.rs`：`web_search_*` 无 name → `name="web_search"`）保留，剔除检测依赖它。

## 实现位置

- `src-tauri/src/mcp/mod.rs` — `/mcp` handler（鉴权 + 委托）+ 全局服务单例 + `init()` 注入 AppState 与 AppHandle。
- `src-tauri/src/mcp/tools.rs` — `ServerHandler` 实现（`list_tools` 按开关动态过滤 / `call_tool` 分发）+ 工具 schema + `format_search_text`。
- `src-tauri/src/mcp/fetch.rs` — 隐藏 WebView 窗口管理 + 渲染等待轮询 + eval 提取 + 静态回退 + readability 提取 + HTML→Markdown。
- `src-tauri/src/content/` — 内容增强层（search 富化与 fetch 提取共用）：
  - `mod.rs` — `enrich_search_results` 编排（预排选候选 + JoinSet 并发抓取 + TTL 缓存）
  - `readability.rs` — trafilatura-lite 正文提取（prune + 自底向上打分选正文 + 元数据 + 质量分 + 编码解码）
  - `summary.rs` — 抽取式摘要（分句打分 + 贪心选入预算 + 保序拼接）
  - `rank.rs` — rerank_score 组合打分
  - `text.rs` — 分词（CJK bigram + Latin 分词）/分句/查询词覆盖度
  - `authority.rs` — 域名分级表 + 垃圾站黑名单
- `src-tauri/src/api/proxy/stream.rs` — `strip_search_tools` + 入口调用。
- `src-tauri/src/api/router.rs` — `/mcp` 路由注册。
- `src/views/SettingsView.tsx` — 三个开关 + MCP 接入信息卡（端点 + 注册命令 + 复制）。

## 依赖

- `rmcp`（MCP Rust SDK）：`server` + `macros` + `transport-streamable-http-server`。
- Tauri 内置 WebView（`tauri::WebviewWindow` + `eval_with_callback`）——无额外浏览器依赖，见 ADR-038。
- `htmd`（HTML → Markdown）。
- `scraper`（HTML CSS 选择器解析，readability 提取复用）+ `ego-tree`（scraper 的 DOM 树类型 NodeId/NodeMut，原地 detach 修剪）+ `encoding_rs`（非 UTF-8 中文页面 GBK/GB2312 解码）。

> rmcp 的 `StreamableHttpService` 泛型接受任意 `http_body::Body`，返回 `BoxBody`，可直接嵌入 axum 0.7 handler，无需升级 0.8。

## 测试要求

1. **单元测试**：工具按开关过滤（三开关 8 组合核心子集）、工具 schema 字段、搜索结果格式化、URL 归一化、输出截断、HTML→Markdown。
2. **集成**：`/mcp` 鉴权（无/错误 key → 401）、initialize / tools/list / tools/call 往返（手工 JSON-RPC POST）。

## 完成标准

- [x] `/mcp` Streamable HTTP 端点 + Service Key 鉴权
- [x] `web_search` / `web_fetch` 工具，按开关动态过滤 `tools/list`
- [x] WebFetch 用 Tauri 内置 WebView 渲染（隐藏窗口 + eval 提取 + 静态回退）
- [x] 代理侧搜索工具剔除（`mcp_websearch` ON）
- [x] 删除旧劫持循环（`websearch.rs` + `forward_stream_ir_to_buffer` + `accumulate_ir_events` + 卡片渲染）
- [x] 设置开关 + DB 迁移 V16 + 前端 UI（两开关 + 接入信息卡）
- [x] 文档同步（本 spec + ARCHITECTURE + PRD + DECISIONS + AGENTS + README）
