# AGENTS.md

本文件为 AI Agent 在 xrl-router 项目上工作时必须遵守的边界与约束。

## 项目范围

xrl-router 是一个**单用户本地 LLM API 网关**，以 Tauri 2 桌面应用形式运行。Rust 后端（`src-tauri/src/`）跑 axum HTTP 服务：**单 listener 绑 `0.0.0.0:19068`**，通过路径级 IP 中间件（`admin_ip_guard`）限制 `/api/*` 管理端点仅 loopback 可访问，其余路径（`/v1/*`、`/api/ui-settings`、`/health`、`/ws`）对外开放，未匹配路由 fallback 到前端 SPA `index.html`。Vue 3 前端（`src/`）跑在 Tauri WebView 里，局域网设备也可通过浏览器直接访问 SPA 的 `/install` 页面。所有数据存本地 SQLite。

## 代码组织

```
src-tauri/src/                 后端 Rust
├── main.rs                    入口（thin wrapper，仅调用 lib）
├── lib.rs                     Tauri setup + 数据目录 + master key + DB + 系统托盘 + 网关启动
├── config.rs                  环境变量配置
├── error.rs                   AppError 统一错误类型
├── http.rs                    统一 HTTP 客户端工厂（系统代理自动继承）
├── crypto/mod.rs              AES-256-GCM + Argon2 + master key
├── gateway/server.rs          AppState + start_gateway (单 listener) + CORS
├── api/
│   ├── router.rs              axum 路由表（build_router）+ SPA fallback（ServeDir）
│   ├── handlers/*             管理 API 处理器（按实体分文件；install.rs 提供 local-ip 接口；data.rs 数据导出/导入/重置；stats.rs 含 ui-settings）
│   └── proxy/*                LLM 代理核心
│       ├── handler.rs         薄入口层：认证 + 请求体准备，委托 stream::proxy_stream()
│       ├── stream.rs          流式引擎核心：路由解析 → 立即返回 Response → 后台 spawn 双循环
│       │                      （含搜索工具剔除：mcp_websearch 开启时 strip_search_tools 移除请求自带搜索工具）
│       ├── forward.rs         统一 IR 转发：上游字节 → IR 事件 → 客户端 SSE 字节（~350 行）
│       ├── auth.rs            Service Key 验证（/v1/* 与 /mcp 共用，pub(crate)）
│       ├── quota.rs           5h/7d token 配额检查
│       ├── route.rs           模型别名→上游 URL 解析
│       ├── failover.rs        provider 级冷却表
│       ├── key_rotation.rs    密钥选取 + 健康反馈
│       ├── sniff.rs           SniffStream (透传+嗅探，保留但当前未被 forward.rs 引用)
│       └── ir/                IR 中间表示层（三种协议统一抽象）
│            ├── types.rs          IrRequest / IrMessage / IrContentBlock / IrStreamEvent / IrUsage
│            ├── from_messages.rs      Anthropic Messages → IR
│            ├── from_chat_completions.rs  OpenAI Chat Completions → IR
│            ├── from_responses.rs     OpenAI Responses API → IR
│            ├── to_messages.rs          IR → Anthropic Messages
│            ├── to_chat_completions.rs  IR → OpenAI Chat Completions
│            ├── to_responses.rs       IR → OpenAI Responses API
│            └── usage.rs          Token usage 提取（三种格式）
├── mcp/*                      本地 MCP 工具服务器（/mcp Streamable HTTP 端点，rmcp）
│    ├── mod.rs                /mcp handler（Service Key 鉴权 + 委托）+ 全局服务单例 + init()
│    ├── tools.rs              ServerHandler 实现（web_search / web_fetch，tools/list 按开关动态过滤）
│    └── fetch.rs              WebFetch 渲染层（本机 Chrome/Edge headless + htmd + 静态回退）
├── db/*                       SQLite 封装（mod.rs + schema.rs + 按实体分文件）
├── types/*                    数据结构定义（Provider/Model/ApiKey/Chat/Route/...）
├── providers/                 Provider 适配器（proxy 不经过它）
│    ├─ adapter.rs             Adapter async trait（chat/chat_stream/health_check）
│    ├─ anthropic.rs           AnthropicAdapter 实现
│    └─ openai.rs              OpenAIAdapter 实现
├── plugin/*                   插件系统（mod.rs + registry/keys/health/types）
├── keys/pool/*                KeyPool（mod.rs + types/rotation/health/persistence）
├── models/mod.rs              ModelRegistry
├── middleware/rate_limit.rs   令牌桶限流
├── middleware/admin_guard.rs  IP 限制中间件（/api/* 仅 loopback）
├── search/bing.rs             Bing 搜索（HTTP 浏览器头 + cookie 复用 + 懒预热 + 双域名 fallback + ck/a 重定向解码，绕过代理直连）
├── sdk-test/                  SDK 合规验证（仅 #[cfg(test)] 编译）
│    ├── fixtures.rs           IR 转换测试 fixtures
│    ├── ir_sdk_verify.py      Python SDK 验证脚本
│    └── README.md
└── capabilities/default.json  Tauri 权限配置（shell/dialog/fs/autostart/window）

> **HTTP 客户端**：所有出站 HTTP 请求**必须**使用 `http::build_http_client()` 或 `http::http_client()`，**不要**直接 `reqwest::Client::new()` 或 `reqwest::Client::builder()`。统一工厂自动继承系统代理（环境变量 → Windows 注册表 → macOS scutil），`localhost`/`127.0.0.1` 自动豁免直连。**唯一例外**：`search/bing.rs` 的 `SearchHttp` **不**走统一工厂——Bing 对代理出口 IP（海外）返回降级结果，搜索必须直连。

src/                           前端 Vue 3
├── main.ts / App.vue / router.ts
├── api.ts                     REST 客户端（动态 BASE_URL：Tauri/localhost 用 http://127.0.0.1:19068，LAN 浏览器用当前 origin）
├── ws.ts                      WebSocket 客户端（自动重连 3s）
├── theme.ts                   明/暗/跟随系统主题（localStorage 持久化，prefers-color-scheme 监听，设置同步到后端供 LAN install 页读取）
├── i18n/                      自研 i18n：index.ts（t/setLocale/initI18n，语言切换同步到后端）+ zh-CN.ts / en.ts
├── styles/global.css          全局样式（MD3 design tokens + [data-theme="dark"]）
├── fm/                        Claude FM 播放器（极简前端：~60 行纯命令/事件）
├── views/*                    7 个页面（ClaudeFm/Providers/ProviderNew/Keys/Stats/Settings/Install）
├── components/*               AppShell / ConnectionStatus / PluginRegisterDialog / MdiIcon（动态 MDI 图标，@mdi/js SVG path）
└── stores/*                   3 个 Pinia stores（providers/keys/models）

> **Claude FM**：音频解码与播放由 Rust 后端 `FmEngine`（`api/handlers/fm.rs`）直接完成——rodio 输出到系统音频设备，souvlaki 接入系统媒体控制（macOS Now Playing / Windows SMTC / Linux MPRIS）。引擎以 `std::thread::spawn` 运行（rodio 需要稳定线程），通过 `mpsc` channel 接收播放控制消息。前端 `src/fm/player.ts`（~60 行）仅负责展示元信息（通过 `fm-meta` 事件）和控制播放暂停（通过 `fm_toggle` / `fm_play` / `fm_pause` Tauri command）。托盘 FM 菜单点击直接调用引擎（不再绕前端中转）。改 FM 逻辑改 `api/handlers/fm.rs`，改播放器 UI 改 `ClaudeFmView.vue`。

docs/                          文档（本目录）
```

