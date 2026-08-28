# Spec: FM 像素艺术桌面壁纸（Pixel Wallpaper）

## 目标

右击 FM 像素艺术画面勾选「设置为桌面背景」后，桌面壁纸被劫持为与应用程序内
像素艺术**严格同步**的动画（不显示播放/暂停按钮与歌曲信息）；取消勾选即刻
恢复原壁纸；勾选态持久化，应用重启（含 `--minimized` 静默启动）自动重建。

## 架构

```
┌─ 主窗口 "main"(ClaudeFmView) ──┐   ┌─ 壁纸引擎（Windows GDI / macOS WebView）──┐
│ PixelScene(seed,playing,sampleT)│   │ Win: painter 线程 → WorkerW 子窗 GDI 5fps │
│ ContextMenu: 设置为/取消桌面背景 │   │ mac: WallpaperScene(黑底全屏+grayscale)    │
└──────────────┬──────────────────┘   └────────────────┬───────────────────────┘
    invoke: wallpaper_set /           listen: fm-meta / fm-ready /
    wallpaper_get_state / fm_scene_t  fm-state-changed（进程级广播）
               ▼                                     ▼
   ┌──────────────────────────────────────────────────────────┐
   │ FmEngine（std::thread，'wait 100ms 轮询 + 'outer 每轮）      │
   │   FmPlaybackState.scene_t: f64（仅 !muted 按 Instant 流逝累加）│
   │   FmEngine::state_arc() → 共享 Arc（壁纸线程零 IPC 直读）     │
   └──────────────────────────────────────────────────────────┘
   ┌──────────────────────────────────────────────────────────┐
   │ wallpaper::WallpaperState（app.manage）                    │
   │   enable：Win 起 painter 线程（幂等）/ mac 建窗主线程挂载     │
   │   disable：旗标 → Win 线程销毁窗口退出 / mac 销窗            │
   │   DB settings.wallpaper_enabled；Win 外部销毁 1s 自愈重建    │
   └──────────────────────────────────────────────────────────┘
```

- **Windows 渲染**：透明 WebviewWindow（`transparent(true)`——关键词，
  WebView2 内容经 DWM 视觉合成上屏，是桌面 WorkerW 层唯一可靠渲染路径），
  `tauri-plugin-desktop-underlay` 的 `set_desktop_underlay(true)` 挂载进壁纸
  WorkerW；WebView2 显式 `--disable-features=CalculateNativeWinOcclusion`
  （避免遮挡检测暂停合成）+ 独立 user data directory（避免与主窗口环境
  参数冲突 0x8007139F）；点击穿透 `WS_EX_TRANSPARENT`（顶层 + 递归子窗
  1s 补轮，**禁用 WS_EX_LAYERED**——分层窗口不设属性不显示内容）。
  画面仍由前端同一份 `pixelart.ts` 渲染（`WallpaperScene` 黑底全屏）。
- **时钟**：`scene_t` 仅播放（未静音）时按真实流逝累计、暂停冻结；主窗口与
  壁纸窗口的 `PixelScene` 都经 `fm_scene_t` 采样（失败回退本地 dt）——
  同源，严格同步。
- **macOS 渲染**：第二个 WebviewWindow（`initialization_script` 注入
  `window.__WALLPAPER_MODE__ = true`；**不用 URL query**——未固定行为），
  前端按标志分支渲染 `WallpaperScene`；`NSWindow.setLevel(
  kCGDesktopIconWindowLevel)` + `orderFront:`（禁止 tao show 抢焦点）
  + `setIgnoresMouseEvents` 穿透。

## 输入契约

### Tauri command（前端 → Rust）

```rust
wallpaper_set(enabled: bool) -> Result<bool, String>   // 返回实际状态（勾选态）
wallpaper_get_state() -> { enabled: bool, supported: bool }
fm_scene_t() -> Result<f64, String>                    // 场景动画时钟（秒）
```

- `wallpaper_set(true)`：幂等建窗 + 挂载 + 写 DB；已启用时仅置位。
- `wallpaper_set(false)`：先置 `enabled=false` 再销毁（防止 Destroyed 重建）。

### DB settings

```sql
INSERT INTO settings (key, value) VALUES ('wallpaper_enabled', 'true'|'false')
ON CONFLICT(key) DO UPDATE SET value = excluded.value;
```

## 关键约束

1. **平台**：仅 Windows / macOS（`wallpaper_supported` 为 false 时前端隐藏
   菜单项）；v1 仅主显示器。
2. **Windows 壁纸窗**：普通 `WS_CHILD` 窗口（无浏览器进程），创建时带
   `WS_EX_TRANSPARENT|WS_EX_LAYERED|WS_EX_NOACTIVATE|WS_EX_TOOLWINDOW`
   （点击穿透 + 不聚焦 + 不进任务栏/切换器）；WndProc 仅 DefWindowProc，
   内容直绘进 DC，不依赖 WM_PAINT/消息泵。
3. **macOS 壁纸窗**：WebviewWindow 无装饰、`focused(false)`、`skip_taskbar`、
   不可见创建 + 黑底；`orderFront:` 呈现（禁止 tao `show()`——抢焦点）。
