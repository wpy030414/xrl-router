# xrl-router — 产品需求文档

> 版本: CalVer (tauri 26.8.11+1600) · 更新日期: 2026-08-11
>
> 📎 [架构文档](./ARCHITECTURE.md) · [决策记录](./DECISIONS.md) · [规格契约](./specs/)

---

## 1. 背景与动机

### 1.1 问题陈述

LLM 生态的协议碎片化：Anthropic、OpenAI 等 Provider 的 API 格式互不兼容。开发者要接入多家 Provider 需维护多套客户端代码，密钥散落各处，缺乏统一的健康监控和轮换机制喵～

具体痛点：

- **Claude Code 等客户端只认 Anthropic API**，想用 OpenAI 模型（GPT-4o、DeepSeek）没有代理层
- **密钥管理分散**，每个 Provider 独立管理，哪个 Key 还能用全靠人肉记忆
- **现有方案偏服务端**——OpenRouter 是云端 SaaS、LiteLLM 是 Python 服务、one-api 需要 Docker 部署，本地开发体验差

### 1.2 为什么不用现有方案

| 方案 | 不足 |
|------|------|
| **OpenRouter** | 云端 SaaS，依赖网络，数据经第三方 |
| **LiteLLM** | Python 实现，部署重；服务端思维，无桌面体验 |
| **one-api** | Go 实现，功能丰富但无桌面客户端 |
| **Portkey** | 商业化，部分功能收费，不支持纯本地 |

### 1.3 核心洞察

> 开发者需要一个**本地优先、轻量、美观**的 LLM 网关桌面应用——像本地代理一样运行，让 Claude Code 等客户端零配置接入 Anthropic 和 OpenAI 喵～

---

## 2. 产品定位与目标

### 2.1 一句话定位

**xrl-router** — 运行在桌面上的 LLM API 统一网关，让任何客户端通过一套 API 访问所有大模型。

### 2.2 产品目标

| 目标 | 衡量方式 |
|------|---------|
| 统一接入 | 通过单一端点访问所有 Provider |
| 零摩擦启动 | 从打开应用到发出第一个请求 < 3 分钟 |
| 可靠运行 | 单个 Key 失效不影响服务 |
| 透明可观测 | 所有请求的 token 用量、延迟、成功率可追踪 |

---

## 3. 用户画像与场景

### 3.1 主要用户：AI 开发者

| 属性 | 描述 |
|------|------|
| 技术水平 | 熟悉 API 调用，了解 REST/HTTP |
| 使用频率 | 每天使用，作为日常开发基础设施 |
| 核心诉求 | 一个端点接入所有模型，密钥自动管理，本地运行 |
| 痛点 | 切换 Provider 要改代码、密钥散落各处 |

### 3.2 次要用户：Claude Code / AI IDE 用户

| 属性 | 描述 |
|------|------|
| 技术水平 | 会用终端，不一定了解 API 细节 |
| 使用频率 | 日常编码时持续使用 |
| 核心诉求 | 让 Claude Code 能用非 Anthropic 的模型 |
| 痛点 | Claude Code 只支持 Anthropic API |

### 3.3 核心使用场景

**场景 A：首次配置**
1. 打开 xrl-router 桌面应用
2. 在「供应商」页面添加 Provider（选类型、填 URL、填 Key）
3. 创建 Service Key
4. 在 Claude Code 配置 base URL 和 Service Key
5. 开始使用

**场景 B：密钥故障恢复**
1. Provider 返回 401 → Key 自动标红
2. 系统自动切换到下一个可用 Key
3. 后续请求透明继续
4. 用户稍后在面板查看红灯原因

**场景 C：插件自动发现**
1. 启动 xrl-router-plugin-wukong
2. 插件 WS 连接到 Router → 发送注册信息
3. Router 弹出确认对话框
4. 用户确认 → 委托供应商自动激活
5. 密钥自动同步，可直接在 Claude Code 中使用 DEAP 模型

**场景 D：局域网设备快速接入**
1. 主机在「密钥管理」页创建密钥 → 弹窗显示明文密钥 + 分发链接
2. 把分发链接发给局域网设备（手机/另一台电脑）
3. 设备浏览器打开链接 → 按平台显示一行命令（装 CLI + 写配置）
4. 设备复制命令到终端执行一次
5. 设备上的 Claude Code 直接通过主机网关使用所有模型，主机统计页可见流量

