# xrl-router — 架构文档

> 架构地图：描述稳定的结构关系，让 Agent 理解系统边界。通常半年甚至一年不变。

---

## 1. 系统总览

xrl-router 是一个 **Tauri 2 桌面应用**，内部跑着一个 Rust axum HTTP 服务（`0.0.0.0:19068`），前端 Vue 3 SPA 运行在 Tauri WebView 中喵～

```
┌─── Tauri 桌面应用 ───────────────────────────────────────────────────────┐
│                                                                           │
│  WebView (Vue 3 SPA)                    Rust 后端 (axum + tokio)          │
│  ┌───────────────────┐                  ┌──────────────────────────────┐ │
│  │ ProvidersView     │  HTTP (无认证)   │ /api/providers,keys,models   │ │
│  │ KeysView          │────────────────▶│ /api/stats,settings,plugins  │ │
│  │ StatsView         │                  │ /health  /api/install/local-ip│ │
│  │ SettingsView      │  WebSocket       │ /ws (实时推送)               │ │
│  │                   │═════════════════▶│ /ws/plugin (插件注册)        │ │
│  └───────────────────┘                  └──────────────────────────────┘ │
│                                        (同一进程, 单 listener :19068)   │
└───────────────────────────────────────────────────────────────────────────┘

外部 LLM 客户端 (Claude Code / 其他)
    │
    │  x-api-key: xrl-xxxx (Service Key)
    │  POST /v1/messages / /v1/chat/completions / /v1/responses
    ▼
┌───────────────────────────────────────────────────────────────────────────┐
│               单 listener 绑 0.0.0.0:19068 (axum)                         │
│                                                                           │
│  公开路径（不限 IP）              /api/* 管理路径（IP 中间件限 loopback）  │
│  ├─ /v1/* 代理 (service key)     ├─ CRUD: providers,keys,models          │
│  ├─ /api/ui-settings (公开)      ├─ stats, settings, plugins             │
│  ├─ /health  /  /ws  /ws/plugin  ├─ install/local-ip                     │
│  ├─ /assets/* (ServeDir)         └─ data/export,import,reset             │
│  ├─ SPA fallback → index.html                                           │
│  └─ 同一套 proxy_routes                                                   │
│                                                                           │
│  请求入口 → 认证 → 路由解析 → 密钥选取 → 协议转换 → 上游转发 → 流式回传    │
│                                                                           │
│       ┌──────────────────────────────────────────────────────────┐        │
│       │ 所有上游: 统一 IR 转发 (forward_stream_ir)               │        │
│       │   上游字节 → IR 事件 → 客户端 SSE 字节                    │        │
│       │ 插件上游: 插件自行转换，Router 只管密钥轮换                │        │
│       └──────────────────────────────────────────────────────────┘        │
└───────────────────────────────────────────────────────────────────────────┘
```

---

## 2. 后端模块依赖图

