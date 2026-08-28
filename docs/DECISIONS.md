# Architecture Decision Records

设计背后的历史原因。防止架构漂移。

---

## ADR-001: 选择 Tauri 2 作为桌面框架

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

需要一个轻量级桌面应用来运行本地 LLM API 网关。考虑过 Electron、原生应用、纯 CLI。

### 决策

采用 Tauri 2：Rust 后端 + WebView 前端。

### 原因

1. **轻量**: 安装包 < 10MB，内存占用 < 100MB（Electron 通常 > 200MB）
2. **性能**: Rust 后端处理高并发代理请求
3. **安全**: Rust 内存安全 + 系统级加密库原生支持
4. **跨平台**: 一套代码编译 macOS/Windows/Linux

### 代价

- 需要 Rust 工具链（学习曲线）
- WebView 渲染在某些系统可能不一致

---

## ADR-002: 仅支持流式代理（stream=true）

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

LLM API 支持流式（SSE）和非流式两种模式。完整支持需要两套代码路径。

### 决策

强制 `stream=true`。即使客户端发送 `stream=false`，也会被静默覆写为 `true`。

### 原因

1. **简化实现**: 只需一套流式处理逻辑
2. **用户体验**: Claude Code 等主流客户端都默认流式
3. **资源效率**: 流式可以边生成边传输，不需要缓存完整响应

### 代价

- 无法支持需要完整响应的场景（如某些 batch 处理）

---

## ADR-003: 密钥健康状态纯内存存储

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

密钥健康状态（green/yellow/red）需要持久化还是仅内存？

### 决策

健康状态仅存内存，启动时全部初始化为 green。只有轮询指针（`current_index`）持久化到 `settings` 表。

### 原因

1. **启动恢复**: 重启后从上次轮询位置继续
2. **减少 IO**: 每次健康状态变更都写 DB 会产生大量小事务
3. **语义合理**: 健康状态是运行时概念，重启后重新探测更合理

### 代价

- 重启后无法看到历史健康状态

---

## ADR-004: usage_log 自包含快照设计

**日期**: 2026-08-01  
**状态**: 已接受

### 背景

`usage_log` 表需要关联 `providers`、`models`、`api_keys`、`service_keys`。是否用外键？

### 决策

`usage_log` 不使用外键，而是存储快照字段：`provider_name`、`model_display_name`、`key_name`、`service_key_name` 等。

### 原因

1. **历史完整性**: 删除 Provider/Model/Key 后，历史统计仍然可见
2. **查询性能**: 统计查询不需要 JOIN 多张表
3. **数据独立**: 即使上游表结构变化，历史记录不受影响

### 代价

- 数据冗余（每条日志多存 ~100 字节）

---

## ADR-005: 管理 API 无认证，绑定 loopback

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

管理 API（`/api/providers`、`/api/keys` 等）是否需要认证？

### 决策

管理 API 无认证，仅通过绑定 loopback IP + CORS 白名单保护。

### 原因

1. **本地场景**: 桌面应用运行在用户本机，威胁模型是"本机其他进程"
2. **简化使用**: 无需登录/Token，打开应用即用
3. **CORS 保护**: 浏览器端恶意网页无法跨域调用

### 威胁模型

- **已防护**: 远程攻击（绑定 loopback）、浏览器跨域攻击（CORS）
- **未防护**: 本机恶意进程可以访问管理 API
- **接受风险**: 桌面应用场景，用户应保证本机安全

---

## ADR-008: Provider API Key 使用 AES-256-GCM 加密

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

Provider API Key 需要持久化存储。如何保护？

### 决策

使用 AES-256-GCM 对称加密，主密钥存储在 `master.key` 文件（权限 0600）。

### 原因

1. **可逆**: 需要解密后发送给上游 API（不能用哈希）
2. **强加密**: AES-256-GCM 是 NIST 标准，抗已知攻击
3. **认证加密**: GCM 模式提供完整性校验，防篡改