---

## 4. 功能需求

### 4.1 P0 — 核心功能（已实现 ✅）

| ID | 功能 | 实现位置 |
|----|------|---------|
| F-01 | **Provider CRUD** | `api/handlers/providers.rs` |
| F-02 | **API Key CRUD + 密钥池轮询** | `api/handlers/keys.rs` + `keys/pool/` |
| F-03 | **Service Key 认证（Argon2）** | `api/proxy/auth.rs` |
| F-04 | **LLM 流式代理** | `api/proxy/handler.rs`（薄入口）+ `api/proxy/stream.rs`（流式引擎）+ `api/proxy/forward.rs`（流式转发） |
| F-05 | **Anthropic ↔ OpenAI ↔ Responses 三协议 IR 转换** | `api/proxy/ir/` (from_*.rs / to_*.rs / types.rs / usage.rs) |
| F-06 | **模型别名** | `api/proxy/route.rs` |
| F-07 | **密钥健康监控（红绿灯）** | `keys/pool/health.rs` |
| F-08 | **桌面应用（Tauri 2）** | `src-tauri/` |
| F-09 | **请求超时保护（自适应头超时 + 120s chunk 间隔）** | `api/proxy/stream.rs` + `api/proxy/forward.rs` + `api/proxy/mod.rs`（`header_timeout_for()` 按估算输入规模放宽） |
| F-10 | **密钥轮询指针持久化** | `keys/pool/persistence.rs` |
| F-11 | **AES-256-GCM 加密 Provider Key** | `crypto/mod.rs` |
| F-12 | **令牌桶限流（128 req/min）** | `middleware/rate_limit.rs` |

### 4.2 P1 — 重要功能（已实现 ✅）

| ID | 功能 | 实现位置 |
|----|------|---------|
| F-13 | **用量统计（数据磁贴 + 折线图）** | `api/handlers/stats.rs` + `StatsView.vue` |
| F-14 | **模型注册 + 层级分类** | `api/handlers/models.rs` + `models/mod.rs` |
| F-15 | **Provider 启用/禁用** | `api/handlers/providers.rs` |
| F-16 | **健康检查端点** | `api/handlers/health.rs` |
| F-17 | **缓存追踪（cache_read_input_tokens）** | `api/proxy/ir/usage.rs` + `api/proxy/forward.rs`（IR 层统一提取） |
| F-18 | **WebSocket 实时推送** | `api/handlers/websocket.rs` + `ws.ts` |
| F-19 | **Service Key 白名单（allowed_models）** | `api/handlers/service_keys.rs` |
| F-20 | **usage_log 自包含快照** | `db/schema.rs` V12 |

### 4.3 P2 — 锦上添花（已实现 ✅）

