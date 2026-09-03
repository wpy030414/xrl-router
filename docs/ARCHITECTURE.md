# ARCHITECTURE — xrl-router

> 架构地图：描述稳定的结构关系，让 Agent 理解系统边界。

## 1. 系统总览

xrl-router 是一个 **Tauri 2 桌面应用**，内部跑着一个 Rust axum HTTP 服务（`0.0.0.0:19068`），前端 React 19 SPA 运行在 Tauri WebView 中。

```
┌─── Tauri 桌面应用 ───────────────────────────────────────────────────────┐
│                                                                           │
│  WebView (React 19 SPA)                 Rust 后端 (axum + tokio)          │
│  ┌───────────────────┐                  ┌──────────────────────────────┐ │
│  │ ProvidersView     │  HTTP (无认证)   │ /api/providers,keys,models   │ │
│  │ KeysView          │────────────────▶│ /api/stats,settings,plugins  │ │
│  │ StatsView         │                  │ /health  /api/install/local-ip│ │
│  │ SettingsView      │  WebSocket       │ /ws (实时推送)               │ │
│  │ FmView            │═════════════════▶│ /ws/plugin (插件注册)        │ │
│  │                   │                  │ /mcp (MCP 工具服务器)        │ │
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
│  └─ /mcp (MCP 工具服务器)                                                │
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
       │    ├─ schema.rs      MIGRATIONS 数组 (V1→V16)
       │    ├─ providers.rs   Provider CRUD
       │    ├─ models.rs      Model CRUD
       │    ├─ api_keys.rs    API Key CRUD
       │    ├─ service_keys.rs Service Key CRUD (含 allowed_models)
       │    ├─ usage.rs       usage_log 查询 + 统计聚合
       │    └─ settings.rs    key-value 设置表 + 导出/导入/重置
       ├─ gateway/server.rs  AppState + start_gateway (单 listener) + CORS
       └─ api/
            ├─ router.rs      axum 路由表 (build_router)
            ├─ handlers/      管理 API
            │    ├─ health.rs, providers.rs, keys.rs, models.rs
            │    ├─ service_keys.rs, stats.rs
            │    ├─ data.rs      (/api/data/export|import|reset)
            │    ├─ install.rs  (本机出口 IP 检测)
            │    ├─ websocket.rs  (/ws 端点)
            │    ├─ plugin.rs     (插件 REST + WS)
            │    ├─ fm.rs         Claude FM 播放引擎 (含 scene_t 时钟)
            │    └─ local.rs      本地模型管理 (CRUD + 引擎生命周期 + 文件导入)
            └─ proxy/         LLM 代理核心
                 ├─ handler.rs     薄入口层: 认证 + 请求体准备
                 ├─ stream.rs      流式引擎核心: 路由解析 → 立即返回 Response → 后台 spawn
                 ├─ forward.rs     统一 IR 转发: 上游字节 → IR 事件 → 客户端 SSE 字节
                 ├─ auth.rs        Service Key 验证
                 ├─ quota.rs       5h/7d token 配额检查
                 ├─ route.rs       模型别名→上游 URL 解析
                 ├─ failover.rs    provider 级冷却表
                 ├─ key_rotation.rs 密钥选取 + 健康反馈
                 └─ ir/            IR 中间表示层
                      ├─ types.rs                IrRequest / IrMessage / IrContentBlock / IrStreamEvent / IrUsage
                      ├─ from_messages.rs        Anthropic Messages → IR
                      ├─ from_chat_completions.rs  OpenAI Chat Completions → IR
                      ├─ from_responses.rs       OpenAI Responses API → IR
                      ├─ to_messages.rs              IR → Anthropic Messages
                      ├─ to_chat_completions.rs  IR → OpenAI Chat Completions
                      ├─ to_responses.rs         IR → OpenAI Responses API
                      └─ usage.rs                Token usage 提取

独立模块：
  ├─ mcp/               MCP 工具服务器 (/mcp 端点)
  │    ├─ mod.rs        /mcp handler + 全局服务单例
  │    ├─ tools.rs      ServerHandler 实现 (web_search / web_fetch / notify)
  │    ├─ fetch.rs      WebFetch 渲染层 (WebView + 静态回退)
  │    └─ notify.rs     桌面通知工具
  ├─ providers/          Provider 适配器
  │    ├─ adapter.rs     Adapter async trait
  │    ├─ anthropic.rs   AnthropicAdapter
  │    └─ openai.rs      OpenAIAdapter
  ├─ models/mod.rs       ModelRegistry (DashMap 缓存)
  ├─ keys/pool/          KeyPool (RwLock HashMap)
  ├─ plugin/             PluginManager
  ├─ middleware/rate_limit.rs  令牌桶
  ├─ search/bing.rs           Bing 搜索
  ├─ wallpaper/               桌面壁纸劫持（FM 像素艺术 → 桌面层）
  │    ├─ mod.rs              WallpaperState + 建窗/挂载/重建
  │    ├─ win.rs              Windows 透明 WebView + tauri-plugin-desktop-underlay
  │    └─ macos.rs            macOS kCGDesktopIconWindowLevel（objc2）
  ├─ local/                   本地模型管理（私有化部署）
  │    ├─ mod.rs              LocalManager（导入/启动/停止/删除/自启动/崩溃重启）
  │    ├─ engine.rs           llama-server 引擎二进制管理 + 健康检查
  │    └─ backend.rs          GPU 后端检测（Metal/CUDA/Vulkan/ROCm/CPU）
  └─ types/                   数据结构定义
```