4. **同步一致性**：同一时刻两处渲染同一帧（同 seed/playing/t）；暂停 →
   双侧同时黑白静止（grayscale + frozen t）；恢复 → 同刻续走不跳位；
   切歌（`fm-meta`）→ 同刻换画。
5. **生命周期**：`enabled` 与渲染资源分离——禁用先置位再销毁；Windows
   外部销毁（Explorer 重启）→ 线程外层循环 1s 后重建；macOS
   `Destroyed` → 清槽 1s 复查重建；进程退出由 OS 清理，原壁纸自动恢复。
6. **幂等**：`enable` 重复调用无副作用（窗口在槽位则仅置位）。
7. **渲染一致性**：壁纸与主窗口渲染同一份 `pixelart.ts`（同 seed/playing/t
   参数），时钟同源（`fm_scene_t`）——任何渲染改动两侧同步可见。

## 错误处理

| 场景 | 行为 |
|------|------|
| 非支持平台（Linux）| `wallpaper_set` 返回 Err；`supported=false` → 前端不渲染菜单项 |
| WorkerW 宿主找不到 | 画家线程等 1s 重试（Explorer 未就绪等瞬态）；期间桌面保持原壁纸 |
| 建窗失败 | 记 warn，1s 后重试；不残留半成品（无 tauri 注册表介入） |
| DB 持久化失败 | 仅 warn 日志，不阻断（窗口已生效，重启后可能需重新勾选） |
| Explorer 重启 | Win：IsWindow 检测 → 1s 自愈重建；mac：Destroyed → 1s 重建 |
| 时钟采样失败（macOS 前端）| `PixelScene` 回退本地 dt 累加，不中断渲染 |
| WebView2 初始化竞态（macOS 恢复）| 恢复延迟 2s + 重试 3 次（主窗口 webview 就绪后） |

## 实现位置

- `src-tauri/src/wallpaper/mod.rs` — WallpaperState + 建窗/挂载/自愈重建
  （挂载经 `tauri-plugin-desktop-underlay`，双平台）
- `src-tauri/src/wallpaper/win.rs` — Windows GDI 直绘（WorkerW 子窗 + StretchDIBits）
- `src-tauri/src/wallpaper/macos.rs` — macOS kCGDesktopIconWindowLevel
- `src-tauri/src/api/handlers/fm.rs` — `scene_t` 引擎权威时钟 + `state_arc()`
- `src-tauri/src/lib.rs` — `wallpaper_set` / `wallpaper_get_state` / `fm_scene_t`
  command + setup 状态管理与惰性恢复
- `src/main.tsx` — `__WALLPAPER_MODE__` 入口分支（macOS 壁纸窗口用）
- `src/components/WallpaperScene.tsx` — 壁纸窗口渲染入口（macOS；无按钮/歌曲信息）
- `src/hooks/useFm.ts` — FM 事件接线（主窗口/壁纸窗口共用）
- `src/components/PixelScene.tsx` — `sampleT` 引擎时钟采样
- `src/views/ClaudeFmView.tsx` — 右键 ContextMenu（设置为桌面背景）
- `src/components/ui/context-menu.tsx` — Radix 右键菜单组件
- `src-tauri/capabilities/default.json` — windows 数组含 `"wallpaper"`（macOS）
- `src-tauri/Cargo.toml` — target 依赖（windows 0.61 Gdi/LibraryLoader / objc2 0.6）

## 测试要求

1. **编译**：`cargo check`（Windows 本机；macOS cfg 由 CI macos-14 job 兜底）；
   `pnpm exec tsc --noEmit`。
2. **手工冒烟（Windows 11）**：
   - 勾选后桌面出现主屏像素动画；任务栏/Alt-Tab 无此窗口；无装饰；
   - 桌面图标可点、桌面右键菜单正常（点击穿透）；
   - 播放/暂停/切歌与主窗口全程同步（含 grayscale/冻结/续走）；
   - 壁纸无播放按钮、无歌曲信息；
   - 取消勾选 → 原壁纸恢复；托盘 Quit 后恢复；
   - 勾选后重启（含 `--minimized`）→ 自动重建；
   - `taskkill /f /im explorer.exe` 后重启 Explorer → ~1-2s 自动重建。
3. **手工冒烟（macOS 真机）**：层级在壁纸图之上/图标之下、图标可点、
   不抢键盘焦点、随 Space/全屏跟随、退出恢复。
4. **多语言**：zh-CN/en 菜单文案切换跟随。

## 完成标准

- [x] 右键菜单「设置为桌面背景/取消桌面背景」（i18n 双包）
- [x] 桌面壁纸动画与主窗口严格同步（同 seed/playing/时钟）
- [x] 壁纸无播放按钮、无歌曲信息；点击穿透；不进任务栏/切换器
- [x] 暂停 → 双侧同时黑白静止；恢复续走不跳位；切歌同换画
- [x] 取消勾选 / 应用退出 → 原壁纸恢复
- [x] 重启（含 --minimized）自动恢复勾选态
- [x] Explorer 重启自动重建（Destroyed → 1s 延迟）
- [x] `cargo check` + `tsc --noEmit` 通过
- [ ] macOS 真机验证（CI 编译兜底；运行时验证待真机）
