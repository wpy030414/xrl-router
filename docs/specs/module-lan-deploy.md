# Spec: 局域网快速部署 Claude Code / ChatGPT（install 页面 + 分发链接）

## 目标

让主人把本机作为局域网 API 网关：局域网设备打开一个 install 页面，复制一行命令到终端，
即可安装客户端 CLI 并把配置指向本机网关。支持两种消费端：**Claude Code**（`~/.claude/settings.json`）和 **ChatGPT/Codex**（`~/.codex/config.toml` + `auth.json`）。

## 技术约束

浏览器沙箱无法直接安装 CLI、无法改写客户端配置文件。最接近"自动"的形态
是 install 页面生成单行安装命令，用户复制粘贴到终端执行一次。

## 架构：单 listener + 路径级 IP 限制 + SPA fallback

```
┌──────────────────── xrl-router (axum) ────────────────────┐
│  单 listener  0.0.0.0:19068                                │
│  公开路径（不限 IP）：                                       │
│    /health  /ws  /ws/plugin  /api/ui-settings  /v1/*       │
│    /assets/* (ServeDir, 前端构建产物)                         │
│    所有未匹配 GET → SPA fallback (index.html)              │
│  管理路径（admin_ip_guard 限 loopback）：                    │
│    /api/*  /api/install/local-ip  /api/data/*  /api/settings │
└────────────────────────────────────────────────────────────┘
```

**职责分工**:
- **公开路径**：`/health`、`/ws`、`/ws/plugin`、`/api/ui-settings`（主题/令牌色/语言）、`/v1/*` 代理（需 service key 鉴权）、`/assets/*`（前端构建产物，`tower_http::ServeDir`）、SPA fallback（`index.html`，React Router 处理前端路由如 `/install`）。局域网设备可访问。
- **管理路径**：`/api/*` CRUD + `/api/install/local-ip` + `/api/data/*` + `/api/settings`（UI 设置写入）。仅 loopback IP（`127.0.0.1` / `::1`）可访问，非本机返回 403。

`/v1/*` 由 `proxy_routes(state)` 构建（套 `rate_limit_middleware` + 64MiB body limit）。
`admin_ip_guard` 中间件用 `ConnectInfo<SocketAddr>` 提取客户端 IP，`server.rs` 使用
`into_make_service_with_connect_info::<SocketAddr>()` 启用 IP 提取。

## 配置

`Config`（`src-tauri/src/config.rs`）字段，环境变量覆盖：

| 字段 | 默认值 | 环境变量 |
|---|---|---|
| `host` | `0.0.0.0` | `HOST` |
| `port` | `19068` | `PORT` |

`addr()` → listener 地址。管理端点通过 `admin_ip_guard` 中间件限制 loopback，无需单独端口。

## 路由

`build_router(state)`（`api/router.rs`）：统一路由表，公开路径与管理路径共存。
`/api/*` 子路由挂 `middleware::from_fn(admin_ip_guard)` 层，非 loopback 返回 403。

路由包括：
- 公开：`/health`、`/ws`、`/ws/plugin`、`/api/ui-settings`、`/assets/*` (ServeDir)、SPA fallback
- 管理（IP 限制）：`/api/providers`、`/api/keys`、`/api/models`、`/api/stats`、`/api/settings`、`/api/plugins`、`/api/install/local-ip`、`/api/data/*`
- 代理：`/v1/chat/completions`、`/v1/messages`、`/v1/models`、`/v1/user/balance`（套 `rate_limit_middleware`）

SPA fallback（`spa_fallback()`）：所有未匹配 axum 路由的 GET 请求返回 `dist/index.html`（`DIST_DIR` 环境变量或 `../dist` 默认），由 React Router 接管前端路由。

## install 页面契约 — `src/views/InstallView.tsx`

React SPA 组件，通过 React Router `/install` 路由访问。LAN 浏览器打开分发链接时，后端 SPA fallback 返回 `index.html`，前端 React Router 匹配 `/install` 路由渲染 InstallView。

**输入**：URL query `?t=<明文 service key>`。无 `t` → 显示"请从密钥管理页获取分发链接"占位。

**UI 设置同步**：页面加载时调 `GET /api/ui-settings` 读取管理端的 `theme`/`hue`/`locale`，同步应用到 LAN 页面。API 不可用时 fallback 到 URL `?lang=` 参数 > 浏览器语言（en 前缀 → English，否则中文）。

**消费端选择**：分段按钮切换 Claude Code / ChatGPT 两种客户端，命令实时重新生成。

**模型选择**：页面打开时用 `t` 调 `GET /v1/models`（`x-api-key: <t>`，同源 19068 无 CORS 问题），
取可用别名列表（`data[].id` = display_name）。默认选中第一项（后端按 tier 排序，通常为主模型）。
拉取失败则提示可忽略，但客户端会用官方模型名请求而 404。

**平台切换**：UA 自动检测（Windows → PowerShell / 其他 → macOS Bash），可手动切换。

### Claude Code 命令

按平台（Windows PowerShell / macOS·Linux Bash）生成单行命令。命令由两段组成：
- A 段：`npm i -g @anthropic-ai/claude-code`（装 CLI）——**注**：当前 InstallView 省略了 A 段（无 CLI 安装勾选框），仅生成 B 段配置写入命令
- B 段：写 `~/.claude/settings.json`，合并 `env` 块与顶层配置（**保留既有字段**）：

```jsonc
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "<key>",
    "ANTHROPIC_BASE_URL": "<页面 origin>",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "<别名>",
    "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME": "<别名>",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "<别名>",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": "<别名>",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "<别名>",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": "<别名>",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "<别名>",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "<别名>",
    "CLAUDE_CODE_SUBAGENT_MODEL": "<别名>"
  }
}
```

