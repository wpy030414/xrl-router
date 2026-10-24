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

- **参数**：
  - `query`（required）
  - `num_results`（optional，1~20，默认 8）
  - `freshness`（optional，`day`/`week`/`month`/`year`；各引擎支持度不同，Bing 为 best-effort）
  - `include_domains` / `exclude_domains`（optional，string[]）
  - `ori_question`（optional，多轮对话原始意图，参与重排与摘录选取）
- **引擎选择（`crate::search`）**：
  - 后端：`auto`（默认）| `bing` | `searxng` | `tavily` | `exa` | `brave`；密钥与实例地址存
    `settings` 表（API Key 走 AES-256-GCM，见 `search/config.rs`）。
  - `auto`：把**已配置**的 SearXNG / Tavily / Exa / Brave **并发**执行（单引擎 15s、
    整体 20s 预算），按 RRF（k=60）融合名次，再做 URL 归一化去重（去 fragment /
    跟踪参数 / 尾斜杠）；全部失败时回退 Bing。未配置任何来源 → 仅 Bing。
  - 显式指定后端时不做融合，直接返回该引擎排序。
  - **查询缓存**：`(backend, query, num_results, freshness, domains)` 为键，TTL 15min、
    容量 64，避免同一会话反复打同一查询。
  - **域名过滤**：Tavily 用原生参数，其余引擎用 `site:` / `-site:` 操作符表达；
    结果侧再统一过滤一次（子域匹配、排除优先）。
- **内容增强**（`crate::content::enrich_search_results`，永不失败）：
  1. 黑名单过滤（文档倒卖/内容农场站）+ URL 去重 + 零成本预排（title/snippet
     覆盖度 + 域名权威度）选出 top 3 抓取候选；
  2. 并发静态抓取候选页（复用 `AppState.http_client` 统一工厂 + 浏览器 UA，
     单页 8s 超时、整体 10s 预算，deadline 下放到 join 循环保留部分结果）；
  3. readability 提取正文 → **命中段摘录** `summary`（围绕最高分句的连续窗口，
     ≤600 字，带上下文衔接）+ `rerank_score`（0~3，参考分，由相关性 × 域名权威 ×
     新鲜度 × 内容质量组成）；
  4. 按 `rerank_score` 稳定降序，裁剪到 ≤20 条输出。
  - 慢页/失败页降级为仅 snippet；TTL 提取缓存（容量 32、15min）只服务重复搜索，
    **web_fetch 不读**（防静态降级内容污染渲染路径）。
- **输出格式**（`[n] title\nURL:\n元数据行\n摘要:` 多条拼接）：

```
[1] 标题1
URL: https://example.com/1
日期: 2026-03-12 | 站点: Example Blog | 权威度: 3 | 相关度: 2.6/3
摘要: 命中段摘录1（抓到正文时为围绕最佳句的连续窗口；未抓到时退化为 snippet）

[2] 标题2
URL: https://example.com/2
日期: 未知 | 站点: 未知 | 权威度: 1 | 相关度: 0.3/3
摘要: Bing/引擎 snippet2
```

  - `相关度` 是**参考分**，工具描述不再要求模型按阈值丢弃结果；缺失元数据显示「未知」。
- **错误处理**：空结果 → "No web search results found for: {query}"；全部来源失败 →
  "Web search unavailable: {各来源错误}. Do NOT make up information."（工具级错误）。
  enrichment 阶段永不失败——全部抓取失败时退化为纯 snippet 输出。

### `web_fetch`（受 `mcp_webfetch` 开关控制）

- **参数**：`{ "url": string, "start_index"?: integer, "max_chars"?: integer }`
  - `url` required；`start_index` 默认 0（按字符计）；`max_chars` 默认 60000，clamp 到 `[1000, 200000]`。