```
main.rs
  └─ lib.rs (Tauri setup + 启动流程 + 插件注册 + 托盘 i18n)
       ├─ config.rs          环境变量 → Config
       ├─ error.rs           AppError 统一错误类型 (thiserror)
       ├─ crypto/mod.rs      AES-256-GCM + Argon2 + master key
       ├─ http.rs            统一 HTTP 客户端工厂（系统代理自动继承）
       ├─ db/                SQLite 封装
       │    ├─ mod.rs         Database 结构体 + WAL + migrate()
       │    ├─ schema.rs      MIGRATIONS 数组 (V1→V15)
       │    ├─ providers.rs   Provider CRUD
       │    ├─ models.rs      Model CRUD
       │    ├─ api_keys.rs    API Key CRUD
       │    ├─ service_keys.rs Service Key CRUD
       │    ├─ usage.rs       usage_log 查询 + 统计聚合 + 请求日志分页
       │    └─ settings.rs    key-value 设置表 + 导出/导入/重置
       ├─ gateway/server.rs  AppState + start_gateway (单 listener) + CORS
       └─ api/
            ├─ router.rs      axum 路由表 (build_router)
            ├─ handlers/      管理 API
            │    ├─ health.rs, providers.rs, keys.rs, models.rs
            │    ├─ service_keys.rs, stats.rs (含 /api/stats/requests 分页 + failover 开关)
            │    ├─ data.rs      (/api/data/export|import|reset)
            │    ├─ install.rs  (本机出口 IP 检测 /api/install/local-ip)
            │    ├─ websocket.rs  (/ws 端点)
            │    ├─ plugin.rs     (插件 REST + WS)
            │    └─ fm.rs         Claude FM 播放引擎 (rodio + souvlaki, std::thread)
            └─ proxy/         LLM 代理核心
                 ├─ handler.rs     薄入口层: 认证 + 请求体准备 (~300 行)
                 ├─ stream.rs      流式引擎核心: 路由解析 → 立即返回 Response → 后台 spawn 双循环 (~630 行)
                 ├─ forward.rs     统一 IR 转发: 上游字节 → IR 事件 → 客户端 SSE 字节 (~350 行)
                 ├─ auth.rs        Service Key 验证
                 ├─ quota.rs       5h/7d token 配额检查
                 ├─ route.rs       模型别名→上游 URL 解析 (resolve_route / resolve_route_candidates)
                 ├─ failover.rs    provider 级冷却表 (纯内存, 60s)
                 ├─ key_rotation.rs 密钥选取 + 健康反馈
                 ├─ sniff.rs       SniffStream (透传+嗅探，保留但当前未被 forward.rs 引用)
                 └─ ir/            IR 中间表示层（三种协议统一抽象）
                      ├─ types.rs                IrRequest / IrMessage / IrContentBlock / IrStreamEvent / IrUsage
                      ├─ from_messages.rs        Anthropic Messages → IR
                      ├─ from_chat_completions.rs  OpenAI Chat Completions → IR
                      ├─ from_responses.rs       OpenAI Responses API → IR
                      ├─ to_messages.rs              IR → Anthropic Messages
                      ├─ to_chat_completions.rs  IR → OpenAI Chat Completions
                      ├─ to_responses.rs         IR → OpenAI Responses API
                      └─ usage.rs                Token usage 提取（三种格式）

独立模块（被 handler/proxy 使用）：
  ├─ http.rs             统一 HTTP 客户端工厂（系统代理自动继承）
  │    ├ system_proxy()  解析环境变量 → Windows 注册表 → macOS scutil，OnceLock 缓存
  │    ├ build_http_client()  返回带代理的 reqwest ClientBuilder
  │    └ http_client()   便捷方法：默认构建
  ├─ providers/          Provider 适配器
  │    ├─ adapter.rs     Adapter async trait (chat/chat_stream/health_check)
  │    ├─ anthropic.rs   AnthropicAdapter 实现
  │    └─ openai.rs      OpenAIAdapter 实现
  ├─ models/mod.rs       ModelRegistry (DashMap 缓存)
  ├─ keys/pool/          KeyPool (RwLock HashMap)
  │    ├─ mod.rs          结构体 + 集合操作
  │    ├─ types.rs        KeyEntry + KeyPoolStats
  │    ├─ rotation.rs     round-robin 选取
  │    ├─ health.rs       mark_invalid/low_quota/success
  │    └─ persistence.rs  load_all_keys_from_db + 指针持久化
  ├─ plugin/             PluginManager
  │    ├─ mod.rs          结构体 + DB helpers
  │    ├─ registry.rs     register/confirm/disconnect
  │    ├─ keys.rs         keys_update 同步
  │    ├─ health.rs       check_heartbeats (30s/90s)
  │    └─ types.rs        消息类型定义
  ├─ middleware/rate_limit.rs  令牌桶 (128 req/min)
  ├─ search/bing.rs           Bing 搜索 (HTTP 浏览器头 + cookie 复用 + 懒预热 + 双域名 fallback + ck/a 重定向解码，绕过代理直连)
  └─ types/                   数据结构定义
       ├─ provider.rs    ProviderKind / ProviderConfig / DelegateKeyConfig
       ├─ model.rs       Capability / ModelTier
       ├─ key.rs         KeyStatus (Green/Yellow/Red/Unknown)
       ├─ chat.rs        聊天相关类型
       ├─ route.rs       Route 结构体
       └─ balance.rs     BalanceInfo
```

---

## 3. 数据流：一次 LLM 请求的完整生命周期