### 代价

- 主密钥文件丢失则所有 Provider Key 不可恢复

---

## ADR-009: Service Key 使用 Argon2 哈希

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

Service Key（客户端访问令牌）如何存储？

### 决策

使用 Argon2id 哈希算法，随机 salt，存储在 `service_keys.key_hash`。

### 原因

1. **不可逆**: Service Key 不需要解密，只需验证
2. **抗暴力破解**: Argon2 是内存硬算法，GPU/ASIC 攻击成本高
3. **OWASP 推荐**: Password Storage Cheat Sheet 首选 Argon2id

### 代价

- 验证需要逐条遍历所有 Service Key（无法索引查找）

---

## ADR-010: 数据库 UPSERT 使用 ON CONFLICT DO UPDATE

**日期**: 2026-07-28  
**状态**: 已接受

### 背景

`save_provider`、`save_model` 等方法需要"存在则更新，不存在则插入"。

### 决策

使用 `INSERT ... ON CONFLICT DO UPDATE`，不使用 `INSERT OR REPLACE`。

### 原因

1. **避免级联删除**: `INSERT OR REPLACE` 会触发 `ON DELETE CASCADE`，误删子表数据
2. **语义明确**: `ON CONFLICT DO UPDATE` 明确表示"冲突时更新"
3. **可控更新**: 可以指定哪些字段更新，哪些保留

### 代价

- SQL 语法更复杂

---

## ADR-013: 插件系统采用 WebSocket 注册

**日期**: 2026-08-01  
**状态**: 已接受

### 背景

需要支持"委托供应商"（如钉钉 DEAP），插件负责协议转换 + 业务头注入。

### 决策

插件通过 WebSocket 连接 `/ws/plugin`，发送注册/心跳/密钥同步消息。

### 原因

1. **实时通信**: WebSocket 支持双向消息，适合心跳 + 密钥同步
2. **生命周期管理**: 连接断开自动检测（90s 无心跳标记离线）
3. **解耦**: 插件是独立进程，崩溃不影响 Router

### 职责分工

| 职责 | Router | Plugin |
|------|--------|--------|
| 密钥轮换 | ✅ | ❌ |
| 健康监控 | ✅ | ❌ |
| 用量统计 | ✅ | ❌ |
| 协议转换 | ❌ | ✅ |
| 业务头注入 | ❌ | ✅ |

---

## ADR-015: Token 配额用滚动窗口 + 按需聚合

**日期**: 2026-08-02  
**状态**: 已接受

### 背景

需求：每个 Service Key 可配置 5 小时 / 7 天内的 token 上限，触顶返回 429。

### 决策

1. **滚动窗口**：窗口按 Unix 时间对齐（`now % window_secs`），不是自然日/自然小时
2. **上限持久化、用量按需聚合**：`service_keys` 只存 `quota_5h/quota_7d`；已用量每次从 `usage_log` 条件聚合
3. **429 采用 quota_error 类型**：携带 `retry-after` 头 + 可读的重置时间

### 原因

1. **正确性**：滚动窗口平滑且与上游配额对齐
2. **简单**：单条 SQL 即得两窗口用量，无新增状态
3. **一致**：`/v1/user/balance` 与表格「限额」列共用同一聚合函数

### 代价

- 每个代理请求多一次 SQLite 条件聚合查询

---

## ADR-016: 统一 HTTP 客户端工厂 + 系统代理自动继承

**日期**: 2026-08-03  
**状态**: 已接受

### 背景

项目有多处出站 HTTP 请求，各自独立构建。国内网络下需走代理才能连通。

### 决策

新增 `http.rs` 模块作为唯一 HTTP 客户端工厂：

1. `system_proxy()`: 解析系统代理（环境变量 → Windows 注册表 → macOS scutil），OnceLock 缓存
2. `build_http_client() -> ClientBuilder`: 返回带系统代理的 builder
3. NO_PROXY 默认豁免 `localhost`、`127.0.0.1`、`[::1]`