| ID | 功能 | 实现位置 |
|----|------|---------|
| F-21 | **本地 MCP 工具服务器（/mcp：web_search + web_fetch + web_vision）** | `mcp/`（Streamable HTTP 端点，rmcp）+ `search/bing.rs` (SearchHttp: 浏览器头 + cookie 复用 + 懒预热 + 双域名 fallback + 绕过代理直连) + `mcp/fetch.rs`（Tauri 内置 WebView 渲染 + htmd 转 Markdown + 静态回退）+ `mcp/vision.rs`（视觉模型配置 + 取图 + 三协议上游调用）。开关开启时代理剔除请求自带搜索工具（`api/proxy/stream.rs::strip_search_tools`），取代旧 server-side 劫持循环（已删除） |
| F-22 | **模型同步（从上游拉取）** | `api/handlers/models.rs` |
| F-23 | **系统托盘** | `lib.rs` |
| F-24 | **插件系统（委托供应商）** | `plugin/` |
| F-25 | **插件密钥自动同步** | `plugin/keys.rs` |
| F-26 | **插件心跳监控（30s/90s）** | `plugin/health.rs` |
| F-27 | **供应商拖拽排序** | `ProvidersView.vue` + `api/handlers/providers.rs` (V13) |
| F-28 | **暗色模式** | `theme.ts` + `global.css` |
| F-29 | **上游模型代理获取（避 CORS）** | `api/handlers/models.rs` |
| F-30 | **应用设置（MCP WebSearch / WebFetch 开关 + 接入信息卡）** | `api/handlers/` + `SettingsView.vue` |
| F-31 | **Token 配额（5h/7d 滚动窗口）** | `api/proxy/quota.rs` + `KeysView.vue` (V14) |
| F-32 | **余额端点（/v1/user/balance）** | `api/proxy/quota.rs` |
| F-33 | **系统代理自动继承（http.rs 统一工厂 + 多平台支持）** | `http.rs`（环境变量 → Windows 注册表 → macOS scutil，OnceLock 缓存） |
| F-34 | **ConnectionStatus 绝对路径修复** | `ConnectionStatus.vue` |
| F-35 | **局域网分发（install 页面 + 分发链接）** | `api/handlers/install.rs`（local-ip 接口）+ `src/views/InstallView.vue`（Vue SPA）+ `api/router.rs`（SPA fallback）+ `KeysView.vue` |
| F-36 | **单 listener + 路径级 IP 限制** | `gateway/server.rs` + `api/router.rs` + `middleware/admin_guard.rs` + `config.rs` |
| F-37 | **故障转移（Provider Failover）** | `api/proxy/stream.rs`（双循环重试）+ `api/proxy/forward.rs`（流式转发）+ `api/proxy/failover.rs`（冷却表）+ `api/proxy/route.rs`（候选解析） |
| F-38 | **请求日志分页** | `api/handlers/stats.rs` + `db/usage.rs` + `StatsView.vue` |
| F-39 | **国际化（zh-CN/en，前端 + 托盘菜单 + install 页）** | `src/i18n/` + `lib.rs` + `src/views/InstallView.vue`（从 `/api/ui-settings` 读取语言） |
| F-40 | **主题跟随系统（light/dark/system）** | `theme.ts` |
| F-41 | **开机静默启动（--minimized 驻留托盘）** | `lib.rs` + `tauri-plugin-autostart` |
| F-42 | **数据导出/导入/重置** | `api/handlers/data.rs` + `db/settings.rs` + `SettingsView.vue` |
| F-43 | **Claude FM 播放器（后端引擎 + 系统媒体控制）** | `api/handlers/fm.rs` (rodio + souvlaki, std::thread) + `src/fm/player.ts` + `src/views/ClaudeFmView.vue` + `lib.rs` |
| F-44 | **OpenAI Responses API 支持（第三协议）** | `api/proxy/ir/from_responses.rs` + `api/proxy/ir/to_responses.rs` + `api/proxy/handler.rs::proxy_responses` |
| F-45 | **usage 真实值覆盖估算占位** | `api/proxy/ir/from_chat_completions.rs` + `api/proxy/ir/from_responses.rs` (max → 覆盖) + `api/proxy/ir/usage.rs` (Responses 增量口径) |
| F-46 | **上下文超限预警（软警告）** | `api/proxy/stream.rs` (warn 而非 400，避免阻断 auto-compact) |
| F-47 | **list_models 扩展（capabilities + max_output_tokens）** | `api/proxy/handler.rs::proxy_list_models` |
| F-48 | **V15: provider kind 统一命名** | `db/schema.rs`（`openai` → `chat_completions`、`anthropic` → `messages`） |
| F-49 | **Bing 搜索策略升级（SearchHttp + 浏览器头 + 双域名 fallback）** | `search/bing.rs`（SearchHttp 结构体 + 懒预热 + ck/a 解码 + 降级检测） |
| F-50 | ~~WebSearch server-side tool 渲染（Messages 客户端搜索卡片）~~（已删除，随 server-side 劫持循环一并移除；模型改用客户端注册的 MCP 工具，见 F-21） | — |
| F-51 | **macOS 系统代理自动检测（scutil --proxy）** | `http.rs`（`resolve_macos_proxy()`） |
| F-52 | **MdiIcon 组件（@mdi/js SVG 动态图标）** | `src/components/MdiIcon.vue` + `src/components/AppShell.vue` |
| F-53 | **主题色相滑块（hue slider）** | `src/theme.ts`（`setHue()` 生成 MD3 色阶）+ `SettingsView.vue` |
| F-54 | **统一 IR 转发（forward_stream_ir 替代三路分支）** | `api/proxy/forward.rs`（单一函数处理所有格式组合） |
| F-55 | **Install 页面迁移为 Vue SPA + 多消费端** | `src/views/InstallView.vue`（Claude Code + ChatGPT/Codex）+ `api/router.rs`（ServeDir + SPA fallback）+ `api/handlers/stats.rs`（`/api/ui-settings` 公开端点） |
| F-56 | **UI 设置后端持久化（theme/hue/locale）** | `api/handlers/stats.rs`（settings 表读写）+ `src/theme.ts` + `src/i18n/index.ts`（同步到后端） |
| F-57 | **动态 BASE_URL（LAN 浏览器同源访问）** | `src/api.ts`（按 hostname 判断 Tauri vs LAN，LAN 用当前 origin） |
| F-58 | **组合别名（Combo）** | `api/proxy/route.rs`（resolve_combo 展开）+ `api/proxy/stream.rs`（组合强制回退）+ `api/handlers/combos.rs` + `db/combos.rs` (V18) + `CombosView.vue` + `ComboNewView.vue`。多个模型别名按顺序捆绑，客户端用组合名连接时依次尝试直到可用；普通 400 立即透传；白名单按组合名授予 |