```
客户端 POST /v1/messages / /v1/chat/completions / /v1/responses
  │
  ▼
[1] rate_limit_middleware ──── 令牌桶检查 (per Service Key)
  │
  ▼
[2] proxy_messages / proxy_chat_completions / proxy_responses (handler.rs)
  │
  ├─ 提取 x-api-key / Authorization: Bearer
  │
  ▼
[3] authenticate_and_stream (handler.rs) ──── 认证 + 配额 + 请求体准备
  │  verify_service_key → Argon2 逐条校验
  │  check_quota → 5h/7d 滚动窗口用量聚合
  │  allowed_models 白名单检查
  │  客户端格式 → IR (from_messages / from_chat_completions / from_responses)
  │  失败 → 401/403/429
  │
  ▼
[4] proxy_stream (stream.rs) ──── 流式引擎核心
  │
  ▼
[4a] resolve_combo / resolve_route / resolve_route_candidates (route.rs)
  │  模型名是 enabled 组合 → resolve_combo: 按成员 position 逐个展开候选
  │    (跨成员按 (provider_id, real_model_id) 去重保序; 成员不可解析跳过)
  │  failover_enabled=false → 仅主 provider (resolve_route, 历史行为)
  │  failover_enabled=true  → 全部候选 (同 display_name, 按 sort_order 排序,
  │                           按 provider_id 去重, 跳过离线插件 provider)
  │  失败 → 400
  │  成功 → ResolvedRoute { upstream_url, provider_kind, real_model_id, context_window, ... }
  │  委托供应商 → 从 PluginManager 取实时 base_url
  │  (组合命中后强制 failover 语义, 见 4e)
  │
  ▼
[4b] 搜索工具剔除（MCP 模式）──── mcp_websearch 开关
  │  ON  → strip_search_tools (stream.rs):
  │    移除请求自带的搜索类工具（客户端 `WebSearch` / 上游 server-side `web_search_*`），
  │    防止上游官方搜索生效；tool_choice 指向被移除工具时改写为 Auto。
  │    模型联网搜索走客户端注册的本地 MCP（/mcp 的 web_search 工具）。
  │  OFF → 完全不碰工具定义
  │
  ▼
[4c] 上下文超限预警 ──── 估算输入 token (chars/4) > model.context_window
  │  超限 → warn 日志（不阻断请求）
  │  原因：chars/4 估算偏保守；硬拒绝会阻断客户端 auto-compact 死锁
  │  message_start 携带 input_tokens / cache_read_input_tokens
  │  （上游真实值或估算值），供客户端上下文条感知
  │
  ▼
[4d] IR → 上游格式渲染 (to_messages / to_chat_completions / to_responses)
  │  强制 stream=true, model=real_model_id
  │  同协议: passthrough (IR 渲染结果即上游格式)
  │  异协议: IR 渲染为目标上游格式
  │
  ▼
[4e] failover 双层重试循环 (stream.rs + key_rotation.rs + failover.rs)
  │  外层: 遍历 provider 候选 (冷却中的直接跳过)
  │  failover = 全局开关 || 组合请求 (组合成员间回退强制生效)
  │  内层: pick_key_for() → round-robin, 跳过 Red/Yellow
  │  http::build_http_client() → 自动继承系统代理
  │  发送请求 → 自适应头超时 header_timeout_for() (300/480/600s 按估算输入 token)
  │  401/403 → mark_key_invalid(Red) → 换 key (内层继续)
  │  402/429 → mark_key_low_quota(Yellow) → 换 key (内层继续)
  │  5xx / 网络错误 / 头超时 → 有后续候选: mark_provider_failed(60s 冷却) → 切 provider
  │     无后续候选: 网络错误 → 502, 头超时 → 504, 5xx → 透传上游错误
  │  2xx → mark_provider_ok(清冷却) + 记 winner → break
  │  无 winner: key 4xx 耗尽透传最后一次上游失败响应 / 无可用 key → 503
  │  组合特殊: 普通 400 (非配额) → 立即透传, 不试下一成员 (请求级错误)
  │
  ▼
[4f] 流式转发 (forward.rs 统一 IR 路径)
  │  forward_stream_ir() ──── 单一函数处理所有格式组合
  │           └─ 上游字节 → 按 provider_kind 解析为 IR 事件 → 按 client_format 渲染为客户端 SSE
  │           └─ 立即返回 Response + :keepalive 初始字节
  │           └─ 后台 spawn 转发 + 15s keepalive 心跳 (oneshot 取消信号驱动, 见 ADR-021)
  │           └─ 响应头: Cache-Control: no-cache, Connection: keep-alive, X-Accel-Buffering: no
  │           └─ 请求体上限 64MiB (MAX_REQUEST_BODY_BYTES, 覆盖多模态大会话)
  │  120s chunk 间隔超时
  │
  ▼
[4g] 异步记录 usage_log ──── provider/model/key/service_key + token 用量
  │  usage 真实值覆盖估算占位（max → 覆盖，避免 chars/4 估算值偏大压住真实值）
  │  Responses input_tokens 减去缓存命中部分（增量口径）
  │
  ▼
SSE 流返回客户端
```

