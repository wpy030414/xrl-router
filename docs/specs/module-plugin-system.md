# Spec: 插件系统

## 目标

允许外部服务通过 WebSocket 注册为"委托供应商"（反向代理网关），处理非标 API 协议和业务头注入。

**V24 契约要点**：Router **不再为插件管理密钥**——凭证由插件方自持（login 流程获取）；
每个插件实现必须是一个 **TypeScript + Hono 网关**，通过 package.json 脚本暴露 `login` / `serve` 生命周期；
Router 可托管插件网关进程（伴生启动）。

## 架构

```
插件（TS + Hono 反向代理网关，独立进程）
    ↓ WebSocket（控制面：注册/心跳/配置）
xrl-router
    ↓ HTTP（数据面：标准协议请求 + 占位凭证）
插件网关
    ↓ 插件自身凭证
上游 API
```

**职责分工**:

| 职责 | Router | Plugin |
|------|--------|--------|
| 协议转换 | ✅（标准三格式 ↔ IR） | ✅ 非标 → 标准 |
| 业务头注入 | ❌ | ✅ |
| **密钥/凭证管理** | ❌（V24 起剥离） | ✅（login 流程自持） |
| 健康监控 | ✅（心跳 + provider 冷却） | — |
| 用量统计 | ✅ | — |
| 进程托管（伴生启动） | ✅（可选） | — |

## 反向代理实现契约（V24）

每个插件实现（反向代理）必须满足：

1. **形态**：一个 TypeScript 项目，HTTP 网关基于 **Hono**。
2. **package.json 脚本**：
   - `login` — 弹出**系统浏览器**网页让用户完成上游登录（OAuth/账密/扫码等由插件实现决定），
     捕获会话后持久化到插件自己的存储。**绝不能要求客户端（LLM 消费端）到位或做任何配合**。
     登录成功后以退出码 0 结束。
   - `serve` — 启动 Hono 网关并连 WS `ws://127.0.0.1:19068/ws/plugin` 注册。要求：
     - **幂等**：若已有实例在监听（端口占用/锁文件），干净退出（exit 0），不得抢占；
     - **容忍 Router 未就绪**：WS 连接失败时以退避重试，直到 Router 启动；
     - 每 30s 发心跳。
3. **运行时依赖**：系统内可用 `node` 与 `pnpm`（Router 托管前置条件）；依赖安装（`pnpm install`）
   由插件使用方负责，Router 只运行 `pnpm run serve/login`。
4. **凭证**：Router 发来的请求携带**占位凭证**（见下节），插件必须忽略，用自身凭证访问上游。

### 占位凭证契约

Router 对插件上游请求**不注入任何真实密钥**，恒发占位值 `xrl-router`：

| provider kind | 携带的头 |
|---------------|---------|
| `messages` | `x-api-key: xrl-router` + `anthropic-version: 2023-06-01` |
| `chat_completions` / `responses` | `Authorization: Bearer xrl-router` |

实现上这由代理层的虚拟密钥机制承载：插件候选在密钥轮换处恒返回合成
`PickedKey { key_hash: "xrl-router", id: "plugin:{provider_id}" }`，内层重试恒为 1 次。

### 可用性反馈契约

插件按业务实际情况返回标准状态码，Router 的 failover / provider 冷却机制据此反应：

| 插件返回 | Router 行为 |
|---------|------------|
| 401 / 403（会话失效） | 透传错误（占位密钥无健康黏性，每请求仍会重新尝试——自愈） |
| 402 / 429（配额/限流） | 透传错误 |
| 5xx / 网络错误 / 响应头超时 | provider 冷却 60s，切换下一候选 |
| 2xx | 正常转发 |

**注意**：占位密钥不在密钥池中，`update_key_health` 对其天然 no-op——插件会话恢复后
无需任何恢复通道，下一个请求即恢复服务。

## WebSocket 协议

端点：`/ws/plugin`（公开路径，不限 IP）。

### 插件 → Router

#### register（注册，首条消息）

```json
{
  "type": "register",
  "plugin_id": "plugin-wukong",
  "provider": {
    "kind": "chat_completions",
    "base_url": "http://localhost:19067",
    "api_path": "/v1/chat/completions"
  },
  "models": [
    {"model_id": "dingtalk-auto", "display_name": "钉钉 Auto", "tier": "custom"}
  ],
  "workdir": "C:/Users/me/plugins/wukong-gateway"
}
```