### 4.4 未实现（计划中）

| ID | 功能 | 计划版本 |
|----|------|---------|
| F-59 | 管理 API 认证层（Basic Auth / Session Token） | v0.3 |
| F-60 | 路由规则引擎（`routes` 表，优先级 + 权重） | v0.3 |
| F-61 | 指数退避重试（failover 已实现 provider 级切换 + 60s 冷却，退避算法未做） | v0.3 |
| F-62 | 更多 Provider 内置（DeepSeek、Gemini） | v0.3 |
| F-63 | 自动更新机制 | v1.0 |

### 4.5 已知断裂（待修复）

_无。前端 `dashboardApi` / `stores/dashboard.ts` 此前指向未注册的后端路由，已作为死代码清理。_

### 4.6 基础设施模块（未在功能需求中单列）

| 模块 | 说明 | 位置 |
|------|------|------|
| Provider Adapter 抽象层 | `Adapter` async trait（chat/chat_stream/health_check），Anthropic 和 OpenAI 各自实现 | `providers/adapter.rs`、`anthropic.rs`、`openai.rs` |
| 统一错误类型 | `thiserror`-based `AppError` 枚举，跨模块统一数据库/JSON/HTTP/认证/加密/限流错误 | `error.rs` |

---

## 5. 协议转换规格

下游统一暴露 Anthropic Messages API、OpenAI Chat Completions API、OpenAI Responses API 三入口。所有客户端格式先经 IR（中间表示层）统一抽象，再渲染为目标上游格式：

```
客户端 → [Messages | Chat Completions | Responses] → IR (IrRequest / IrStreamEvent / IrUsage) → [上游 Provider]
```

| 上游类型 | 处理方式 |
|---------|---------|
| 所有上游 | 统一 IR 转发：上游字节 → IR 事件（按 provider_kind 解析）→ 客户端 SSE 字节（按 client_format 渲染） |
| 插件（委托供应商） | 插件负责非标→标准，Router 只管密钥轮换 |

### 5.1 必须支持的转换特性

- ✅ 文本消息 (text content blocks)
- ✅ 系统提示 (system prompt → messages[0].role="system")
- ✅ 工具调用 (tool_use ↔ tool_calls)
- ✅ 工具结果 (tool_result ↔ role: "tool")
- ✅ 思考过程 (thinking ↔ reasoning_content，非官方字段)
- ✅ 工具选择 (tool_choice: auto/any/none ↔ auto/required/none)
- ✅ 流式响应 (SSE streaming，逐 chunk 转换)
- ✅ 缓存 token (cache_read_input_tokens)
- ✅ OpenAI Responses API (input/output items、response.completed 事件)
- ✅ server-side web_search 工具归一化 (Messages 客户端 type 前缀匹配)

### 5.2 usage 语义

| 项 | 规则 |
|----|------|
| 合并策略 | **真实值覆盖估算占位**（不用 max）——`forward.rs` 预填的 `chars/4` 估算值偏大，max 会永久压住真实值，污染 usage_log 与客户端上下文条 |
| Responses input_tokens | 减去 `cached_tokens`，保持增量口径（与 Chat Completions `prompt_tokens - cached_tokens` 一致） |
| message_delta 补全 | IR → Messages 渲染时 `message_delta.usage` 补上 `input_tokens`（此前缺失） |
| 上下文超限预警 | 仅 warn 日志，不阻断请求（避免阻断客户端 auto-compact 死锁） |