## 3. 数据流：一次 LLM 请求的完整生命周期

```
客户端 POST /v1/messages / /v1/chat/completions / /v1/responses
  │
  ▼
[1] rate_limit_middleware ──── 令牌桶检查 (per Service Key)
  │
  ▼
[2] proxy handler (handler.rs) ──── 认证 + 配额 + 请求体准备
  │  verify_service_key → Argon2 逐条校验
  │  check_quota → 5h/7d 滚动窗口用量聚合
  │  allowed_models 白名单检查
  │  客户端格式 → IR (from_messages / from_chat_completions / from_responses)
  │
  ▼
[3] proxy_stream (stream.rs) ──── 流式引擎核心
  │
  ▼
[3a] resolve_combo / resolve_route / resolve_route_candidates (route.rs)
  │  模型名是 enabled 组合 → resolve_combo: 按成员 position 逐个展开候选
  │  failover_enabled=false → 仅主 provider
  │  failover_enabled=true  → 全部候选 (同 display_name, 按 sort_order 排序)
  │  成功 → ResolvedRoute { upstream_url, provider_kind, real_model_id, ... }
  │
  ▼
[3b] 搜索工具剔除（MCP 模式）──── mcp_websearch 开关
  │  ON  → strip_search_tools: 移除请求自带的搜索类工具
  │  OFF → 完全不碰工具定义
  │
  ▼
[3c] 上下文超限预警 ──── 估算输入 token (chars/4) > model.context_window
  │  超限 → warn 日志（不阻断请求）
  │
  ▼
[3d] IR → 上游格式渲染 (to_messages / to_chat_completions / to_responses)
  │  上游强制 stream=true, model=real_model_id（客户端 stream: false 时收集 SSE → JSON）
  │
  ▼
[3e] failover 双层重试循环 (stream.rs + key_rotation.rs + failover.rs)
  │  外层: 遍历 provider 候选 (冷却中的直接跳过)
  │  内层: pick_key_for() → round-robin, 跳过 Red/Yellow
  │  http::build_http_client() → 自动继承系统代理
  │  发送请求 → 自适应头超时 (300/480/600s 按估算输入 token)
  │  401/403 → mark_key_invalid(Red) → 换 key
  │  402/429 → mark_key_low_quota(Yellow) → 换 key
  │  5xx / 网络错误 / 头超时 → 切下一个 provider 并标记冷却
  │  2xx → mark_provider_ok(清冷却) → break
  │
  ▼
[3f] 流式转发 (forward.rs 统一 IR 路径)
  │  forward_stream_ir() ──── 单一函数处理所有格式组合
  │           └─ 上游字节 → 按 provider_kind 解析为 IR 事件 → 按 client_format 渲染为客户端 SSE
  │           └─ 立即返回 Response + :keepalive 初始字节
  │           └─ 后台 spawn 转发 + 15s keepalive 心跳
  │  120s chunk 间隔超时
  │
  ▼
[3g] 异步记录 usage_log ──── provider/model/key/service_key + token 用量
  │  usage 真实值覆盖估算占位
  │
  ▼
SSE 流返回客户端
```