- **抓取链**（`mcp/fetch.rs::fetch_url`）——**静态优先，按需渲染**：
  1. `normalize_url`（无协议补 `https://`，仅接受 http/https）→ **SSRF 守卫**
     （`crate::net_guard::ensure_public_url`：字面量 + DNS 解析后双重检查，拒绝
     loopback / 私有段 / 链路本地 / 云元数据服务）；
  2. **内容缓存**（键 = 去 fragment 与 `utm_*`/`gclid` 等跟踪参数的 URL，TTL 15min，
     容量 64，`Arc<Page>` 共享）——重复抓取零成本；
  3. **静态请求**（专用 client：`crate::http::build_http_client()` 继承系统代理 +
     完整浏览器头 + `Accept: text/markdown, text/html;q=0.9, …` 内容协商 +
     逐跳重定向守卫 + 20s 超时 + 5MB 上限）：
     - `text/html`/`application/xhtml+xml`：`content::decode_body` 解码 →
       `content::extract` 提取正文 → `htmd` 转 Markdown；
     - `text/*`、`application/json`、`application/xml`、`text/csv`：直接返回
       （JSON/XML/CSV 包代码围栏），不解析；
     - `application/pdf`：`pdf_extract::extract_text_from_mem` 提取文本层；
     - 其他二进制类型：工具级错误「不支持的内容类型」。
  4. **升级渲染判定**：静态失败时按状态码决定——403/429/451/5xx 与网络错误值得
     再试 WebView，404/410 等直接返回错误；静态成功但正文 < 400 字符**且**页面带
     SPA 特征（`id="root"`/`__NEXT_DATA__`/`ng-version` 等）或正文 < 80 字符时升级。
  5. **WebView 渲染**（macOS WKWebView / Windows WebView2 / Linux WebKitGTK）：
     隐藏窗口**池**（默认 2 个，`Semaphore` 限流，Drop 归还）→ `navigate` →
     轮询 `readyState == "complete"` 且资源计数稳定 → eval 取渲染后
     `{ href, html }`（JS 侧截断 ~1.5MB）→ `content::extract` + `htmd`。
     `on_navigation` 对每次导航（含 3xx）做内网闸。
  6. **降级链**：渲染失败但静态成功 → 用静态内容并在头部注明「可能不含 JS 渲染
     结果」；两者都失败 → 错误文本同时包含两侧原因。
- **输出框架**：`# {title}`（有标题时）+ 元数据头（`URL` / `抓取方式` /
  `正文: {total} 字符；本次返回 {start}–{end}`）+ 指定区间正文。截断时尾部附
  「剩余 N 字符，可设置 `start_index={end}` 继续读取」；`start_index` 越界时明确
  提示未返回内容。
- **错误处理**：SSRF 拒绝、超时、非 2xx、不支持类型、PDF 无文本层 → 工具级错误文本。

### SSRF 防护（`net_guard.rs`）

`web_fetch` 与 `web_search` 富化抓取共用同一道闸：

- **协议**：仅 `http`/`https`。
- **字面量**：IPv4 回环/私有/链路本地（含 169.254.169.254）/CGNAT/文档保留/组播；
  IPv6 回环/未指定/唯一本地/链路本地/组播/IPv4 映射；域名 `localhost`、`*.localhost`、
  `*.local`、`*.internal`、`*.home.arpa`、云元数据域名。
- **解析后**：域名先 DNS（3s 超时）解析，任一地址命中私有段即拒绝。
- **重定向**：静态 client 的 `Policy::custom` 与渲染窗口的 `on_navigation` 逐跳复查
  字面量。
- **已知边界**：检查与连接之间的 DNS rebinding 窗口不设防（单用户桌面场景取舍，
  见 ADR-056）。局域网地址一律拦截——`web_fetch` 不承担内网探测职责。

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
- `src-tauri/src/mcp/fetch.rs` — 静态优先抓取链 + 内容缓存 + 渲染池 + 内容类型分支 + 分段输出。
- `src-tauri/src/net_guard.rs` — SSRF 守卫（字面量 + DNS 解析后检查，重定向复用）。
- `src-tauri/src/content/` — 内容增强层（search 富化与 fetch 提取共用）：
  - `mod.rs` — `enrich_search_results` 编排（预排选候选 + JoinSet 并发抓取 + TTL 缓存）
  - `readability.rs` — trafilatura-lite 正文提取（prune + 自底向上打分选正文 + 元数据 + 质量分 + 编码解码）
  - `summary.rs` — 抽取式摘要（分句打分 + 贪心选入预算 + 保序拼接）
  - `rank.rs` — rerank_score 组合打分
  - `text.rs` — 分词（CJK bigram + Latin 分词）/分句/查询词覆盖度
  - `authority.rs` — 域名分级表 + 垃圾站黑名单
- `src-tauri/src/search/` — 搜索层：`mod.rs`（引擎计划 / 并发 / RRF / 缓存）、
  `bing.rs`（零配置默认后端）、`engines.rs`（SearXNG / Tavily / Exa / Brave）、
  `config.rs`（settings 读写 + API Key 加密）。
- `src-tauri/src/api/proxy/stream.rs` — `strip_search_tools` + 入口调用。
- `src-tauri/src/api/router.rs` — `/mcp` 路由注册。
- `src/views/SettingsView.tsx` — 三个开关 + MCP 接入信息卡（端点 + 注册命令 + 复制）。

## 依赖

- `rmcp`（MCP Rust SDK）：`server` + `macros` + `transport-streamable-http-server`。
- Tauri 内置 WebView（`tauri::WebviewWindow` + `eval_with_callback`）——无额外浏览器依赖，见 ADR-038。
- `htmd`（HTML → Markdown）。
- `pdf-extract`（PDF 文本层提取，纯 Rust）。
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
