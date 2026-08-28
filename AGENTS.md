# AGENTS.md

本文件为 AI Agent 在 xrl-router 项目上工作时必须遵守的边界与约束。

## Overview

xrl-router 是一个**单用户本地 LLM API 网关**，以 Tauri 2 桌面应用形式运行。

- **后端**：Rust + axum HTTP 服务，单 listener 绑 `0.0.0.0:19068`
- **前端**：React 18 SPA，跑在 Tauri WebView 里
- **数据库**：SQLite 本地文件
- **核心功能**：客户端通过 Anthropic Messages / OpenAI Chat Completions / OpenAI Responses API 三种端点访问所有大模型 Provider，网关经 IR 中间表示层统一协议转换，负责路由解析、密钥轮换和用量统计

## Boundaries and scope

### In scope

- 本地单用户桌面网关
- 三种 LLM API 协议互转（Anthropic ↔ OpenAI ↔ Responses）
- 密钥池管理（轮询 + 健康监控 + 持久化）
- 插件系统（委托供应商，WebSocket 注册）
- 局域网分发（install 页面 + 分发链接）
- MCP 工具服务器（web_search / web_fetch / notify）
- 国际化（zh-CN / en）
- 数据导出/导入/重置
- Claude FM 桌面壁纸劫持（Windows/macOS）

### Non-Goals（明确不做的事）

Agent 倾向于扩展。以下功能**不要主动实现**：

#### 架构层面

- ❌ **不做云端 SaaS / 多租户 / 多实例部署**。项目是单用户桌面应用，SQLite 单文件
- ❌ **不做 Docker 容器化**。Tauri 是桌面框架，容器化没意义
- ❌ **不做 CLI 模式（无 GUI）**。Tauri 的 setup 流程依赖 app handle
- ❌ **不做横向扩展 / 负载均衡**。单实例足够本地场景
- ❌ **不做远程管理界面**。`/api/*` 管理端点受 `admin_ip_guard` IP 中间件限制，仅 loopback 可访问
- ❌ **不做公网部署 / 穿透 / TLS**。局域网分发是边界

#### 功能层面

- ❌ **不做 LLM 模型微调 / 训练 / 评估**。项目是网关，不是 ML 平台
- ❌ **不做 Agent 编排 / 工作流引擎**。项目转发请求，不编排调用链
- ❌ **不做 RAG / 向量库 / 知识库**。不属于网关职责
- ❌ **不支持 Google Gemini / 其他新协议**。目前内置三种格式（IR 层统一抽象），新协议走插件系统

#### 安全层面

- ❌ **不加管理 API 认证**。IP 限制（loopback only）+ CORS 白名单是当前的安全模型
- ❌ **不做 TLS / HTTPS**。localhost 流量不需要加密
- ❌ **不做 OAuth / WebAuthn / 多用户登录**。单用户桌面应用

#### UI 层面

- ❌ **不引入非 shadcn/ui 的组件库**（Ant Design、MUI、Arco Design 等）
- ❌ **不做响应式移动适配**。Tauri 窗口默认 1200x800，桌面场景
- ✅ **国际化已实现**（zh-CN / en）。新增页面时必须为新字符串补充两个语言包的 key

## Agent operating guide

### 关键约定

#### 数据目录

生产环境的数据目录由 Tauri 的 `app.path().app_data_dir()` 解析（macOS: `~/Library/Application Support/im.xrl.router/`），**不要**在代码里硬编码相对路径 `data/`。

#### 数据库迁移

- 迁移定义在 `src-tauri/src/db/schema.rs` 的 `MIGRATIONS` 数组
- 每个元素是一条完整 SQL，启动时按序执行
- 新增迁移：追加到数组末尾，**不要**修改已有迁移
- 用 `ON CONFLICT DO UPDATE`（UPSERT），**不要用** `INSERT OR REPLACE`（会触发 `ON DELETE CASCADE`）

#### 密钥双轨

- **Provider API Key**：AES-256-GCM 加密存储，主密钥在 `master.key`
- **Service Key**：Argon2 哈希存储，创建时仅返回一次明文，可设置 `allowed_models` 白名单
- 不要混淆这两套

#### 代理流式/非流式

上游**始终**走流式（`ir_request.stream = true`）。客户端 `stream: false` 时，引擎收集全部 SSE 事件后返回完整 JSON（`client_wants_stream` 标志控制）。非流式路径不引入额外逻辑，仅影响输出格式。

#### 代理代码组织