## 4. 前端架构

```
src/
├── main.tsx           React 入口 + initI18n（壁纸窗口按 __WALLPAPER_MODE__ 分支）
├── App.tsx            根组件: RouterProvider + WebSocket 连接
├── api.ts             REST 客户端 (动态 BASE_URL)
├── ws.ts              WebSocket 客户端 (自动重连 3s)
├── index.css          Tailwind CSS + CSS 变量
├── i18n/              Zustand + useT hook: zh-CN.ts / en.ts
├── lib/               工具函数 (utils.ts, tauri.ts)
├── hooks/             自定义 hooks (useTheme, useWebSocket, useFm)
│
├── views/
│    FmView.tsx            Claude FM 视图（右键菜单：设置为桌面背景）
│    ProvidersView.tsx     供应商列表（拖拽排序）
│    ProviderFormView.tsx  供应商创建/编辑（支持插件供应商编辑）
│    KeysView.tsx          Service Key 管理（创建时设置模型白名单）
│    StatsView.tsx         用量统计（Recharts 图表 + 数字翻动动画）
│    SettingsView.tsx      设置 3 Tab（通用/路由/隐私）
│    InstallView.tsx       局域网分发页
│    CombosView.tsx        组合列表
│    ComboFormView.tsx     组合创建/编辑
│    LocalModelsView.tsx   本地模型管理（导入/启动/停止/编辑）
│
├── components/
│    AppShell.tsx              导航抽屉 + Windows 自定义窗口控制
│    ConnectionStatus.tsx      离线横幅
│    PluginRegisterDialog.tsx  插件注册确认（自监听事件）
│    WallpaperScene.tsx        壁纸窗口入口（黑底全屏像素，无按钮）
│    PixelScene.tsx            像素画布（sampleT 引擎时钟采样）
│    ui/                       shadcn/ui 组件（含 context-menu）
│
└── stores/ (Zustand)
     providers.ts    Provider 列表
     keys.ts         API Key 列表
     models.ts       Model 列表
     settings.ts     应用设置
     combos.ts       组合管理
     localModels.ts  本地模型状态
     ui.ts           UI 状态
```

### 4.1 Claude FM 桌面壁纸（wallpaper 引擎）

FM 像素艺术可被劫持为桌面壁纸（关闭即恢复原壁纸）：

- **Windows（透明 WebView + 社区插件，见 ADR-043）**：`tauri-plugin-desktop-underlay`
  的 `set_desktop_underlay` 把透明 WebView 窗口 SetParent 进壁纸 WorkerW。
  WebView2 内容经 DWM 视觉合成上屏，是桌面 WorkerW 层唯一可靠渲染路径。
  点击穿透为 `WS_EX_TRANSPARENT`（禁 `WS_EX_LAYERED`），见 `wallpaper/win.rs`。
- **macOS（WebView 方案，ADR-041）**：动态创建第二个 WebviewWindow
  （`initialization_script` 注入 `__WALLPAPER_MODE__`，前端分支渲染
  `WallpaperScene`——黑底全屏像素、无按钮/歌曲信息），
  `NSWindow.setLevel(kCGDesktopIconWindowLevel)` + `orderFront:` 呈现，
  `setIgnoresMouseEvents` 穿透。