未选中别名（拉取失败/网关无模型）→ 省略全部 `_MODEL_NAME`/`SUBAGENT` 行，只写 base+token。

`ANTHROPIC_BASE_URL` 只填到端口（`http://<IP>:19068`），不带 `/v1/messages`（Claude Code 自拼）。

**引号约定**：PS 用单引号字符串（key/base/别名不含单引号）；bash 里整个脚本包在
`node -e "..."` 双引号中，值一律用**单引号**包裹（双引号值会截断外层引号）。

### ChatGPT/Codex 命令

写 `~/.codex/config.toml` + `~/.codex/auth.json`：

**config.toml**:
```toml
model = "<别名>"
model_provider = "xrl"

[model_providers.xrl]
name = "XRL Router"
base_url = "<页面 origin>/v1"
```

**auth.json**:
```json
{"OPENAI_API_KEY":"<key>"}
```

Windows PowerShell 用 `Set-Content` 写文件；macOS/Linux 用 `cat >` heredoc + `printf`。

## 公开端点：`/api/ui-settings`

`get_ui_settings()`（`api/handlers/stats.rs`）返回管理端的 UI 设置，**公开端点**（不受 `admin_ip_guard` 限制），供 LAN install 页面读取：

```json
{
  "theme": "light|dark|system",
  "hue": 264,
  "locale": "zh-CN|en"
}
```

数据来源：`settings` 表的 `theme`/`hue`/`locale` 键（默认 `system`/`264`/`zh-CN`）。

设置写入通过管理端点 `PUT /api/settings`（仅 loopback 可访问），前端 `theme.ts` 和 `i18n/index.ts` 在主题/语言切换时自动同步到后端。

## 本机 IP + 端口

`GET /api/install/local-ip`（管理端点，仅 loopback）返回：

```json
{ "ip": "192.168.1.100", "port": 19068 }
```

- `ip`：UDP socket 连 8.8.8.8:80（不发数据）取本机出口 IP，过滤回环
- `port`：`state.config.port`，用于 KeysView 拼分发链接

## 分发链接 — 密钥管理页

`KeysView`「密钥已创建」dialog（创建 key 后弹出，明文仅此一次）:
- 内容区：明文密钥框 + 分发链接框（`http://<本机IP>:<端口>/install?t=<明文key>`）。
- actions：「复制」+「复制分发链接」+「完成」。
- 本机 IP + 端口由 `GET /api/install/local-ip` 返回。端口从接口动态获取（不再硬编码 `19069`）。

分发 key 即普通长期 service key（argon2 哈希，明文仅创建时返回），撤销就在密钥列表删除。
**系统不自动删除/提示删除**——删不删由主人手动决定。

## 安全模型

- 单端口 `0.0.0.0:19068`：局域网任何人可调 `/v1/*`（需有效 key）、可看 `/install`
  （无 key 仅显示占位页）、可访问 `/api/ui-settings`（无敏感信息）。
- 管理 `/api/*` 受 `admin_ip_guard` 中间件限制，仅 loopback IP 可访问，局域网返回 403。
- 明文 key 在 install URL query：局域网嗅探可见，主人复制给特定用户，撤销即删。

## 鉴权

install 页面用 URL 里的 `t` 调 `/v1/models` 获取模型列表（`x-api-key: <t>`）。`/v1/*` 复用现有 `verify_service_key`
（`api/proxy/auth.rs`）：`/v1/messages` 优先读 `x-api-key` 回退 `Authorization: Bearer`；
Claude Code 用 `ANTHROPIC_AUTH_TOKEN` 走 `Authorization: Bearer`，网关已兼容。

## 动态 BASE_URL

`src/api.ts` 的 `getBaseUrl()` 按运行环境动态选择：
- **Tauri/localhost**（hostname 为 `localhost`/`127.0.0.1` 或 protocol 为 `tauri:`）→ `http://127.0.0.1:19068`
- **LAN 浏览器**（hostname 为本机局域网 IP）→ 使用当前 origin（`${protocol}//${hostname}:${port}`），避免 CORS

前端代码（`App.tsx`、`theme.ts`、`fm/player.ts`）通过动态 `import()` 延迟加载 Tauri API（`@tauri-apps/api/*`），LAN 浏览器访问时不触发 Tauri 依赖报错。`App.tsx` 在 `/install` 路由时隐藏 AppShell + ConnectionStatus，install 页面全屏展示。

## 完成条件

1. `cargo build` 通过；`tsc --noEmit` 通过。
2. 本机 `curl 127.0.0.1:19068/health` → 200；局域网 `curl <本机IP>:19068/health` → 200（公开路径不限 IP）。
3. 局域网 `curl <本机IP>:19068/api/providers` → 403（admin_ip_guard 拦截）。
4. 局域网 `curl <本机IP>:19068/api/ui-settings` → 200（公开端点，返回 theme/hue/locale）。
5. 局域网浏览器开 `http://<本机IP>:19068/install`（无 t）→ 显示占位页。
6. 密钥管理页创建密钥 → dialog 显示明文 key + 分发链接 → 复制分发链接。
7. 局域网设备打开分发链接 → 看到消费端选择 + 平台命令 → 终端运行 → 配置文件含正确 base URL/token。
8. Claude Code：`settings.json` 含 `env.ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN` 且其他字段保留。
9. ChatGPT/Codex：`config.toml` 含 `base_url`、`auth.json` 含 `OPENAI_API_KEY`。
10. 设备上的客户端发消息 → 网关统计页见流量（`/v1/*` 命中、key 鉴权通过）。
11. 密钥列表删除该 key → 该设备再用同 key → 401。