## 关键约定

### 数据目录

生产环境的数据目录由 Tauri 的 `app.path().app_data_dir()` 解析（macOS: `~/Library/Application Support/im.xrl.router/`），**不要**在代码里硬编码相对路径 `data/` ——安装后的 app bundle 工作目录不可写，会导致启动闪退。

### 数据库迁移

- 迁移定义在 `src-tauri/src/db/schema.rs` 的 `MIGRATIONS` 数组
- 每个元素是一条完整 SQL，启动时按序执行
- 当前版本：**V15**（统一 provider kind 命名：`openai` → `chat_completions`、`anthropic` → `messages`）
- 新增迁移：追加到数组末尾，**不要**修改已有迁移
- 用 `ON CONFLICT DO UPDATE`（UPSERT），**不要用** `INSERT OR REPLACE`（会触发 `ON DELETE CASCADE` 清空子表，`db/mod.rs` 有回归测试）

### 密钥双轨

- **Provider API Key**：AES-256-GCM 加密存储到 `api_keys.key_hash`，主密钥在 `master.key`
- **Service Key**：Argon2 哈希存储到 `service_keys.key_hash`，创建时仅返回一次明文
- 不要混淆这两套；验证 Service Key 必须逐条 `verify_password`（盐随机不可比）

### 代理只支持流式

