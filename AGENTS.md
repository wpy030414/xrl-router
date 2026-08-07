# AGENTS.md

本文件为 AI Agent 在 xrl-router 项目上工作时必须遵守的边界与约束。

## 项目范围

xrl-router 是一个**单用户本地 LLM API 网关**，以 Tauri 2 桌面应用形式运行。Rust 后端（`src-tauri/src/`）跑 axum HTTP 服务：**单 listener 绑 `0.0.0.0:19068`**，通过路径级 IP 中间件（`admin_ip_guard`）限制 `/api/*` 管理端点仅 loopback 可访问，其余路径（`/v1/*`、`/install`、`/health`、`/ws`）对外开放。Vue 3 前端（`src/`）跑在 Tauri WebView 里。所有数据存本地 SQLite。

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
│   ├── router.rs              axum 路由表（build_router）
│   ├── handlers/*             管理 API 处理器（按实体分文件；install.rs 托管 /install 页面；data.rs 数据导出/导入/重置）
│   └── proxy/*                LLM 代理核心
│       ├── handler.rs         薄入口层：认证 + 请求体准备，委托 stream::proxy_stream()
│       ├── stream.rs          流式引擎核心：路由解析 → 立即返回 Response → 后台 spawn 双循环
│       ├── forward.rs         流式转发分支：passthrough / O→A / A→O
│       ├── auth.rs            Service Key 验证
│       ├── quota.rs           5h/7d token 配额检查
│       ├── route.rs           模型别名→上游 URL 解析
│       ├── failover.rs        provider 级冷却表
│       ├── key_rotation.rs    密钥选取 + 健康反馈
│       ├── websearch.rs       Bing 劫持 loop
│       ├── sniff.rs           SniffStream (透传+嗅探)
│       └── translate/         协议转换
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
├── search/bing.rs             Bing 搜索（WebSearch 劫持用）

> **HTTP 客户端**：所有出站 HTTP 请求**必须**使用 `http::build_http_client()` 或 `http::http_client()`，**不要**直接 `reqwest::Client::new()` 或 `reqwest::Client::builder()`。统一工厂自动继承系统代理（环境变量 → Windows 注册表），`localhost`/`127.0.0.1` 自动豁免直连。

src/                           前端 Vue 3
├── main.ts / App.vue / router.ts
├── api.ts                     REST 客户端（BASE_URL 硬编码为 http://localhost:19068，含 installApi）
├── ws.ts                      WebSocket 客户端（自动重连 3s）
├── theme.ts                   明/暗/跟随系统主题（localStorage 持久化，prefers-color-scheme 监听）
├── i18n/                      自研 i18n：index.ts（t/setLocale/initI18n）+ zh-CN.ts / en.ts
├── styles/global.css          全局样式（MD3 design tokens + [data-theme="dark"]）
├── fm/                        Claude FM 播放器（极简前端：单例 <audio> 收听后端直播流，~40 行）
├── views/*                    6 个页面（ClaudeFm/Providers/ProviderNew/Keys/Stats/Settings）
├── components/*               AppShell / ConnectionStatus / PluginRegisterDialog
└── stores/*                   3 个 Pinia stores（providers/keys/models）

> **Claude FM**：所有播放逻辑（歌单、墙钟时间轴、音源解析、预加载、切歌）在 Rust 后端 `FmEngine`（`api/handlers/fm.rs`）完成。引擎以 `tokio::spawn` 后台任务运行，通过 `broadcast::channel` 推送音频字节给所有 `/fm/live` 订阅者。前端 `src/fm/player.ts`（~40 行）只有一个模块级单例 `<audio>` 收听直播流 + 监听 `fm-meta` 事件更新曲目元数据。托盘勾选经 `fm_set_playing` / `fm_ready` Tauri command 同步（见 `lib.rs`）。改 FM 逻辑改 `api/handlers/fm.rs`，改播放器 UI 改 `ClaudeFmView.vue`。

src-tauri/assets/install.html   局域网 install 静态页（include_str! 编译进二进制）

docs/                          文档（本目录）
```

## 关键约定

### 数据目录

生产环境的数据目录由 Tauri 的 `app.path().app_data_dir()` 解析（macOS: `~/Library/Application Support/im.xrl.router/`），**不要**在代码里硬编码相对路径 `data/` ——安装后的 app bundle 工作目录不可写，会导致启动闪退。

### 数据库迁移

- 迁移定义在 `src-tauri/src/db/schema.rs` 的 `MIGRATIONS` 数组
- 每个元素是一条完整 SQL，启动时按序执行
- 当前版本：**V14**（`service_keys` 增加 `quota_5h` / `quota_7d`）
- 新增迁移：追加到数组末尾，**不要**修改已有迁移
- 用 `ON CONFLICT DO UPDATE`（UPSERT），**不要用** `INSERT OR REPLACE`（会触发 `ON DELETE CASCADE` 清空子表，`db/mod.rs` 有回归测试）

### 密钥双轨

- **Provider API Key**：AES-256-GCM 加密存储到 `api_keys.key_hash`，主密钥在 `master.key`
- **Service Key**：Argon2 哈希存储到 `service_keys.key_hash`，创建时仅返回一次明文
- 不要混淆这两套；验证 Service Key 必须逐条 `verify_password`（盐随机不可比）

### 代理只支持流式

`api/proxy/handler.rs` 强制 `stream: true`。不要加非流式分支 ——Claude Code 等主流客户端始终流式，加非流式只会增加代码复杂度。

### 代理代码组织

- **handler.rs** 是薄入口层（~250 行）：提取 API key → authenticate_and_stream() → 委托 stream.rs
- **stream.rs** 是流式引擎核心（~550 行）：路由解析 → 立即返回 Response（含 keepalive）→ 后台 spawn 双循环重试 + 流式转发
- **forward.rs** 是流式转发分支（~350 行）：passthrough / O→A / A→O 三种流转发模式
- 新增代理逻辑时，应修改 stream.rs（路由/重试）或 forward.rs（流转发）而非 handler.rs
- 修改认证/配额/请求体准备时，修改 handler.rs 的 authenticate_and_stream()

### 协议转换

- 实现在 `api/proxy/translate/`，按方向分 `to_openai.rs` / `to_anthropic.rs`，共享类型在 `common.rs`
- 不兼容特性（thinking、tool_choice 等）要显式转换并记 warn 日志，不要静默丢弃

### 密钥池

- 健康状态**纯内存**（启动全 green），DB 的 `status`/`last_error` 列保留但不再读写
- **轮询指针**持久化到 `settings` 表（键名 `keypool_index_{provider_id}`）
- 锁序生死攸关：`keys/pool/mod.rs` 注释里有详细规则，违反会跟插件的 `keys_update` 形成 ABBA 死锁

### 单 listener + 路径级 IP 限制（单端口架构）

- **单 listener** 绑 `0.0.0.0:19068`（`Config.host/port`），承载全部路由
- **`/api/*` 管理端点**：仅 loopback IP（`127.0.0.1` / `::1`）可访问，由 `admin_ip_guard` 中间件拦截非本机请求返回 403
- **公开端点**：`/health`、`/ws`、`/ws/plugin`、`/install`、`/v1/*` 代理（套 `rate_limit_middleware`）——不限 IP
- **CORS**：统一使用 origin 白名单（`Config.cors_origins`）
- **`/v1/*` 端点**：由 `router.rs` 的 `proxy_routes(state)` 构建，返回未 with_state 的 `Router<AppState>`，由调用方 `.with_state` 统一收敛
- **新增端点**：管理/密钥读写端点挂 `/api/*` 子路由（自动受 IP 限制）；局域网设备该访问的路由挂公开区
- **install 页面**：静态 HTML 放 `src-tauri/assets/install.html`，用 `include_str!` 编译进二进制（零运行时文件依赖），`handlers/install.rs` 的 `serve_install_page` 返回。页面契约（`?t=` 明文 key、生成命令的引号约定等）见 `docs/specs/spec-lan-deploy.md`，改页面必须同步该 spec

### 前端

- UI 用 Material Design 3（`@material/web`），**不要**引入其他组件库
- 颜色用 CSS 变量 `var(--md-sys-color-*)`，**不要**硬编码 hex
- MWC 组件在 `main.ts` 按需导入，**不要**导入 `all.js`
- `api.ts` 的 `BASE_URL` 是写死的 `http://localhost:19068`，前端不走相对路径
- 外链打开用 `@tauri-apps/plugin-shell` 的 `open()`（如 SettingsView），不要用 `window.open`（Tauri WebView 内不可靠）

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
- ❌ **不支持 Google Gemini / 其他新协议**。目前只内置 Anthropic 和 OpenAI 两种，新协议走插件系统

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
| 修改 install 页面 | `src-tauri/assets/install.html`（include_str! 编译进二进制）+ `docs/specs/spec-lan-deploy.md` 契约 |
| 修改网关启动/监听 | `gateway/server.rs`（单 listener + ConnectInfo）+ `config.rs` + `middleware/admin_guard.rs` |
| 新增 DB 表/列 | `db/schema.rs`（追加迁移）、`db/mod.rs`（UPSERT 测试） |
| 修改代理逻辑 | `api/proxy/stream.rs`（流式引擎核心）、`api/proxy/handler.rs`（薄入口）、`api/proxy/translate/`、`http.rs`（代理配置） |
| 修改密钥池 | `keys/pool/mod.rs` 注释的锁序规则 |
| 修改前端 | `src/main.ts`（MD3 导入模式）、`src/styles/global.css`（design tokens） |
| 修改 Claude FM | `src-tauri/src/api/handlers/fm.rs`（广播电台引擎）、`src/fm/player.ts`（前端单例）、`src/views/ClaudeFmView.vue`（UI）、`lib.rs`（托盘 `fm` command） |
| 新增插件消息 | `plugin/types.rs`、`plugin/registry.rs` |
| 修改协议转换 | `api/proxy/translate/common.rs`、两个方向文件 |