### 5.3 仅支持流式

非流式分支已移除。所有代理请求强制 `stream: true`。

---

## 6. 密钥池规格

| 状态 | 颜色 | 触发条件 | 行为 |
|------|------|---------|------|
| 正常 | 🟢 绿 | 初始 / 请求成功 | 正常使用 |
| 低配额 | 🟡 黄 | 402 / 429 | 跳过，冷却 300 秒后自动恢复 |
| 失效 | 🔴 红 | 401 / 403 | 永久跳过 |

- 健康状态纯内存（启动全 green），DB `status` 列保留但不读写
- 轮询指针持久化到 `settings` 表（`keypool_index_{provider_id}`）
- 重启后从上次位置继续，而非从 key[0] 开始

---

## 6.1 Token 配额规格（5h / 7d 滚动窗口）

每个 Service Key 可配置两个滚动窗口的 token 上限：**5 小时** 和 **7 天**，默认都是 0（不设限）。

| 项 | 规则 |
|----|------|
| 窗口定义 | 滚动窗口，按 Unix 时间对齐（`now % window_secs`），非自然日 |
| 用量口径 | `prompt + completion + cache_read_input_tokens`，从 usage_log 按需聚合 |
| 超限判定 | `used >= limit`（limit > 0）即 429；任一窗口触顶即拒绝 |
| 恢复方式 | 窗口滚动重置后自动恢复，无需人工干预 |
| 错误响应 | `429` + `retry-after` 头 + `quota_error` 错误体（message 含重置时间） |
| 查询端点 | `GET /v1/user/balance`（认证同代理端点）返回设限窗口的用量，格式为 CCSwitch ZenMux 兼容：`{"success": true, "data": {"quota_5_hour": {"usage_percentage": 0.43, "resets_at": "..."}, "quota_7_day": {...}}}`；未设限窗口省略字段 |

用途：把单个密钥的消费上限锁住，防止一个密钥把上游额度耗尽；配额在应用内管理页面配置。

---

## 6.2 故障转移规格（Provider Failover）

同一模型别名（display_name）配置在多个 Provider 上时，网关按序尝试全部候选，上游故障自动切换，不打断客户端会话。

| 项 | 规则 |
|----|------|
| 开关 | 设置页「路由」Tab `failover_enabled`（**默认关闭**，关闭时行为与单 Provider 完全一致） |
| 候选来源 | `resolve_route_candidates()`：同 display_name 全部 models JOIN providers 行，按 `sort_order ASC, created_at ASC` 排序，按 provider_id 去重，跳过插件离线的委托 provider |
| 尝试顺序 | 双层循环：外层遍历 provider 候选，内层遍历该 provider 的 key 池；key 级 4xx（401/402/403/429）先耗尽当前 provider 全部 key 才切下一个 |
| 触发切换 | 上游 5xx / 网络错误 / 响应头超时 → 切下一个候选并标记冷却 |
| 冷却 | 失败 provider 冷却 60s（纯内存 `provider_cooldowns`，不持久化），2xx 成功立即清除；冷却中直接跳过 |
| 最终错误码 | 全部候选失败：网络错误 502、响应头超时 504、key 4xx 耗尽透传最后一次上游失败响应、无可用 key 503 |
| 混合协议 | 候选可混合 Anthropic / OpenAI 类型，请求体骨架循环外预构建，循环内按候选类型选用并覆写 model |

用途：同一模型在多家 Provider 上配置（官方 + 代理镜像）时，一家上游故障自动切换，避免客户端请求失败；60s 冷却防止「每次都先打坏的 provider 再失败一次」。

---

## 6.3 Claude FM 规格（后端电台引擎）

应用内置一台「电台」：音频解码与播放由 Rust 后端 `FmEngine` 直接完成，输出到系统音频设备。前端仅负责展示与控制。