`api/proxy/handler.rs` 强制 `stream: true`。不要加非流式分支 ——Claude Code 等主流客户端始终流式，加非流式只会增加代码复杂度。

### 代理代码组织

- **handler.rs** 是薄入口层（~300 行）：提取 API key → authenticate_and_stream() → 委托 stream.rs
- **stream.rs** 是流式引擎核心（~630 行）：路由解析 → 搜索工具剔除（`mcp_websearch` 开启时 `strip_search_tools` 移除请求自带搜索工具，防止上游官方搜索生效）→ 立即返回 Response（含 keepalive）→ 后台 spawn 双循环重试 + 流式转发
- **forward.rs** 是统一 IR 转发层（~350 行）：单一 `forward_stream_ir` 函数处理所有格式组合（上游字节 → IR 事件 → 客户端 SSE 字节），不再有 passthrough / O→A / A→O 三路分支
- **mcp/** 是本地 MCP 工具服务器（`/mcp` Streamable HTTP 端点）：模型联网搜索与网页抓取不再走代理劫持循环（已删除），而是客户端注册该 MCP 端点后直接调用 `web_search`（复用 `search/bing.rs`）/ `web_fetch`（本机 Chrome/Edge headless 渲染）工具。契约见 `docs/specs/spec-mcp-tools.md`
- **ir/** 是协议转换核心：三种客户端格式（Anthropic Messages / OpenAI Chat Completions / OpenAI Responses）统一转换为 IR 再渲染回目标格式
- 新增代理逻辑时，应修改 stream.rs（路由/重试）、forward.rs（流转发）或 ir/（协议转换）而非 handler.rs
- 修改认证/配额/请求体准备时，修改 handler.rs 的 authenticate_and_stream()

### IR 中间表示层

- 实现在 `api/proxy/ir/`，按方向分 `from_*.rs`（3 种客户端格式 → IR）和 `to_*.rs`（IR → 3 种客户端格式）
- IR 以 Anthropic Messages 为骨架（`IrContentBlock` 覆盖 Text/Image/Thinking/ToolUse/ToolResult），并集覆盖三种格式字段
- `IrStreamEvent` 6 种变体：MessageStart → ContentBlockStart → ContentBlockDelta → ContentBlockStop → MessageDelta → MessageStop
- `IrUsage` 字段：input_tokens / output_tokens / cache_read_input_tokens / cache_creation_input_tokens / output_chars
- usage 合并策略：**真实值覆盖估算值**（不用 max——`forward.rs` 预填的 `chars/4` 估算值偏大，max 会永久压住真实值）
- Responses `input_tokens` 需减去 `cached_tokens`，保持增量口径（与 Chat Completions 一致）
- 不兼容特性（thinking、tool_choice 等）要显式转换并记 warn 日志，不要静默丢弃

### 密钥池

- 健康状态**纯内存**（启动全 green），DB 的 `status`/`last_error` 列保留但不再读写
- **轮询指针**持久化到 `settings` 表（键名 `keypool_index_{provider_id}`）
- 锁序生死攸关：`keys/pool/mod.rs` 注释里有详细规则，违反会跟插件的 `keys_update` 形成 ABBA 死锁

### 单 listener + 路径级 IP 限制（单端口架构）

- **单 listener** 绑 `0.0.0.0:19068`（`Config.host/port`），承载全部路由
- **`/api/*` 管理端点**：仅 loopback IP（`127.0.0.1` / `::1`）可访问，由 `admin_ip_guard` 中间件拦截非本机请求返回 403
- **公开端点**：`/health`、`/ws`、`/ws/plugin`、`/api/ui-settings`、`/v1/*` 代理（套 `rate_limit_middleware`）——不限 IP
- **静态文件 + SPA fallback**：`/assets/*` 由 `tower_http::ServeDir` 服务前端构建产物；所有未匹配 GET 请求 fallback 到 `index.html`，由 Vue Router 处理前端路由
- **CORS**：统一使用 origin 白名单（`Config.cors_origins`）
- **`/v1/*` 端点**：由 `router.rs` 的 `proxy_routes(state)` 构建，返回未 with_state 的 `Router<AppState>`，由调用方 `.with_state` 统一收敛
- **新增端点**：管理/密钥读写端点挂 `/api/*` 子路由（自动受 IP 限制）；局域网设备该访问的路由挂公开区
- **install 页面**：Vue SPA 路由 `/install`（`src/views/InstallView.vue`），不再编译进二进制。后端 SPA fallback 返回 `index.html`，前端 Vue Router 接管渲染。页面契约（`?t=` 明文 key、命令生成、消费端选择等）见 `docs/specs/spec-lan-deploy.md`，改页面必须同步该 spec

### 前端

- UI 用 Material Design 3（`@material/web`），**不要**引入其他组件库
- 颜色用 CSS 变量 `var(--md-sys-color-*)`，**不要**硬编码 hex
- MWC 组件在 `main.ts` 按需导入，**不要**导入 `all.js`
- `api.ts` 的 `BASE_URL` 是动态解析的：Tauri/localhost 环境用 `http://127.0.0.1:19068`，LAN 浏览器用当前 origin（避免 CORS）
- 外链打开用 `@tauri-apps/plugin-shell` 的 `open()`（如 SettingsView），不要用 `window.open`（Tauri WebView 内不可靠）
- **非 Tauri 环境兼容**：前端代码（`App.vue`、`theme.ts`、`fm/player.ts` 等）通过动态 `import()` 延迟加载 Tauri API，LAN 浏览器访问 install 页面时不会触发 Tauri 依赖报错

## 测试

- 测试写在内联 `#[cfg(test)] mod tests` 块里，**不要**新建 `tests/` 目录
- 用 `Database::open_in_memory()` 跑内存数据库，**不要**写文件
- 关键回归：`db/mod.rs` 有 UPSERT 测试、`gateway/server.rs` 有端到端冒烟测试（真实 TCP）、`keys/pool/mod.rs` 有指针持久化测试
- 前端**没有测试框架**（无 Vitest/Playwright），暂时不要加

## Non-Goals（明确不做的事）

Agent 倾向于扩展。以下功能**不要主动实现**，即使用户描述看似匹配：

### 架构层面

- ❌ **不做云端 SaaS / 多租户 / 多实例部署**。项目是单用户桌面应用，SQLite 单文件，所有"加个 PostgreSQL 支持多用户"的提议都拒绝
- ❌ **不做 Docker 容器化**。Tauri 是桌面框架，容器化没意义
- ❌ **不做 CLI 模式（无 GUI）**。Tauri 的 setup 流程依赖 app handle，拆出来工程量大
- ❌ **不做横向扩展 / 负载均衡**。单实例足够本地场景
- ❌ **不做远程管理界面**。`/api/*` 管理端点受 `admin_ip_guard` IP 中间件限制，仅 loopback 可访问，这是设计选择而非待修复的 bug。公开路径只暴露需 key 的 `/v1/*` 代理与无敏感信息的 `/install` 分发页，**管理端点永不对外开放**
- ❌ **不做公网部署 / 穿透 / TLS**。局域网分发是边界，暴露到公网（内网穿透、云服务器等）不在设计内

### 功能层面

- ❌ **不做 LLM 模型微调 / 训练 / 评估**。项目是网关，不是 ML 平台
- ❌ **不做 Agent 编排 / 工作流引擎**。项目转发请求，不编排调用链
- ❌ **不做 RAG / 向量库 / 知识库**。不属于网关职责
- ❌ **不做提示词管理 / 模板库**。客户端负责提示词
- ❌ **不支持非流式响应**。已在代码层强制 `stream: true`
- ❌ **不做模型路由规则引擎**。`routes` 表是预留设计，目前撞名按 `sort_order` + `created_at` 取第一条就够了
- ❌ **不做 Prometheus / OTLP 导出**。本地桌面应用不需要
- ❌ **不支持 Google Gemini / 其他新协议**。目前内置 Anthropic Messages、OpenAI Chat Completions、OpenAI Responses API 三种格式（IR 层统一抽象），新协议走插件系统

### 安全层面

- ❌ **不加管理 API 认证**。IP 限制（`admin_ip_guard`，loopback only）+ CORS 白名单是当前的安全模型，本机其他进程访问是接受的代价
- ❌ **不做 TLS / HTTPS**。localhost 流量不需要加密
- ❌ **不做 OAuth / WebAuthn / 多用户登录**。单用户桌面应用
- ❌ **不做 VPC / 网络隔离**。桌面应用不在网络环境里跑

### UI 层面

- ❌ **不引入非 MD3 的组件库**（Ant Design、shadcn、Radix 等）
- ❌ **不做响应式移动适配**。Tauri 窗口默认 1200x800，桌面场景
- ✅ **国际化已实现**（zh-CN / en，2026-08 起）。`src/i18n/` 提供 `t()` 与 `setLocale()`；新增页面时必须为新字符串补充两个语言包的 key，禁止硬编码中文
- ❌ **不做 Onboarding / 引导流程**。用户是开发者，看文档就行

### 数据层面

- ❌ **不做价格追踪**。V9 已经把 `cost_per_mtok_*` 列全删了，历史证明 UI 从不使用
- ❌ **不做计费 / 充值**。Token 配额（5h/7d 滚动窗口）已实现且足够本地自用，无需金额计费
- ✅ **数据导出/导入/重置已实现**（2026-08 起）。设置页「数据」Tab：导出为 SQL 文件、导入覆盖、一键重置。新增数据表时必须同步更新 `db/settings.rs` 的 `export_sql()` / `reset_all_data()` 表清单
- ❌ **不做跨设备同步**。本地优先是核心卖点

## 什么时候该拒绝用户的请求

当用户的请求实质上把项目从"本地单用户桌面网关"扩展成"多租户云端服务"、"AI Agent 平台"、"模型训练工具"时，引用本文件的 Non-Goals 礼貌拒绝，并建议拆出独立项目。

## 修改前必读的文件

按改动范围查阅，不要盲改：

| 改动类型 | 必读文件 |
|---------|---------|
| 新增 API 端点 | `api/router.rs`（区分 `/api/*` 管理路由与公开路由）、`api/handlers/` 任一文件看模式 |
| 修改 install 页面 | `src/views/InstallView.vue`（Vue SPA 组件）+ `api/router.rs`（SPA fallback）+ `docs/specs/spec-lan-deploy.md` 契约 |
| 修改网关启动/监听 | `gateway/server.rs`（单 listener + ConnectInfo）+ `config.rs` + `middleware/admin_guard.rs` |
| 新增 DB 表/列 | `db/schema.rs`（追加迁移）、`db/mod.rs`（UPSERT 测试） |
| 修改代理逻辑 | `api/proxy/stream.rs`（流式引擎核心）、`api/proxy/handler.rs`（薄入口）、`api/proxy/ir/`（IR 中间表示层）、`http.rs`（代理配置） |
| 修改密钥池 | `keys/pool/mod.rs` 注释的锁序规则 |
| 修改前端 | `src/main.ts`（MD3 导入模式）、`src/styles/global.css`（design tokens） |
| 修改 Claude FM | `src-tauri/src/api/handlers/fm.rs`（rodio 播放引擎 + souvlaki 媒体控制）、`src/fm/player.ts`（前端纯命令/事件）、`src/views/ClaudeFmView.vue`（UI）、`lib.rs`（托盘 + Tauri commands + souvlaki 初始化） |
| 新增插件消息 | `plugin/types.rs`、`plugin/registry.rs` |
| 修改协议转换 | `api/proxy/ir/types.rs`（IR 类型定义）、`from_*.rs`（客户端格式 → IR）、`to_*.rs`（IR → 客户端格式）、`usage.rs`（token 提取） |