所有出站 HTTP 请求必须使用工厂方法。

### 原因

1. **统一代理**：所有调用点自动继承系统代理
2. **零配置**：Windows 用户配 Clash 系统代理后自动继承
3. **性能**：OnceLock 缓存代理解析结果

### 代价

- 代理在应用运行期间不可变

---

## ADR-017: 单 listener + 路径级 IP 限制

**日期**: 2026-08-06  
**状态**: 已接受

### 背景

旧方案用两个 listener 分离：admin 绑 `127.0.0.1:19068`，public 绑 `0.0.0.0:19069`。

### 决策

合并为单 listener 绑 `0.0.0.0:19068`，通过路径级 IP 中间件控制访问权限：

| 路径类型 | 限制方式 | 路由 |
|----------|----------|------|
| 公开 | 不限 IP | `/health`、`/ws`、`/v1/*` 代理 |
| 管理 | `admin_ip_guard` 中间件限 loopback | `/api/*` CRUD |

### 原因

1. **单端口简化运维**：防火墙只需放行 19068
2. **IP 中间件而非双 listener**：路径级限制比端口级更灵活
3. **保留 loopback 安全模型**：`admin_ip_guard` 确保 `/api/*` 管理端点永不对外开放

### 代价

- `0.0.0.0:19068` 向局域网暴露：任何人可调 `/v1/*`（需有效 key）

---

## ADR-019: 故障转移（Provider Failover）

**日期**: 2026-08-03  
**状态**: 已接受

### 背景

同一模型别名配置在多个 Provider（官方 + 代理镜像）时，上游故障会让整个会话失败。

### 决策

1. **候选解析**：`resolve_route_candidates()` 返回同 `display_name` 下全部候选（`sort_order ASC, created_at ASC`，按 provider_id 去重）
2. **双层循环**：外层遍历 provider 候选，内层遍历 key 池
3. **冷却表纯内存**：失败 provider 冷却 60s
4. **开关默认关闭**：`failover_enabled` 存 settings 表
5. **错误码语义**：网络错误 502、响应头超时 504、key 4xx 耗尽透传最后一次上游失败响应、无可用 key 503

### 原因

1. 按 provider_id 去重避免同一上游被重复尝试
2. 冷却 60s 而非指数退避：场景是「上游暂时不可用」，固定冷却足够
3. 开关默认关闭符合「不主动改变既有行为」原则

### 代价

- 双循环复杂度集中在 handler.rs

---

## ADR-027: 引入 IR 中间表示层统一三协议转换

**日期**: 2026-08-09  
**状态**: 已接受

### 背景

项目最初只有 Anthropic Messages ↔ OpenAI Chat Completions 双向转换。随着 OpenAI Responses API 支持需求出现，三协议互转的组合爆炸问题凸显。

### 决策

1. **新建 `api/proxy/ir/` 模块**（Intermediate Representation，中间表示层）
2. **IR 以 Anthropic Messages 为骨架**：`IrContentBlock` 覆盖 Text/Image/Thinking/ToolUse/ToolResult 五种内容块
3. **单向转换取代双向转换**：所有客户端格式 → IR（`from_*`），IR → 所有客户端格式（`to_*`）
4. **`IrStreamEvent` 6 种变体**：MessageStart → ContentBlockStart → ContentBlockDelta → ContentBlockStop → MessageDelta → MessageStop
5. **`IrUsage` 统一 token 统计**：input_tokens / output_tokens / cache_read_input_tokens 等
6. **usage 真实值覆盖估算占位**：上游返回真实 usage 时直接覆盖估算值（不用 `max()`）
7. **上下文超限预警（软警告）**：仅记录 warn 日志，不返回 400 错误（避免阻断客户端 auto-compact）

### 原因

