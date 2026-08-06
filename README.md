# xrl-router

> 多 Provider AI LLM API 路由网关 — Tauri 2 桌面应用

xrl-router 是一个运行在本地的 LLM API 统一网关。客户端通过一套 Anthropic Messages API 端点访问所有大模型 Provider，网关负责路由解析、协议转换、密钥轮换和用量统计喵～

## 技术栈

| 层 | 技术 |
|---|---|
| 后端 | Rust (edition 2021) + Tauri 2 + axum 0.7 + tokio |
| 数据库 | SQLite 3 (rusqlite 0.32 bundled, WAL 模式) |
| HTTP 客户端 | reqwest 0.12 (流式 SSE) |
| 加密 | aes-gcm 0.10 (Provider Key 加密) + argon2 0.5 (Service Key 哈希) |
| 前端 | Vue 3 + Pinia + Vue Router 4 |
| UI | Material Web Components (MD3) + MDI 图标 + Chart.js + vue-chartjs + SortableJS (拖拽排序) + 自研 i18n (zh-CN/en) |
| 桌面插件 | tauri-plugin-autostart (开机自启) + tauri-plugin-dialog/fs (数据导出导入) + tauri-plugin-shell (外链打开) |
| 构建 | Vite 8 (前端) + Cargo (后端) |

## 快速开始

### 前置要求

- **Rust** >= 1.75.0
- **Node.js** >= 20 + **pnpm**
- Tauri CLI 已包含在 devDependencies 中，`pnpm dev` 自动调用

### 安装与运行

```bash
# 前端依赖
pnpm install

# 开发模式（前端 :5173 + 后端 :19068）
pnpm dev

# 生产构建（macOS .dmg / Windows .msi）
pnpm build
```

### 接入 CC Switch

- **Base URL**：`http://localhost:19068`
- **API Key**：在应用内「密钥管理」页创建的 Service Key
- **模型**：使用应用内配置的模型别名（网关负责路由到真实上游）
- **余额查询**：使用 TokenPlan 模板所需的 ZenMux 兼容格式，请求地址 `http://localhost:19068/v1/user/balance?zenmux`，API Key 同上方配置的 API Key
- **配额**：Service Key 可在「密钥管理」页配置 5h/7d 滚动窗口 token 上限，触顶返回 429（`quota_error` + `retry-after`）

### 局域网分发（install 页面）

把本机变成局域网 API 网关：在「密钥管理」页创建密钥后，弹窗里复制「分发链接」，发给局域网设备打开。页面按平台生成单行命令（装 Claude Code CLI + 写 `~/.claude/settings.json` 指向本机网关），复制到终端运行一次即可。详见 [docs/specs/spec-lan-deploy.md](docs/specs/spec-lan-deploy.md)。

> 网关双端口：管理端口 `19068` 仅本机（`127.0.0.1`），公共端口 `19069` 绑 `0.0.0.0` 供局域网设备访问 `/install` 与 `/v1/*`。局域网分发需放行防火墙 19069 端口。

### 配置

通过环境变量（均有默认值）：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `PORT` | `19068` | 管理 HTTP 监听端口 |
| `HOST` | `127.0.0.1` | 管理绑定地址 |
| `PUBLIC_HOST` | `0.0.0.0` | 公共监听绑定地址（局域网分发） |
| `PUBLIC_PORT` | `19069` | 公共监听端口（install 页面 + `/v1/*` 代理） |
| `ENABLE_PUBLIC` | `true` | 是否启用公共 listener（`1`/`true`） |
| `DB_PATH` | _(系统数据目录)_ | SQLite 文件路径 |
| `LOG_LEVEL` | `info` | 日志级别 |
| `API_KEY` | _(无)_ | 预留 API Key 字段（当前未启用认证） |
| `CORS_ORIGINS` | `localhost:5173/19068,127.0.0.1:5173/19068,tauri://localhost,https://tauri.localhost,http://tauri.localhost` | 允许的跨域来源（共 7 个） |

**代理**：上游请求自动继承系统代理（环境变量 `HTTPS_PROXY`/`HTTP_PROXY` → Windows 注册表系统代理），`localhost`/`127.0.0.1` 自动豁免直连（插件系统上游在本机）。国内网络下钉钉 DEAP 等上游需走 Clash 等代理才能连通。