**同步机制**：像素场景动画时钟为**引擎权威**（`fm.rs` 的 `scene_t`，仅
播放时按真实流逝累计、暂停冻结）；主窗口前端（`fm_scene_t` 采样）与
壁纸渲染（同一共享状态）取同一值——两处画面严格同步。seed（曲目 index）
与 playing 沿用既有全窗口广播事件（`fm-meta` / `fm-state-changed`）。

**生命周期**：勾选态持久化于 DB `settings.wallpaper_enabled`，应用重启
（含 `--minimized` 静默启动）延迟 2s 惰性恢复（+重试，避开主窗口 WebView2
初始化竞态）；进程退出由 OS 销毁子窗口，原壁纸自动恢复。

### 4.2 Windows 自定义窗口控制

Windows 平台去除原生标题栏，自定义红绿灯风格窗口控制按钮（关闭/最小化/最大化），
位于侧边栏左上角。拖拽区域高度 40px（macOS 保持 28px）。

## 5. 存储架构

```
┌─ SQLite (WAL 模式) ─────────────────────────────┐
│                                                   │
│  providers        供应商注册表 (含 sort_order)     │
│  models           模型定义 (含别名 display_name)   │
│  combos           组合别名                         │
│  combo_members    组合成员                         │
│  api_keys         Provider Key (AES-256-GCM 加密) │
│  service_keys     客户端 Key (Argon2 哈希)        │
│                   (含 allowed_models JSON 白名单)  │
│  usage_log        请求日志 (自包含快照)            │
│  settings         key-value 设置 + 轮询指针        │
│  plugins          插件注册记录                     │
│  schema_version   迁移版本跟踪                     │
│                                                   │
│  local_models     本地模型注册（GGUF 引擎参数）      │
│  routes           路由规则 (预留, 未使用)           │
└───────────────────────────────────────────────────┘

┌─ 文件系统 ────────────────────────────────────────┐
│  master.key       AES-256-GCM 主密钥 (权限 0600)  │
│  xrl-router.db    SQLite 数据库                   │
│  dist/            Vite 构建产物                    │
└───────────────────────────────────────────────────┘

┌─ 纯内存 ──────────────────────────────────────────┐
│  KeyPool          密钥健康状态                      │
│  ProviderRegistry DashMap 缓存                    │
│  ModelRegistry    DashMap 缓存                     │
│  RateLimiter      令牌桶状态                       │
│  PluginManager    插件连接状态                     │
│  LocalManager     本地模型引擎句柄 + 下载状态       │
│  mcp_websearch    AtomicBool                      │
│  mcp_webfetch     AtomicBool                      │
│  mcp_notify       AtomicBool                      │
│  http_client      reqwest::Client                  │
│  search_http      SearchHttp (搜索专用, 直连)       │
└───────────────────────────────────────────────────┘
```

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
              (令牌桶限流 + 5h/7d token 配额)
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

| 路径类型 | 绑定 | 路由 | 访问方 |
|----------|------|------|--------|
| 公开 | `0.0.0.0:19068` | `/health`、`/ws`、`/api/ui-settings`、`/v1/*` 代理、`/assets/*`、SPA fallback | Tauri WebView、本机客户端、局域网设备 |
| 管理（IP 限制） | 同上（`admin_ip_guard` 中间件限 loopback） | `/api/*` CRUD、`/api/install/local-ip`、`/api/data/*` | 仅本机 |

### 6.2 文件系统权限

前端 WebView 的文件访问仅限**导出/导入对话框选中的文件**：路径白名单（`$HOME/**`、`$DOWNLOAD/**`、`$DOCUMENT/**`、`$DESKTOP/**`、`$APPDATA/**`）。

## 7. 关键设计约束