1. **组合爆炸收敛**：N 个协议只需 2N 个转换模块，而非 N×(N-1) 个双向模块
2. **内部工具解耦**：websearch 劫持、usage 追踪等内部工具只操作 IR 类型
3. **扩展性**：新增协议只需新增一对 `from_*` / `to_*` 模块

### 代价

- 转换路径变长：客户端格式 → IR → 客户端格式（两步）

---

## ADR-035: WebSearch/WebFetch 迁移到本地 MCP 端点

**日期**: 2026-08-24  
**状态**: 已接受

### 背景

旧方案由代理跑 server-side 劫持循环：注入 `web_search` 工具 → 缓冲所有中间搜索轮次 → 本地 Bing 搜索回传 → 收尾合成响应。

### 决策

1. **删除劫持循环**，改为网关内置 **MCP（Streamable HTTP）端点 `/mcp`**，提供 `web_search`、`web_fetch`、`web_vision` 三个工具
2. **Bing 搜索策略**：HTTP 浏览器头 + cookie 复用 + 懒预热 + 双域名 fallback + 绕过代理直连
3. **WebFetch 渲染**：Tauri 内置 WebView（懒创建隐藏窗口）+ 静态抓取回退
4. **web_vision**：设置页指定「视觉专用模型」，网关取图后调该模型生成描述
5. **开关语义**：`mcp_websearch` / `mcp_webfetch` / `mcp_vision` 三个开关独立控制
6. **代理侧只保留「剔除」逻辑**：开关开启时剔除请求自带搜索工具（防上游官方搜索生效）

### 原因

1. **标准协议 > 私有序列**：MCP 是客户端原生支持的协议，工具调用可见、可审批
2. **性能**：模型直接调用工具，无中间轮缓冲
3. **删除大于新增**：净删 ~1600 行，新增 ~600 行

### 代价

- 客户端需一次性注册 MCP（设置页提供可复制的命令）

> **2026-08-28 变更**：`web_vision` 随 MCP Vision 功能整体移除（设置页「路由」Tab 不再提供视觉能力开关与视觉模型配置，`mcp/vision.rs` 删除，代理侧不再剥离图片内容块）。本 ADR 其余决策仍然有效。

---

## ADR-040: 组合别名（Combo）

**日期**: 2026-08-24  
**状态**: 已接受

### 背景

用户需要把多个模型别名捆绑成一个新别名：客户端用组合名连接时，路由「不断尝试列表中的所有模型直到找到可用模型」。

### 决策

1. **组合 = 解析层展开**：`resolve_combo` 把组合按成员 `position` 逐个展开成候选列表，交给现有双循环执行
2. **组合强制回退**：组合命中后 `failover = global_failover || is_combo`——成员间回退不受全局开关影响
3. **仅供应商级失败换成员**：网络错误、头超时、5xx、401/402/403/429 → 换下一个成员；普通 400（非配额）立即透传
4. **成员只能是叶子模型别名**：不允许嵌套组合 → 天然无环
5. **命名双向冲突校验**：组合名不得撞 `models.display_name`
6. **白名单按组合名授予**：授予组合名 = 授予全部成员
7. **统计归因到实际成员**：`usage_log` 零改动

### 原因

1. **复用双循环**：组合的全部回退语义自动继承
2. **叶子成员最简**：嵌套组合需要运行时环检测 + UI 复杂度翻倍
3. **400 透传优于全量尝试**：请求本身非法时换模型是白跑延迟

### 代价

- 组合解析是 N+1 查询（1 次组合 + 每成员 1 次候选查询）

---

## ADR-041: FM 像素艺术桌面壁纸（WorkerW 劫持 + 引擎权威时钟）

**日期**: 2026-08-28  
**状态**: 已接受

### 背景

用户希望右击 FM 像素艺术画面勾选「设置为桌面背景」后，桌面壁纸被劫持为与
应用程序内像素艺术**严格同步**的动画（但壁纸上不显示播放/暂停按钮与歌曲
信息），暂停/切歌时两处画面一致。