- `keys` 字段**已废除**：**字段存在即严格拒绝**（无论空数组、null 还是有值），
  Router 回 `{"type":"error","reason":"keys_not_supported"}` 并断开连接，强制插件方升级。
- `workdir`（可选）：插件项目根目录，伴生启动 `pnpm run serve/login` 的 cwd。
  持久化语义为 COALESCE——仅非空时覆盖，不抹掉历史值。

#### heartbeat（心跳）

```json
{"type": "heartbeat"}
```

**频率**: 每 30 秒（服务端忽略客户端发来的 timestamp，使用 `Utc::now()`）

#### config_update（配置热更）

```json
{"type": "config_update", "base_url": "http://localhost:19068", "api_path": "/v1/x"}
```

#### keys_update（已废除）

收到即回 `{"type":"error","reason":"keys_not_supported"}` 并断开连接。
Router 不再为插件管理密钥（V24 迁移同时清理历史同步进来的密钥行）。

### Router → 插件

#### registered（注册成功）

```json
{"type": "registered", "plugin_id": "plugin-wukong", "provider_id": "provider_xxx", "status": "pending_confirmation"}
```

#### reconnected（重连）

```json
{"type": "reconnected", "provider_id": "provider_xxx"}
```

#### error（协议拒绝）

```json
{"type": "error", "reason": "keys_not_supported" | "expected_register" | "empty_plugin_id" | "invalid_register"}
```

#### deleted（插件已被用户删除）

```json
{"type": "deleted", "reason": "plugin_ignored"}
```

## 输入契约

### 插件注册

```rust
pub fn handle_register(
    state: &AppState,
    msg: RegisterMsg
) -> Result<String>  // 返回 provider_id
```

**PluginRegisterMsg**:

```rust
pub struct PluginRegisterMsg {
    pub plugin_id: String,
    pub provider: ProviderInfo,
    pub models: Vec<ModelInfo>,
    pub workdir: Option<String>,  // 伴生启动 cwd（COALESCE 语义）
}

pub struct ProviderInfo {
    pub kind: String,
    pub base_url: String,
    pub api_path: String,
}
```

register 首条消息的校验在原始 `serde_json::Value` 上进行（serde 忽略未知字段，
`keys` 的存在性必须在反序列化前判定）：`parse_register_msg(&Value) -> Result<PluginRegisterMsg, RegisterReject>`。

### 心跳检测

```rust
pub fn check_heartbeats(state: &AppState) {
    // 每 30 秒执行一次
    // 检查所有插件的 last_heartbeat_at
    // 超过 90 秒未心跳 → 标记离线
}
```

## 输出契约

### 插件状态

插件状态使用纯字符串（非枚举），取值为：

- `"pending"` — 已注册，等待用户确认
- `"active"` — 已确认，正常服务
- `"offline"` — 心跳超时

### Provider 配置

```rust
pub struct ProviderConfig {
    pub plugin_id: Option<String>,
    // ... 其他配置
}
```

**委托供应商标识**: `config_json.plugin_id` 非空

### plugins 表（V24 起）

| 列 | 说明 |
|----|------|
| `work_dir TEXT` | 插件上报的项目根目录（伴生启动 cwd） |
| `autostart INTEGER NOT NULL DEFAULT 0` | 伴生启动开关 |

## 伴生启动（进程托管）

前置条件：系统内 `node` 与 `pnpm` 可用（Router 启动时探测并缓存，`GET /api/plugins/runtime`
返回版本或 null；不可用时 UI 的「伴生启动」「登录」入口禁用并提示原因）。

- **开关**：插件供应商卡片菜单「伴生启动」（`PATCH /api/plugins/:id` 更新 `autostart`）。
- **拉起时机**：XRL Router 启动、网关 listener 就绪后，对 `autostart=1` 且 `work_dir` 非空的
  插件逐个 `pnpm run serve`（日志落 `{data_dir}/logs/plugins/{plugin_id}.log`，覆盖式）。
- **已启动不重复启动**：① 已在 Router 托管列表 → 跳过；② provider 的 base_url 做 1s TCP 探测，
  连通（用户手动启动/上次残留实例）→ 跳过；③ serve 脚本幂等契约兜底。
- **登录**：卡片菜单「登录」（`POST /api/plugins/:id/login`）在 work_dir 运行 `pnpm run login`
  （短命一次性进程，日志 `{plugin_id}-login.log`），脚本自行弹系统浏览器。