---

## 4. 前端架构

```
src/
├── main.ts            Vue 入口 + MD3 组件按需导入 + initI18n + initSystemThemeListener
├── App.vue            根组件: AppShell（install 页面隐藏）+ PluginRegisterDialog + router-view
├── router.ts          8 条路由 (8 个 lazy-loaded 组件路由 + 1 个 redirect, 含 /install)
├── api.ts             REST 客户端 (动态 BASE_URL: Tauri/localhost → 19068, LAN 浏览器 → 当前 origin)
├── ws.ts              WebSocket 客户端 (自动重连 3s, 事件 pub/sub)
├── theme.ts           主题 light/dark/system (localStorage 持久化, prefers-color-scheme 监听, 同步到后端)
├── i18n/              自研 i18n: index.ts (t/setLocale/initI18n, localStorage + 后端托盘 + 后端 settings 同步) + zh-CN.ts / en.ts
├── fm/                Claude FM 前端（极简 ~60 行：纯命令/事件，无音频逻辑）
│    └─ player.ts      通过 Tauri command 控制播放，监听 fm-meta/fm-ready 事件
│
├── styles/
│    global.css                全局样式 (MD3 design tokens + [data-theme="dark"])
│
├── views/
│    ClaudeFmView.vue   Claude FM 视图（大圆按钮 + 曲目 caption，从 fm-meta 事件读取）
│    ProvidersView.vue    供应商列表 (网格卡片 + 拖拽排序 + WS 实时 key 统计)
│    ProviderNewView.vue  供应商创建/编辑 (支持插件模式)
│    KeysView.vue         Service Key 管理 (表格 + 权限对话框 + 分发链接)
│    StatsView.vue        用量统计 (数据磁贴 + Chart.js 折线图 + 请求日志分页表)
│    SettingsView.vue     设置 3 Tab: 通用(语言/主题/开机启动) + 路由(MCP WebSearch/WebFetch/Vision/接入信息/failover) + 数据(导出/导入/重置)
│    InstallView.vue      局域网分发页: 消费端选择(Claude Code/ChatGPT) + 模型下拉 + 命令生成 (LAN 浏览器可访问)
│
├── components/
│    AppShell.vue              MD3 导航抽屉 (响应式, 使用 MdiIcon 渲染图标)
│    ConnectionStatus.vue      离线横幅 + 重试
│    PluginRegisterDialog.vue  插件注册确认对话框
│    MdiIcon.vue               动态 MDI 图标 (@mdi/js SVG path, kebab-case icon name)
│
└── stores/ (Pinia)
     providers.ts    Provider 列表
     keys.ts         API Key 列表 (按 provider 分组)
     models.ts       Model 列表 (按 provider 分组)
```

前端通过 HTTP 访问管理 API（无认证），通过 WebSocket 接收实时推送。Tauri API（window/event/core）通过动态 `import()` 延迟加载，LAN 浏览器访问 install 页面时不触发 Tauri 依赖报错。语言切换经 `set_locale` Tauri command + 后端 settings 表同步（托盘菜单文本 + LAN install 页面语言），开机启动经 `@tauri-apps/plugin-autostart`，外链打开经 `@tauri-apps/plugin-shell`，数据文件读写经 `plugin-dialog` + `plugin-fs`（路径白名单见安全边界）。Claude FM 通过 `fm_toggle` / `fm_play` / `fm_pause` / `fm_get_state` Tauri command 控制播放，通过 `fm-meta` / `fm-ready` / `fm-state-changed` 事件接收状态更新。

---

## 5. 存储架构