### 决策

1. **渲染面 = 第二个 WebviewWindow**（label=`wallpaper`）：注入
   `__WALLPAPER_MODE__` 走 `WallpaperScene` 分支（黑底全屏像素），复用整套
   前端像素渲染管线（`pixelart.ts` 确定性生成），不做 Rust 侧重绘。
2. **Windows = WorkerW 劫持（双布局探测）**：Progman `0x052C` 唤醒壁纸宿主；
   优先取 **Progman 的直接子窗 WorkerW**（Win10/11 实测布局：`SHELLDLL_DefView`
   直接挂 Progman 下，壁纸 WorkerW 为 Progman 子窗），兜底经典布局（含
   `SHELLDLL_DefView` 的 WorkerW 之后的顶层兄弟 WorkerW）；`SetParent` 挂入 +
   递归 `WS_EX_TRANSPARENT` 点击穿透（WebView2 子窗口异步创建，1s 后补一轮）。
   取消勾选/进程退出 = 销毁窗口，WorkerW 自动重绘原壁纸——无需显式还原。
3. **macOS = `kCGDesktopIconWindowLevel`**：NSWindow 降到壁纸图与桌面图标
   之间；`orderFront:` 呈现（tao 的 `show()` 走 makeKeyAndOrderFront 抢
   焦点，禁用）；`setIgnoresMouseEvents` 穿透。
4. **同步 = 引擎权威时钟**：动画时钟从本地累计改为 `fm.rs` 的 `scene_t`
   （仅播放时按真实流逝累计、暂停冻结），两窗口 `PixelScene` 均以
   `invoke('fm_scene_t')` 采样——不引入新的广播事件，天然严格同步，
   且顺带修复了主窗口路由返回到 /fm 时动画相位重置的问题。
5. **穿透方式 = 手动 Win32 样式**（非 `set_ignore_cursor_events`）：后者仅
   作用于顶层 HWND，不覆盖 WebView2 子窗口。
6. **持久化 + 自愈**：勾选态写 DB `wallpaper_enabled`，启动延迟 500ms 惰性
   恢复（setup 期间事件循环未泵送，主线程屏障会死锁）；`Destroyed` 后清槽
   1s 复查重建。

### 备选与权衡

- **定时截图刷新系统壁纸**（`SystemParametersInfo` 轮换 PNG）：5fps 下
  壁纸刷新延迟/闪烁不可接受，且无法严格同步 → 否决。
- **全屏置底普通窗口**（不挂 WorkerW）：会被其他窗口覆盖，不是真壁纸 →
  仅作 WorkerW 失效时的回退。
- **URL query 区分壁纸入口**：`WebviewUrl::App` 带 query 属未固定行为 →
  用 `initialization_script`（WebView2/WKWebView 均在页面脚本前注入）。
- **`transparent` 窗口特性**：Windows 上 tauri 的 transparent 实为无效
  （tao 源码注明），且像素画本身铺满全屏 → 纯 CSS 黑底，不启用。

### 代价

- 仅在 Windows（WorkerW 配方随系统版本可能变化）/ macOS 两级平台实现；
  多显示器下 v1 仅主显示器，分辨率/DPI 变化后需重新勾选（已知限制）。
- `win.rs`/`macos.rs` 是平台专用 unsafe 代码（Win32 / objc2），macOS 侧
  编译验证需在 macOS 上完成（CI macos-14 job 兜底）。

---

## ADR-042: Windows 壁纸改为 Rust GDI 直绘（放弃 WebView2）

**日期**: 2026-08-28  
**状态**: 已接受（Windows 部分；macOS 暂保留 ADR-041 的 WebView 方案）

### 背景