- **退出清理**：应用退出（RunEvent::Exit）时回收**Router 自己 spawn** 的 serve 进程——
  Windows `taskkill /T /F`（杀祖孙进程树 pnpm.cmd → cmd → node），unix `kill -9 -- -pgid`
  （spawn 时设独立进程组）。用户手动启动的网关不受影响。
- **不做崩溃重启 watcher**：托管进程意外退出后由心跳超时自然标记离线。

## 管理 API

| 路由 | 说明 |
|------|------|
| `GET /api/plugins` | 列表（含 work_dir / autostart / connected） |
| `GET /api/plugins/runtime` | node/pnpm 版本或 null（托管可用性） |
| `GET /api/plugins/:id` | 详情（provider + models + work_dir + autostart） |
| `PATCH /api/plugins/:id` | 更新 `{autostart?, work_dir?}`（work_dir 校验目录存在） |
| `POST /api/plugins/:id/confirm` | 确认激活 |
| `POST /api/plugins/:id/login` | 运行 `pnpm run login`（202；work_dir 缺失/runtime 缺失 → 400） |
| `DELETE /api/plugins/:id` | 删除（先回收托管进程，级联 Provider + Models） |

## 关键约束

1. **独立进程**: 插件运行在独立进程，崩溃不影响 Router
2. **心跳机制**: 每 30 秒发送心跳，90 秒无心跳标记离线
3. **无密钥职责**: Router 不存储/轮换/注入插件密钥；占位凭证恒为 `xrl-router`
4. **严格拒绝**: register 带 `keys` 或 `keys_update` 消息 → 回错并断开
5. **模型管理**: 插件注册时提供模型列表，Router 存储到 models 表
6. **生命周期**: 注册 → 确认 → 服务 → 离线/删除
7. **autostart 不可被注册重置**: `save_plugin` 的 UPSERT 不含 autostart 列

## 错误处理

| 场景 | 行为 |
|------|------|
| WebSocket 连接断开 | 插件自动重连，Router 标记离线 |
| 心跳超时 | 标记 `status=offline`，`providers.enabled=0` |
| register 带 `keys` / `keys_update` | 回 `keys_not_supported` 错误并断开 |
| 插件重复注册 | 更新已有记录，不创建新 Provider |
| 插件删除 | 回收托管进程 + 删除 Provider + Models |
| node/pnpm 不可用 | 伴生启动整体跳过（warn 日志），UI 入口禁用 |
| work_dir 不存在 | spawn 失败记日志；PATCH 时 400 拒绝 |

## 实现位置

- `src-tauri/src/plugin/mod.rs` - 插件管理器（DB helpers、autostart/workdir 写入）
- `src-tauri/src/plugin/registry.rs` - 注册逻辑
- `src-tauri/src/plugin/health.rs` - 心跳检测
- `src-tauri/src/plugin/host.rs` - 伴生启动（进程托管：runtime 探测 / spawn / kill）
- `src-tauri/src/plugin/types.rs` - 类型定义
- `src-tauri/src/api/handlers/plugin.rs` - WebSocket 处理 + 管理 API
- `src-tauri/src/api/proxy/key_rotation.rs` - 虚拟占位密钥（`pick_key_for` 插件分支）

## 测试要求

1. **单元测试**: `parse_register_msg`（keys 拒绝/空数组拒绝/workdir 解析）、workdir 持久化与
   autostart 不被重连重置、`pick_key_for` 插件分支
2. **集成测试**: 插件注册 → 占位凭证转发（上游收到 `Bearer xrl-router`）→ usage_log 归属
3. **故障测试**: 插件回 401/5xx 时错误透传与 provider 冷却

## 完成标准

- [x] WebSocket 连接管理
- [x] 插件注册（创建 Provider + Models；无密钥）
- [x] 心跳检测（30s 间隔，90s 超时）
- [x] 严格拒绝 keys（register 字段存在 / keys_update 消息 → 回错断开）
- [x] 虚拟占位密钥放行（`xrl-router`，单次尝试，无健康黏性）
- [x] 伴生启动（runtime 探测 / TCP 去重 / 退出回收）
- [x] 离线标记（`status=offline`，`enabled=0`）
- [x] 插件删除（回收托管进程 + 级联删除 Provider + Models）
- [x] 通过所有单元测试和集成测试