- **handler.rs**：薄入口层（认证 + 请求体准备 + 保存 `client_wants_stream`），委托 stream.rs
- **stream.rs**：流式引擎核心（路由解析 → 立即返回 Response → 后台 spawn 双循环）
- **forward.rs**：统一 IR 转发（上游字节 → IR 事件 → 客户端 SSE 字节）
- **ir/**：协议转换核心（三种客户端格式 ↔ IR）
- 新增代理逻辑时，应修改 stream.rs / forward.rs / ir/ 而非 handler.rs

#### IR 中间表示层

- IR 以 Anthropic Messages 为骨架，并集覆盖三种格式字段
- usage 合并策略：**真实值覆盖估算值**（不用 max）
- Responses `input_tokens` 需减去 `cached_tokens`，保持增量口径

#### 密钥池

- 健康状态**纯内存**（启动全 green）
- **轮询指针**持久化到 `settings` 表
- 锁序生死攸关：`keys/pool/mod.rs` 注释里有详细规则

#### 单 listener + 路径级 IP 限制

- **单 listener** 绑 `0.0.0.0:19068`
- **`/api/*` 管理端点**：仅 loopback IP 可访问（`admin_ip_guard` 中间件）
- **公开端点**：`/health`、`/ws`、`/api/ui-settings`、`/v1/*` 代理——不限 IP
- **静态文件 + SPA fallback**：`/assets/*` + 未匹配 GET fallback 到 `index.html`

#### 前端

- UI 用 shadcn/ui（基于 Radix UI + Tailwind CSS），**不要**引入其他组件库
- 颜色用 Tailwind 工具类（如 `bg-primary`, `text-muted-foreground`），**不要**硬编码 hex
- `api.ts` 的 `BASE_URL` 是动态解析的（Tauri/localhost vs LAN）
- **非 Tauri 环境兼容**：前端代码通过动态 `import()` 延迟加载 Tauri API
- 状态管理用 Zustand，**不要**用 Redux 或其他状态库
- 路由用 react-router v8
- **插件对话框**：`PluginRegisterDialog` 自监听 `plugin-register` 事件，无需父组件传递

#### HTTP 客户端

所有出站 HTTP 请求**必须**使用 `http::build_http_client()` 或 `http::http_client()`，**不要**直接 `reqwest::Client::new()`。统一工厂自动继承系统代理。**唯一例外**：`search/bing.rs` 的 `SearchHttp` **不**走统一工厂（Bing 对代理出口 IP 返回降级结果）。

### 测试

- 测试写在内联 `#[cfg(test)] mod tests` 块里，**不要**新建 `tests/` 目录
- 用 `Database::open_in_memory()` 跑内存数据库，**不要**写文件
- 前端**没有测试框架**，暂时不要加

### 什么时候该拒绝用户的请求

当用户的请求实质上把项目从"本地单用户桌面网关"扩展成"多租户云端服务"、"AI Agent 平台"、"模型训练工具"时，引用本文件的 Non-Goals 礼貌拒绝。

## Directory cheat-sheet

```
src-tauri/src/
├── main.rs / lib.rs           入口 + Tauri setup
├── config.rs / error.rs       配置 + 错误类型
├── crypto/                    AES-256-GCM + Argon2
├── http.rs                    统一 HTTP 客户端工厂
├── gateway/server.rs          AppState + 网关启动
├── api/
│   ├── router.rs              axum 路由表
│   ├── handlers/              管理 API（providers/keys/models/stats/settings/data/install/plugin/fm）
│   └── proxy/                 LLM 代理核心（handler/stream/forward/ir/auth/quota/route/failover）
├── mcp/                       MCP 工具服务器（/mcp 端点）
├── db/                        SQLite 封装（schema + CRUD）
├── keys/pool/                 密钥池（轮询 + 健康 + 持久化）
├── plugin/                    插件系统（WebSocket 注册）
├── providers/                 Provider 适配器
├── search/bing.rs             Bing 搜索
├── wallpaper/                 桌面壁纸劫持（FM 像素艺术 → 桌面层）
└── types/                     数据结构定义

src/
├── main.tsx / App.tsx         前端入口（壁纸窗口按 __WALLPAPER_MODE__ 分支）
├── api.ts / ws.ts             REST + WebSocket 客户端
├── hooks/                     自定义 hooks（useTheme, useWebSocket, useFm）
├── i18n/                      国际化（zh-CN / en）
├── stores/                    Zustand stores（providers, keys, models, settings, combos, ui）
├── views/                     9 个页面视图（.tsx）
├── components/                AppShell / ConnectionStatus / PluginRegisterDialog / PixelScene / WallpaperScene + ui/（shadcn/ui 组件）
└── lib/                       工具函数（utils.ts, tauri.ts）

docs/
├── PRD.md                     产品需求文档
├── ARCHITECTURE.md            架构地图
├── DECISIONS.md               架构决策记录
├── assets/                    界面截图（fm.png, provider.png, secret.png, setting.png）
└── specs/                     代码生成契约（11 个 spec 文件）
```

## 修改前必读的文件

| 改动类型 | 必读文件 |
|---------|---------|
| 新增 API 端点 | `api/router.rs` + `api/handlers/` 任一文件 |
| 修改 install 页面 | `src/views/InstallView.tsx` + `api/router.rs` + `docs/specs/spec-lan-deploy.md` |
| 修改网关启动 | `gateway/server.rs` + `config.rs` + `middleware/admin_guard.rs` |
| 新增 DB 表/列 | `db/schema.rs`（追加迁移）+ `db/mod.rs` |
| 修改代理逻辑 | `api/proxy/stream.rs` + `handler.rs` + `ir/` |
| 修改密钥池 | `keys/pool/mod.rs` 注释的锁序规则 |
| 修改前端 | `src/main.tsx` + `src/index.css` |
| 修改 Claude FM | `api/handlers/fm.rs` + `src/views/ClaudeFmView.tsx` + `src/hooks/useFm.ts` |
| 修改协议转换 | `api/proxy/ir/types.rs` + `from_*.rs` + `to_*.rs` |
| 修改桌面壁纸 | `wallpaper/mod.rs` + `wallpaper/win.rs` / `wallpaper/macos.rs` + `src/components/WallpaperScene.tsx` |