| 项 | 规则 |
|----|------|
| 播放引擎 | `FmEngine`（`api/handlers/fm.rs`），rodio 解码 + 系统音频设备输出，`std::thread::spawn` 运行（rodio 需稳定线程），`mpsc` channel 接收播放控制消息 |
| 系统媒体控制 | souvlaki 接入 macOS Now Playing / Windows SMTC / Linux MPRIS，主线程初始化 |
| 预加载 | 双缓冲：当前曲播放时 `tauri::async_runtime::spawn` 预下载下一曲（`tokio::sync::oneshot` 传递结果），切歌零等待 |
| 暂停语义 | 暂停 = 静音，时间轴照常推进 |
| 生命周期 | 与应用进程绑定：窗口关闭只隐藏到托盘（进程常驻），音乐持续 |
| 预热 | 音源就绪后才解锁播放按钮与托盘 FM 项；就绪前菜单项隐藏，避免误操作 |
| 视图 | `/fm` 路由（`ClaudeFmView.vue`）：大圆形播放/暂停按钮 + 底部等宽字体「标题 - 艺人」（电台语义，不显示时间码） |
| 前端 | `src/fm/player.ts`（~60 行）纯命令/事件：`fm_toggle` / `fm_play` / `fm_pause` / `fm_get_state` Tauri command + `fm-meta` / `fm-ready` / `fm-state-changed` 事件 |
| 托盘 | 预热完成后菜单加入「Claude FM」勾选项：勾选 = 播放，取消 = 暂停；语言随 `settings.locale`；点击直接调用引擎（不绕前端中转） |
| i18n | `fm.play` / `fm.pause` / `fm.error`（曲目无法播放自动跳过） |

用途：编码时挂一台低打扰的氛围电台，无需开第三方播放器；托盘勾选即控，与应用窗口解耦。

---

## 7. 插件系统规格

外部服务通过 WebSocket 注册为「委托供应商」。职责分工：

| 职责 | Router | Plugin |
|------|--------|--------|
| 密钥池管理 | ✅ 轮询 + 红绿灯 + 持久化 | ❌ |
| 协议转换 | ✅ Anthropic ↔ OpenAI | ✅ 非标 → 标准 |
| 业务头注入 | ❌ | ✅ |
| 健康监控 | ✅ 基于请求响应 | ❌ |
| 用量统计 | ✅ usage_log | ❌ |

**生命周期**：注册 → 用户确认 → 激活 → 密钥同步（`keys_update`）→ 心跳（30s/90s）→ 忽略（彻底删除 + WS 断开 → 插件重连后重新注册）

---

## 7.1 局域网分发规格（install 页面 + 单端口）

把本机网关能力延伸到局域网设备：浏览器沙箱无法直接装 CLI、写客户端配置，所以 install 页面生成「一行命令」让用户在终端执行一次。Install 页面从旧版的静态 HTML（`assets/install.html`，`include_str!` 编译进二进制）迁移为 Vue SPA 组件（`src/views/InstallView.vue`），后端通过 `tower_http::ServeDir` 托管前端构建产物 + SPA fallback 到 `index.html`，由 Vue Router 处理 `/install` 路由。详见 [specs/spec-lan-deploy.md](specs/spec-lan-deploy.md)。

| 项 | 规则 |
|----|------|
| 单 listener + IP 限制 | `0.0.0.0:19068`，`/api/*` 管理端点由 `admin_ip_guard` 中间件限 loopback；`/api/ui-settings`、`/v1/*` 对外开放；未匹配 GET 请求 fallback 到 `index.html` |
| 分发链接 | 密钥管理页创建密钥后弹窗展示：`http://<本机IP>:19068/install?t=<明文key>`，可一键复制 |
| 本机 IP + 端口 | `GET /api/install/local-ip` 返回 `{ ip, port }`（UDP socket 连 8.8.8.8:80 取出口 IP + `Config.port`） |
| UI 设置同步 | `GET /api/ui-settings`（公开端点）返回管理端的 `theme`/`hue`/`locale`，LAN install 页面加载时读取并应用，保持与主机应用一致的视觉风格 |
| 消费端选择 | 支持 **Claude Code**（写 `~/.claude/settings.json`）和 **ChatGPT/Codex**（写 `~/.codex/config.toml` + `auth.json`）两种客户端，用户可切换 |
| 模型选择 | 用 `?t=` 里的 key 调 `/v1/models` 取模型别名下拉；未选中别名时省略模型相关配置行 |
| Claude Code 命令 | 按平台生成：A 段 `npm i -g @anthropic-ai/claude-code`（可勾选省略）；B 段写 `settings.json`：`env.ANTHROPIC_AUTH_TOKEN` + `ANTHROPIC_BASE_URL` + 4 模型槽位（`_MODEL`/`_MODEL_NAME` 统一用网关别名）+ `permissions.defaultMode=bypassPermissions`，**保留客户端既有字段** |
| ChatGPT/Codex 命令 | 写 `~/.codex/config.toml`（`model`/`model_provider`/`base_url`）+ `~/.codex/auth.json`（`OPENAI_API_KEY`） |
| 安全边界 | 密钥明文嵌入 URL（局域网嗅探可见），只发给可信设备；撤销即在密钥列表删除，立即失效 |
| 非 Tauri 兼容 | 前端代码通过动态 `import()` 延迟加载 Tauri API，LAN 浏览器访问时不触发 Tauri 依赖报错 |

