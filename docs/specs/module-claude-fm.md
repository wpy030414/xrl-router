# Spec: Claude FM 播放器（内置氛围电台）

## 目标

应用内置一个「永远在广播」的氛围电台（PRD F-43）：内置歌单循环播放，
后端直接解码输出到系统音频设备，前端只负责展示元信息与控制播放暂停；
系统媒体控制（macOS Now Playing / Windows SMTC）同步播放态。
桌面壁纸联动见 module-pixel-wallpaper。

## 架构

```
控制源                        Rust（lib.rs setup）                     播放线程 (std::thread)
┌─ FmView（前端） ─┐     souvlaki MediaControls                FmEngine::engine_loop
├─ 托盘菜单勾选项        ─┼─▶  （主线程创建，回调经                  ├─ mpsc 收控制消息
└─ 系统媒体键（souvlaki）─┘     control_tx_clone 转发）              ├─ rodio OutputStream + Sink
                                                                     ├─ 墙钟种子定位 + 双缓冲预加载
                                                                     └─ 共享 Mutex<FmPlaybackState>
事件广播（进程级，主窗口 + 壁纸窗口都收到）:
  fm-ready / fm-meta {artist,title,index} / fm-state-changed {bool}
```

- **音频在后端**：rodio 在专用 `std::thread` 解码 MP3 直出系统音频，
  无前端 `<audio>` 缓冲层，多窗口间无同步误差。
- **前端是哑终端**：`useFm()` hook 订阅事件 + `fm_get_state` 拉快照，
  控制一律走 Tauri command → `FmEngine` mpsc。

## Radio 模式语义（核心）

- **暂停 = 静音，不是停时间轴**：`sink.set_volume(0.0)`，音频继续按实时速率消费，
  `sink.empty()` 照常触发切歌——与真正的收音机一致。
- **墙钟种子**：歌单总时长 `TOTAL_DURATION`（当前 4306s，21 首）作为取模周期；
  启动时用墙钟 `now % TOTAL_DURATION` 二分定位曲目 + 曲内偏移，首轮 `try_seek`
  到「此刻该在的位置」续播（修正启动下载的 1~3s 漂移）。
- **恢复播放 = re-anchor**：静音期间可能已切歌；恢复时重算墙钟位置——
  同曲则 seek 对齐 + 恢复音量，已切歌则 `sink.clear()` 后外层循环重新下载新曲。

## 输入契约（Tauri command）

| Command | 功能 |
|---------|------|
| `fm_toggle` / `fm_play` / `fm_pause` | 播放控制（前端 / 托盘 / 系统媒体键共用） |
| `fm_get_state` | 状态快照：`{ready, playing, artist, title, index, scene_t}` |
| `fm_scene_t` | 像素场景动画时钟（秒），供渲染侧轮询采样 |
| `fm_ready` | 前端预热完成回调 → 托盘菜单加入 FM 勾选项 |
| `fm_set_playing` | 前端状态变化回调 → 同步托盘勾选态 |

**系统媒体控制**：souvlaki 回调 `Toggle/Play/Pause` → `control_tx_clone()`
转发进同一 mpsc；切歌/播放态变化经 `run_on_main_thread` dispatch 更新
Now Playing 元数据与进度。

## 输出契约（事件）

| 事件 | 时机 | 载荷 |
|------|------|------|
| `fm-ready` | OutputStream 创建成功 | `()` |
| `fm-meta` | 引擎启动 + 每次切歌 | `{artist, title, index}` |
| `fm-state-changed` | 播放/暂停切换 | `bool` |

主窗口 `FmView` 与壁纸窗口 `WallpaperScene` 共用同一套事件接线。

## 场景时钟（`scene_t`，引擎权威）

像素艺术动画的唯一时钟源：仅播放（未静音）时按真实流逝累计，暂停冻结
（`tick_scene_t`，100ms 轮询推进）。主窗口与壁纸窗口各自经 `fm_scene_t`
采样同一共享状态——两处画面严格同步。切曲下载/解码的静默间隙照走，与音频一致。

## 音源获取

- 曲目内置（`TRACKS`，含 `netease_id` + 时长），音源在线解析：
  优先 `api.paugram.com` 解析接口取直链，失败回退网易云官方外链（302 → CDN）。
- **双缓冲预加载**：当前曲播放时后台 `tauri::async_runtime::spawn` 下载下一曲
  （`oneshot` 通道按索引绑定），切歌零等待；re-anchor 跳曲导致索引失配时丢弃重下。

## 关键约束

| 约束 | 原因 |
|------|------|
| rodio `OutputStream` 必须在播放线程内存活 | drop 即音频停止 |
| souvlaki 必须主线程创建/调用 | macOS MPRemoteCommandCenter / MPNowPlayingInfoCenter 要求 |
| 控制消息单一消费端（`Option::take`） | 防止 `spawn()` 被调两次导致双播放线程 |
| 播放状态只由控制消息驱动 | 切歌不覆写 `playing`，保持用户意图 |
| 歌单变更须同步 `TOTAL_DURATION` 与累计偏移表 | 墙钟二分依赖它 |
| 预热完成前托盘 FM 项隐藏 | 避免音源未就绪时误操作 |

## 错误处理

| 情况 | 行为 |
|------|------|
| 音源下载失败 | 记 warn，跳到下一首（电台不停播） |
| 解码失败 | 同上，跳过该曲 |
| OutputStream 创建失败 | 记 error，不 emit `fm-ready`（前端/托盘保持隐藏态） |
| seek 失败 | 记 warn，从头播（不阻断） |

## 实现位置

- Rust：`src-tauri/src/api/handlers/fm.rs`（引擎）+ `lib.rs`（command / souvlaki / 托盘）
- 前端：`src/views/FmView.tsx`、`src/hooks/useFm.ts`、
  `src/components/PixelScene.tsx`（像素画布，`sampleT` 采样 `fm_scene_t`）

## 测试要求

- 暂停/恢复后墙钟对齐：静音超过一曲再恢复，应播「现在电台在播的那首」而非暂停处。
- 托盘勾选、系统媒体键、前端按钮三入口状态一致。
- 壁纸窗口与应用内像素画面同步（见 module-pixel-wallpaper）。

## 完成标准

- [x] 21 首内置歌单循环广播，墙钟种子定位
- [x] 三控制入口（前端/托盘/媒体键）统一走 mpsc
- [x] Now Playing 元数据 + 播放态同步（macOS/Windows）
- [x] 双缓冲预加载切歌零等待
- [x] `scene_t` 引擎权威时钟，主窗口/壁纸画面同步