ADR-041 的 WebView 方案在 Windows 11（2024+ Explorer）上连环受阻：
1. WorkerW 双布局 → 已适配；
2. `FindWindowW` 空字符串 ≠ NULL 的坑 → 已修；
3. SetParent 后必须转 `WS_CHILD`（Win8+ 会把 popup 扔到顶层 Z 序最底）→ 已修；
4. `additional_browser_args` 与主窗口环境参数冲突（0x8007139F）→ 改环境变量；
5. **最终死结**：即便窗口/渲染树全部健在（WS_CHILD + 可见 + 全屏），
   Chromium 的`CalculateNativeWinOcclusion` 把挂在 WorkerW 下的窗口判为
   「完全遮挡」→ 合成暂停 → **一个像素都不画**（显式 flag + 独立
   user data folder 均已尝试）。WebView2 在桌面劫持场景不可靠。

### 决策

1. **Windows = Rust GDI 直绘**（`wallpaper/win.rs` 重写 + `pixelart.rs` 移植）：
   - `pixelart.rs` 是 `src/lib/pixelart.ts` 的**逐位一致**移植（mulberry32 用
     u32 包装运算复刻 JS 位语义；`Math.round` 用 `floor(x+0.5)`；canvas
     小数坐标抗锯齿按覆盖率 alpha 混合；随机数调用顺序逐行对应；seed 的
     f64 精度陷阱按 JS 语义复算）——前端与壁纸仍然同一幅画；
   - 窗口：`RegisterClassW` + `CreateWindowExW` 以 `WS_CHILD` **一步创建**
     进壁纸 WorkerW（穿透/无焦点/不进任务栏的 EX_STYLE 建窗时带齐，
     无需 SetParent 与样式补丁），独立 `wallpaper-painter` 线程每 200ms
     渲染 160×90 逻辑帧 → `StretchDIBits` 最近邻放大上屏；
   - 时钟：直接读 `FmPlaybackState`（`fm.state_arc()`，零 IPC），
     引擎权威 `scene_t` 与主窗口前端采样同源，严格同步；
   - 自愈：线程循环发现窗口被外部销毁（Explorer 重启）→ 睡 1s 重建；
     禁用 = 置旗标 → 线程销毁窗口退出。
2. **macOS 暂保留 WebView 方案**（`kCGDesktopIconWindowLevel`），后续可用
   同思路改 CG 直绘（不同窗口体系，另行 ADR）。

### 原因

1. **零依赖零坑**：不经过浏览器/合成器，GDI 子窗是世界公认的壁纸层画法
   （Wallpaper Engine 自绘壁纸同款），不存在"窗口活着但不画"的状态；
2. **性能**：160×90×5fps 的 CPU/内存占用可忽略（远低于一个常驻的
   WebView2 浏览器进程）——壁纸不应成为耗电大户；
3. **前端像素管线零改动**：生成算法仍是同一份 TS 源码的权威实现，
   移植版按黄金值校验锁死一致性。

### 代价

- 维护两份像素渲染器（TS 与 Rust），后续调色/加元素需双改——靠
  golden 值单测 + 视觉对照兜住；
- macOS 仍走 WebView（后续需真机验证 ADR-041 的层级方案）。

---

## ADR-043: Windows 壁纸改回 WebView + 社区插件挂载（弃 GDI 直绘）

**日期**: 2026-08-28  
**状态**: 已接受（Windows 部分；GDI 直绘方案回退存档）

### 背景