用途：团队成员/多设备快速接入同一网关，无需逐个手写 base URL、token、模型名——复制一条链接即完成客户端配置。

---

## 8. 非功能需求

### 8.1 性能

| 指标 | 目标 |
|------|------|
| 启动到就绪 | ≤ 3 秒 |
| 代理额外延迟（透传） | ≤ 5ms |
| 代理额外延迟（转换） | ≤ 20ms |
| 内存占用（空闲） | ≤ 100MB |
| 并发 | ≤ 50 请求 |
| 请求头超时 | 自适应 300/480/600s（按估算输入 token 分档，基准 300s；见 `header_timeout_for()`） |
| 流 chunk 间隔超时 | 120 秒 |

### 8.2 安全

| 要求 | 实现 |
|------|------|
| Service Key 存储 | Argon2 哈希（随机盐 + PHC 格式） |
| Provider Key 存储 | AES-256-GCM 加密（主密钥 `master.key`，权限 0600） |
| 管理 API | `admin_ip_guard` 中间件限 loopback IP，仅本机可访问 `/api/*` 端点 |
| 公共暴露面 | 单端口 `0.0.0.0:19068`，公开路径只暴露 `/v1/*`（需 key 鉴权）、`/api/ui-settings`（主题/语言）、前端 SPA fallback（含 `/install` 页面）、`/health`、`/ws` |
| CORS | 统一 origin 白名单（localhost + 127.0.0.1 的 5173/19068 双端口 + tauri://localhost + https://tauri.localhost + http://tauri.localhost，共 7 个） |
| 频率限制 | 令牌桶 128 req/min，按 Service Key |
| 分发密钥 | install URL query 明文嵌入，仅限可信局域网设备，撤销即在密钥列表删除 |
| 数据文件访问 | 导出/导入文件对话框 + fs 权限白名单（`$HOME`/`$DOWNLOAD`/`$DOCUMENT`/`$DESKTOP`/`$APPDATA`），白名单外路径不可读写 |

### 8.3 网络

| 维度 | 要求 |
|------|------|
| 系统代理继承 | 出站请求自动继承系统代理（环境变量 → Windows 注册表 → macOS scutil），`localhost`/`127.0.0.1` 自动豁免直连 |
| HTTP 客户端 | 统一工厂（`http.rs`），所有出站请求使用 `build_http_client()` / `http_client()` |

### 8.4 兼容性

| 维度 | 要求 |
|------|------|
| Anthropic API | 兼容 `2023-06-01` |
| OpenAI API | 兼容 Chat Completions v1 + Responses API |
| 操作系统 | macOS (primary)、Windows、Linux |

---

## 9. 成功指标

### 9.1 北极星指标

> **成功发出首个代理请求的时间（Time to First Request）** ≤ 3 分钟

### 9.2 功能指标

| 指标 | 目标 |
|------|------|
| Provider 接入成功率 | ≥ 99%（有效 Key） |
| 协议转换正确率 | 100%（无数据丢失） |
| Key 故障自动切换 | 100%（有备用 Key 时） |

---

## 10. 风险

| 风险 | 概率 | 缓解 |
|------|------|------|
| 上游 API 格式变更 | 中 | 版本锁定 + 兼容性测试 |
| SQLite 高并发瓶颈 | 低 | WAL 模式 + 异步批量写入 |
| 协议转换丢失特性 | 中 | 不兼容特性显式报错 |
| 上游挂起网关卡死 | 低 | 独立超时保护（自适应头超时 300/480/600s + 120s chunk 间隔） |
| 密钥泄露 | 低 | AES-256-GCM + Argon2 |