```
┌─ SQLite (WAL 模式) ─────────────────────────────┐
│                                                   │
│  providers        供应商注册表 (含 sort_order)     │
│  models           模型定义 (含别名 display_name)   │
│  combos           组合别名 (V18: 多个别名捆绑)     │
│  combo_members    组合成员 (member_alias 软引用)   │
│  api_keys         Provider Key (AES-256-GCM 加密) │
│  service_keys     客户端 Key (Argon2 哈希, 含 quota)│
│  usage_log        请求日志 (自包含快照, 无 FK)      │
│  settings         key-value 设置 + 轮询指针 +       │
│                   failover_enabled / locale 等     │
│  plugins          插件注册记录                     │
│  schema_version   迁移版本跟踪                     │
│                                                   │
│  routes           路由规则 (预留, 未使用)           │
└───────────────────────────────────────────────────┘

┌─ 文件系统 ────────────────────────────────────────┐
│  master.key       AES-256-GCM 主密钥 (权限 0600)  │
│  xrl-router.db    SQLite 数据库                   │
│  dist/            Vite 构建产物（ServeDir 托管）    │
└───────────────────────────────────────────────────┘

┌─ 纯内存 ──────────────────────────────────────────┐
│  KeyPool          密钥健康状态 (启动全 green)       │
│  ProviderRegistry DashMap 缓存                    │
│  ModelRegistry    DashMap 缓存 (按 tier 索引)      │
│  RateLimiter      令牌桶状态                       │
│  PluginManager    插件连接状态                     │
│  mcp_websearch    AtomicBool                      │
│  mcp_webfetch     AtomicBool                      │
│  http_client      reqwest::Client (共享连接池)     │
│  search_http      SearchHttp (搜索专用, 直连)       │
└───────────────────────────────────────────────────┘
```

---

## 6. 安全边界

```
                    外部客户端 (本机 + 局域网设备)
                        │
            ┌───────────┼───────────────┐
            │     Service Key 认证      │
            │     (Argon2 哈希验证)     │
            └───────────┼───────────────┘
                        │
              /v1/messages, /v1/chat/completions, /v1/responses, /v1/models, /v1/user/balance
              (令牌桶限流 128 req/min + 5h/7d token 配额)
                        │
                        ▼
                    xrl-router
                        │
            ┌───────────┼───────────────┐
            │    Provider API Key       │
            │    (AES-256-GCM 解密)     │
            └───────────┼───────────────┘
                        │
                        ▼
                    上游 LLM API
```

### 6.1 单 listener + 路径级 IP 限制

| 路径类型 | 绑定 | 路由 | 访问方 | CORS |
|----------|------|------|--------|------|
| 公开 | `0.0.0.0:19068` (`HOST:PORT`) | `/health`、`/ws`、`/ws/plugin`、`/api/ui-settings`、`/v1/*` 代理、`/assets/*` (ServeDir)、SPA fallback (`index.html`) | Tauri WebView、本机客户端、局域网设备 | origin 白名单（7 个） |
| 管理（IP 限制） | 同上（`admin_ip_guard` 中间件限 loopback） | `/api/*` CRUD、`/api/install/local-ip`、`/api/data/*` | 仅本机（Tauri WebView、CC Switch 等） | origin 白名单（7 个） |