ADR-042 的 Rust GDI 直绘在真机上也未能上屏（窗口结构完全正确——
WS_CHILD、可见、父链正确、WM_PAINT 正规绘制——桌面依旧无变化）。
检索社区 [tauri-plugin-desktop-underlay](https://github.com/Charlie-XIAO/tauri-plugin-desktop-underlay)
（把窗口挂到桌面 WorkerW 层的 Tauri 插件）发现其真实用户能看到 WebView
内容，而我们的 WebView 尝试（多轮配置）看不见。核心差异：**窗口透明度**。

### 决策

1. **Windows = 透明 WebView 窗口 + 社区插件挂载**：
   - `transparent(true)` 的 WebviewWindow 走 **DWM 视觉合成**渲染——桌面
     WorkerW 层唯一可靠的内容上屏路径（经典 GDI/重定向表面在桌面层
     不被 DWM 呈现——这同时解释了 GDI 直绘版为何"什么都对但看不见"）；
   - `set_desktop_underlay(true)`（插件）负责 SetParent 进 WorkerW，
     不再自研挂载（其配方 = 我们的双布局 + `(0x052C, 0xD, 0x1)`）；
   - WebView2 配置：显式 `additional_browser_args(--disable-features=
     CalculateNativeWinOcclusion)`（WorkerW 子窗会被判"完全遮挡"暂停合成）
     + 独立 user data directory（否则与主窗口环境参数冲突 0x8007139F）——
     即在 ADR-041 链条里从未实际合并部署过的那组修复；
   - 点击穿透：`WS_EX_TRANSPARENT`（顶层 + 递归子窗，1s 补轮）；
     **全程严禁 `WS_EX_LAYERED`**——分层窗口不调 SetLayeredWindowAttributes
     即不显示内容（WebView 版与 GDI 版两次"看不见"的公共元凶）。
2. **GDI 直绘退役**：`pixelart.rs` 移植与画家线程删除（保留在 git 历史）；
   前端 `WallpaperScene` / `sampleT` 引擎时钟方案复用（macOS 同款）。
3. **macOS 不变**（objc2 层级方案；后续真机验证）。

### 代价

- WebView 方案的固有成本（一个浏览器进程）重新回来；
- 依赖社区插件的 Windows 实现（其维护节奏不可控，但配方公开可自行顶替）。

---

## ADR-044: 移除 MCP Vision 工具

**日期**: 2026-08-28  
**状态**: 已接受

### 背景

MCP Vision（`web_vision` 工具）允许模型通过视觉专用模型识别图片。实现包括：
- 设置页「路由」Tab 提供视觉能力开关与视觉模型配置
- 代理侧自动剥离请求中的图片内容块，注入系统提示强制模型使用 `web_vision`
- `mcp/vision.rs` 实现图片取图 + 视觉模型调用

### 决策

1. **删除 `web_vision` 工具**：`mcp/tools.rs` 移除工具定义与执行逻辑
2. **删除 `mcp/vision.rs`**：整个模块删除
3. **移除代理侧图片剥离**：`stream.rs` 不再注入 vision hint，不再剥离图片内容块
4. **移除设置页配置**：SettingsView 不再提供 `mcp_vision` 开关与视觉模型选择
5. **移除相关状态**：AppState 不再包含 `mcp_vision` AtomicBool，settings 表不再存储 `mcp_vision_provider` / `mcp_vision_model`

### 原因

1. **使用率低**：视觉模型配置复杂（需单独指定 provider/model），用户实际使用少
2. **维护成本**：代理侧图片剥离 + 系统提示注入增加代码复杂度
3. **客户端原生支持**：现代客户端（Claude Code、ChatGPT）已原生支持视觉，无需网关层中转
4. **简化 MCP 工具集**：保留 `web_search` / `web_fetch` / `notify` 三个核心工具，职责更清晰

### 代价

- 依赖视觉能力的旧客户端需升级到支持原生视觉的版本
- 部分用户可能需要切换到支持视觉的客户端

---

## ADR-045: Service Key 创建时设置模型白名单

**日期**: 2026-08-28  
**状态**: 已接受

### 背景

旧方案中，Service Key 创建后需在编辑页单独设置 `allowed_models`。用户反馈：创建密钥时即知道要限制哪些模型，额外编辑步骤冗余。

### 决策

1. **创建时即可设置白名单**：`CreateServiceKeyRequest` 新增 `allowed_models: Option<Vec<String>>` 字段
2. **数据库迁移**：`service_keys` 表新增 `allowed_models` 列（JSON 数组，默认 `"[]"`）
3. **前端 UI**：KeysView 创建对话框增加模型选择区域（按 provider 分组，checkbox 多选）
4. **语义不变**：空数组 = 允许全部模型（与编辑时一致）

### 原因

1. **减少操作步骤**：创建即配置，无需二次编辑
2. **UX 一致性**：创建对话框与编辑页使用相同的模型选择 UI
3. **向后兼容**：`allowed_models` 为可选字段，旧客户端不受影响

### 代价

- 创建对话框高度增加（模型列表可滚动）

---

## ADR-046: Windows 自定义窗口控制按钮

**日期**: 2026-08-28  
**状态**: 已接受

### 背景

Tauri 默认使用系统原生标题栏。Windows 11 的原生标题栏与自定义 UI 风格不协调，且无法实现红绿灯风格的窗口控制按钮。

### 决策

1. **去除原生装饰**：`lib.rs` setup 阶段调用 `set_decorations(false)`
2. **自定义拖拽区域**：`AppShell.tsx` 顶部 40px（Windows）/ 28px（macOS）透明区域设置 `data-tauri-drag-region`
3. **红绿灯风格按钮**：`WindowControls` 组件（仅 Windows 渲染），关闭（红）/ 最小化（黄）/ 最大化（绿）
4. **capabilities 更新**：`default.json` 新增 `core:window:allow-close` / `allow-minimize` / `allow-maximize` / `allow-unmaximize` / `allow-is-maximized`

### 原因

1. **视觉一致性**：红绿灯按钮与 macOS 风格统一，跨平台体验一致
2. **自定义空间**：去除原生标题栏后，侧边栏可延伸到窗口顶部，视觉更沉浸
3. **Windows 拖拽体验**：40px 拖拽区域比 28px 更易操作

### 代价

- Windows 需额外实现窗口控制逻辑（macOS 使用原生红绿灯）
- 部分系统主题下自定义按钮可能与背景色冲突（通过 Tailwind 工具类适配）

---

## ADR-047: 插件系统改进（Dialog 自监听 + 编辑支持）

**日期**: 2026-08-28  
**状态**: 已接受

### 背景

旧方案中，`PluginRegisterDialog` 通过 `forwardRef` + `useImperativeHandle` 由父组件控制显示。插件注册事件（`plugin-register`）在 `App.tsx` 监听后手动调用 `dialog.current.show()`。

问题：
1. 事件监听与控制逻辑分散在两个文件
2. 对话框无法显示插件详情（API 格式、模型数、密钥数）
3. 编辑已注册的插件供应商需手动导航到 ProviderFormView，无法直接从插件列表进入

### 决策

1. **Dialog 自监听事件**：`PluginRegisterDialog` 内部 `useEffect` 监听 `plugin-register`，移除 `App.tsx` 的事件监听
2. **显示更多详情**：对话框显示 API 格式、Base URL、模型数、密钥数（从 `PluginRegisterPayload` 读取）
3. **支持编辑插件供应商**：`ProviderFormView` 检测 `provider.config.plugin_id`，自动进入插件模式（隐藏 API Key 输入、禁用 kind/base_url）
4. **新增 `pluginsApi`**：`list` / `get` / `confirm` / `remove` 四个端点
5. **ProvidersView 使用 `pluginsApi`**：替代直接 `fetch('/api/plugins')`

### 原因

1. **职责内聚**：对话框自己管理事件监听与显示逻辑
2. **信息透明**：用户确认前可看到插件完整信息
3. **编辑便捷**：插件供应商与普通供应商共用编辑页，体验一致
4. **API 封装**：`pluginsApi` 统一插件相关请求，便于维护

### 代价

- `PluginRegisterDialog` 从受控组件变为非受控组件（父组件无法主动控制显示）