| 约束 | 原因 |
|------|------|
| 上游始终走流式 | 简化实现；客户端非流式时收集 SSE 事件后返回 JSON |
| SQLite 单文件 | 本地单用户场景足够，WAL 模式缓解并发 |
| 密钥状态纯内存 | 减少 DB 写入开销，启动全 green 可接受 |
| 轮询指针持久化 | 重启后跳过已失效的 key |
| usage_log 无 FK | 删除 Provider/Model/Key 不影响历史统计 |
| 管理 API 无认证 | `admin_ip_guard` IP 中间件限 loopback 是安全模型 |
| IR 真实值覆盖估算值 | `chars/4` 估算值偏大，max 合并会永久压住真实值 |
| 上下文超限预警而非硬拒绝 | 估算口径偏保守，硬拒绝会阻断客户端 auto-compact |
| WebSearch/WebFetch 走本地 MCP | 旧 server-side 劫持循环体验差且复杂 |
| WebFetch 用内置 WebView 渲染 | 不自动下载浏览器，懒创建隐藏 WebView 窗口 |
| failover 冷却纯内存 | 与密钥健康同一哲学，不持久化不广播 |
| 数据导出用 SQL dump | SQLite 原生语句保真度高，导入即执行 |
| 壁纸窗口用透明 WebView | DWM 视觉合成是桌面 WorkerW 层唯一可靠渲染路径（Windows） |

## 8. 外部依赖关系

```
xrl-router
  ├── Tauri 2          桌面框架
  ├── tauri-plugin-autostart  开机自启
  ├── tauri-plugin-dialog     导出/导入文件对话框
  ├── tauri-plugin-fs         读写 .sql 备份文件
  ├── tauri-plugin-shell      外链打开
  ├── tauri-plugin-desktop-underlay  Windows 壁纸劫持（WorkerW 挂载）
  ├── llama-server (外部二进制)     本地 GGUF 模型推理引擎
  ├── axum 0.7         HTTP 框架
  ├── tokio            异步运行时
  ├── rusqlite 0.32    SQLite (bundled)
  ├── aes-gcm 0.10     Provider Key 加密
  ├── argon2 0.5       Service Key 哈希
  ├── dashmap 6        并发 HashMap
  ├── tracing          结构化日志
  ├── reqwest 0.12     HTTP 客户端
  ├── scraper 0.20     HTML 解析 (Bing 搜索)
  ├── rmcp 3           MCP Rust SDK
  ├── htmd 0.2         HTML → Markdown
  ├── rodio 0.20       音频播放
  ├── souvlaki 0.8     系统媒体控制
  ├── windows 0.61     Windows API（壁纸穿透样式）
  ├── objc2            macOS API（壁纸层级）
  ├── React 19         UI 框架
  ├── Zustand          状态管理
  ├── shadcn/ui        UI 组件（基于 Radix UI + Tailwind CSS）
  ├── lucide-react     图标库
  ├── react-router v8       路由
  ├── Recharts         统计图表
  ├── @dnd-kit/core    拖拽排序
  └── @tanstack/react-virtual  虚拟滚动
```

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

## 10. MCP 工具服务器

网关内置 MCP（Streamable HTTP）端点 `/mcp`，提供三个工具：

| 工具 | 功能 | 开关 |
|------|------|------|
| `web_search` | 本地 Bing 搜索 | `mcp_websearch` |
| `web_fetch` | Tauri WebView 渲染页面后取正文 Markdown | `mcp_webfetch` |
| `notify` | 发送系统桌面通知 | `mcp_notify` |

**鉴权**：与 `/v1/*` 代理一致，`Authorization: Bearer <service-key>`（Argon2 校验）。

**会话模式**：无状态（`NeverSessionManager`）——工具只有三个且无服务端推送。

**代理侧配合**：`mcp_websearch` 开关开启时，剔除请求自带的搜索类工具（防上游官方搜索生效）。