- `/v1/*` 由 `proxy_routes()` 构建（套 `rate_limit_middleware` + 64MiB body limit）
- `admin_ip_guard` 中间件用 `ConnectInfo<SocketAddr>` 提取客户端 IP，非 loopback 返回 403
- `server.rs` 使用 `into_make_service_with_connect_info::<SocketAddr>()` 启用 IP 提取
```

### 6.2 文件系统权限（capabilities）

前端 WebView 的文件访问仅限**导出/导入对话框选中的文件**：`fs:allow-read-text-file` / `fs:allow-write-text-file` 带路径白名单（`$HOME/**`、`$DOWNLOAD/**`、`$DOCUMENT/**`、`$DESKTOP/**`、`$APPDATA/**`），配合 `dialog:default`（系统文件对话框）——WebView 无任意路径读写能力。

---

## 7. 关键设计约束

| 约束 | 原因 |
|------|------|
| 代理仅支持流式 | Claude Code 等客户端始终流式，加非流式增加复杂度无收益 |
| SQLite 单文件 | 本地单用户场景足够，WAL 模式缓解并发 |
| 密钥状态纯内存 | 减少 DB 写入开销，启动全 green 可接受 |
| 轮询指针持久化 | 重启后跳过已失效的 key |
| usage_log 无 FK | 删除 Provider/Model/Key 不影响历史统计 |
| 管理 API 无认证 | `admin_ip_guard` IP 中间件限 loopback 是安全模型，本机进程访问是接受的代价；公开路径只暴露需 key 的 `/v1/*`、无敏感信息的 `/api/ui-settings` 与 LAN install 页面 |
| IR 真实值覆盖估算值 | `forward.rs` 预填的 `chars/4` 估算值偏大，max 合并会永久压住真实值，污染 usage_log 与客户端上下文条 |
| Responses input_tokens 增量口径 | 减去 `cached_tokens`，与 Chat Completions `prompt_tokens - cached_tokens` 一致 |
| 上下文超限预警而非硬拒绝 | 估算口径偏保守，硬拒绝会阻断客户端 auto-compact（死锁） |
| WebSearch/WebFetch 走本地 MCP | 旧 server-side 劫持循环（代理跑 tool loop + 缓冲中间轮）体验差且复杂；改为 `/mcp` Streamable HTTP 端点让客户端注册标准 MCP 工具，模型直接调用，代理仅在开关开启时剔除请求自带搜索工具防上游官方搜索生效（详见 `docs/DECISIONS.md` 与 `docs/specs/spec-mcp-tools.md`） |
| WebFetch 用内置 WebView 渲染 | 不自动下载浏览器、不探测本机 Chrome/Edge；懒创建隐藏 WebView 窗口（macOS WKWebView / Windows WebView2 / Linux WebKitGTK）渲染页面（JS 执行）后提取正文；渲染失败回退静态抓取并注明（详见 `docs/DECISIONS.md`） |
| web_vision 视觉识别 | 设置页指定「视觉专用模型」（provider + model）；`/mcp` 提供 `web_vision`：网关取图（http(s)/本地路径，8MiB 上限）→ base64 → 按 ProviderKind 构造非流式请求调上游，返回描述文本；不计配额（详见 `docs/DECISIONS.md` ADR-039） |
| failover 冷却纯内存 | 与密钥健康同一哲学，不持久化不广播；开关默认关闭，开启才改变请求行为 |
| 数据导出用 SQL dump | SQLite 原生语句保真度高，导入即执行，天然支持跨版本迁移 |

---

## 8. 外部依赖关系

```
xrl-router
  ├── Tauri 2          桌面框架 (WebView + 系统托盘)
  ├── tauri-plugin-autostart  开机自启 (LaunchAgent + --minimized)
  ├── tauri-plugin-dialog     导出/导入文件对话框
  ├── tauri-plugin-fs         读写 .sql 备份文件
  ├── tauri-plugin-shell      外链打开 (SettingsView 内 openUrl)
  ├── axum 0.7         HTTP 框架
  ├── tokio            异步运行时
  ├── rusqlite 0.32    SQLite (bundled)
  ├── aes-gcm 0.10     Provider Key 加密
  ├── argon2 0.5       Service Key 哈希
  ├── dashmap 6        并发 HashMap
  ├── tracing          结构化日志 (JSON)
  │
  │  网络基础设施
  ├── reqwest 0.12     HTTP 客户端 (流式 SSE, cookie 复用, 系统代理继承)
  ├── scraper 0.20     HTML 解析 (Bing 搜索结果提取)
  ├── url 2            URL 构造 (Bing 搜索参数)
  ├── base64 0.22      base64url 解码 (Bing ck/a 重定向链接)
  │
  │  MCP 工具服务器 (/mcp 端点)
  ├── rmcp 3           MCP Rust SDK (Streamable HTTP server, 无会话模式)
  ├── htmd 0.2         HTML → Markdown (渲染后正文转 Markdown 喂模型)
  │
  │  序列化 / 工具
  ├── thiserror 1      错误类型派生
  ├── anyhow 1         通用错误处理
  ├── chrono 0.4       时间处理
  ├── uuid 1           UUID 生成 (v4)
  ├── once_cell 1      延迟初始化 (system_proxy 缓存)
  ├── async-trait 0.1  异步 trait 支持
  │
  │  音频 / 媒体控制
  ├── rodio 0.20       音频播放 (MP3 解码 + 系统音频设备输出)
  ├── souvlaki 0.8     系统媒体控制 (macOS Now Playing / Windows SMTC / Linux MPRIS)
  │
  │  前端
  ├── Vue 3            UI 框架
  ├── Pinia            状态管理
  ├── @material/web    MD3 组件
  ├── @mdi/js          MDI 图标 (SVG path, 按需加载)
  ├── Chart.js + vue-chartjs   统计图表
  └── SortableJS       拖拽排序
```

---

## 9. 插件系统交互

```
xrl-router-plugin-wukong (外部进程)
    │
    │  WebSocket /ws/plugin
    │◀═══════════════════════════▶  register + heartbeat + keys_update
    │
    │  HTTP POST /v1/chat/completions
    │════════════════════════════▶  Router 带密钥发请求到插件的 base_url
    │
    │                              插件注入 DEAP 业务头 + 协议转换
    │                              POST https://api-deap.dingtalk.com/...
    │◀════════════════════════════  返回结果
```

**Router 管**: 密钥轮换、健康监控、用量统计、路由解析
**Plugin 管**: 非标→标准协议转换、业务头注入、base_url/api_path 提供