首次启动自动在系统应用数据目录创建数据库（14 版迁移）和主密钥文件：
- macOS: `~/Library/Application Support/im.xrl.router/`
- Linux: `~/.config/im.xrl.router/`
- Windows: `C:\Users\<user>\AppData\Roaming\im.xrl.router\`

## 核心技术

### LLM 代理

客户端请求 `/v1/messages`（Anthropic 格式）或 `/v1/chat/completions`（OpenAI 格式），网关根据模型别名解析到上游 Provider，进行协议转换后流式转发。仅支持流式响应。

**流式引擎架构**：handler.rs 是薄入口层（认证 + 请求体准备），核心逻辑委托给 stream.rs 的 `proxy_stream()` 函数完成路由解析、上游连接、密钥轮换和流式转发。stream.rs 路由解析后立即返回 Response（含 keepalive），上游等待和流式转发在后台 spawn 中完成，客户端毫秒级收到首字节。流式转发分支（passthrough / O→A / A→O）实现在 forward.rs。

**SSE 优化**：为避免客户端因等待上游响应（可能耗时数秒）而超时断开，passthrough 路径立即返回 HTTP Response（含 `:keepalive` 初始字节），后台 spawn 处理上游数据流。响应头包含 `Cache-Control: no-cache`、`Connection: keep-alive`、`X-Accel-Buffering: no`，并每 15 秒发送 keepalive 心跳（oneshot 取消信号驱动，主任务结束即停心跳，修复流式响应永不结束），确保连接存活。

**自适应超时 + 请求体放宽**：上游响应头超时不再固定 60s，改为按估算输入 token 自适应（≥100k → 600s、≥50k → 480s、基准 300s），对齐 Claude Code 客户端超时，避免大上下文长排队被误判挂死而断流。请求体上限从 axum 默认 2MiB 放宽到 64MiB，覆盖多轮历史 + 工具结果 + base64 截图的多模态大会话。

客户端消费端配置见 [接入 CC Switch](#接入-cc-switch)。

### 密钥池

每个 Provider 可配多个 API Key，round-robin 轮询调度。上游返回 401/403 标红永久跳过，402/429 标黄冷却 5 分钟，2xx 恢复绿色。轮询指针持久化到 settings 表，重启后从上次位置继续。

### 故障转移（Provider Failover）

同一模型别名下配置多个 Provider 时，可开启故障转移（设置页「路由」Tab，默认关闭）：上游 5xx / 网络错误 / 响应头超时自动切换到下一个候选 Provider，并对失败的 Provider 冷却 60 秒（纯内存，不持久化）。开启后请求依次尝试候选（按 `sort_order` 排序），key 级 4xx（401/402/403/429）仍优先在 Provider 内换 key 耗尽后才切 Provider。关闭时行为与单 Provider 完全一致。

### 安全

- Provider API Key: **AES-256-GCM** 加密存储，主密钥独立于数据库（`master.key`，权限 0600）
- Service Key: **Argon2** 哈希存储（随机盐），创建时仅返回一次明文
- 管理 API 绑定 `127.0.0.1`（admin listener），CORS origin 白名单；公共 listener（`0.0.0.0`）只暴露 `/v1/*`（需 key 鉴权）与 `/install` 页面，管理接口局域网不可达

### 局域网分发（install 页面）

密钥管理页创建密钥后可复制「分发链接」（`http://<本机IP>:19069/install?t=<明文key>`），局域网设备打开即得按平台生成的一行命令：装 Claude Code CLI + 写 `~/.claude/settings.json`（`ANTHROPIC_AUTH_TOKEN` + `ANTHROPIC_BASE_URL` 指向网关，模型别名走下拉选择）。密钥明文嵌入 URL，仅限可信设备，撤销即在密钥列表删除。

### 插件系统

外部服务通过 WebSocket 注册为「委托供应商」，将非标 API 桥接为标准接口。Router 负责密钥轮换和用量统计，插件负责协议转换和业务头注入。

### 系统托盘 + 开机静默启动

关闭窗口后应用隐藏到系统托盘，网关继续运行。开启「开机静默启动」（设置页通用 Tab）后，系统登录时以 `--minimized` 拉起应用，窗口不显示、网关照常运行、驻留托盘；托盘菜单语言跟随应用语言设置（zh-CN / en）。

### Claude FM（后端广播电台引擎）

应用内置一台「电台」：所有播放逻辑（歌单管理、墙钟时间轴、音源解析、预加载、切歌）由 Rust 后端 `FmEngine` 完成。引擎以 `tokio::spawn` 后台任务持续运行，通过 `broadcast::channel` 将音频字节推送给所有订阅者。前端输出一条永不关闭的 HTTP chunked 直播流（`GET /api/fm/live`），前端只需一个 `<audio>` 标签像收音机一样收听——路由切换不销毁 `<audio>`，窗口关闭只隐藏到托盘，音乐持续。音源就绪后托盘菜单加入「Claude FM」勾选项（勾选播放 / 取消暂停）；曲目切换由 `fm-meta` Tauri 事件主动推送给前端。

### WebSocket 实时推送

前端通过 WebSocket 接收密钥状态变更、用量更新等实时事件（3 秒自动重连）。

### WebSearch 劫持

可选功能：拦截包含 `web_search` 工具的请求，用本地 Bing 搜索替代上游 API 的搜索功能（通过设置页开关控制）。

### 国际化（i18n）

全应用支持 zh-CN / en 双语言（2026-08 起）：前端 `src/i18n/` 极简实现（`t()` + `setLocale()`，localStorage 持久化），后端托盘菜单与 install 分发页同步双语。主题支持 `light / dark / system` 三态，system 模式跟随操作系统（修复了 Tauri 窗口主题强制导致跟随失效的问题）。

### 数据管理

设置页「数据」Tab：**导出**为 SQL dump 文件（.sql，可跨版本迁移）、**导入**覆盖恢复（导入前有确认对话框）、**一键重置**全部数据（保留 schema 版本）。导出/导入通过系统文件对话框选择路径，需要 `tauri-plugin-dialog` + `tauri-plugin-fs`。

### 请求日志

统计页新增「请求日志」区块：分页表格（每页 10 条，时间逆序），展示时间 / 密钥 / 供应商 / 模型 / 输入输出 tokens / 成功失败状态（失败可悬停查看错误信息），配合用量图表使用。

## 当前状态

核心功能已完成，详见 [docs/PRD.md](docs/PRD.md) 路线图。

## 项目文档

```
docs/
├── PRD.md          — 产品需求文档（功能存在的意义）
├── ARCHITECTURE.md — 架构地图（稳定的结构关系）
├── DECISIONS.md    — 架构决策记录（历史原因）
└── specs/          — 代码生成契约（可独立完成的任务单元）
```

## CI

push 到 main 自动构建 macOS arm64 (.dmg) 和 Windows amd64 (.msi) 安装包，发布到 GitHub Releases。
