# Spec: 插件系统

## 目标

允许外部服务通过 WebSocket 注册为"委托供应商"，处理非标 API 协议和业务头注入。

## 架构

```
插件（独立进程）
    ↓ WebSocket
xrl-router
    ↓ HTTP
上游 API
```

**职责分工**:
- **插件**: 协议转换、业务头注入、提供 base_url/api_path
- **Router**: 密钥轮换、健康监控、用量统计

## WebSocket 协议

### 插件 → Router

#### register（注册）

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
  "keys": ["sk-xxx", "sk-yyy"]
}
```

#### heartbeat（心跳）

```json
{
  "type": "heartbeat"
}
```

**频率**: 每 30 秒（服务端忽略客户端发来的 timestamp，使用 `Utc::now()`）

#### keys_update（密钥同步）

```json
{
  "type": "keys_update",
  "provider_id": "provider_xxx",
  "keys": ["sk-new1", "sk-new2"]
}
```

### Router → 插件

#### registered（注册成功）

```json
{
  "type": "registered",
  "plugin_id": "plugin-wukong",
  "provider_id": "provider_xxx"
}
```

#### keys_ack（密钥同步确认）

> **注意**: 当前代码中 Router 处理 `keys_update` 后返回 `Result<usize>`（added count），但**未发送** `keys_ack` 消息回插件。此消息为预留设计。

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
    pub keys: Vec<String>,
}

pub struct ProviderInfo {
    pub kind: String,
    pub base_url: String,
    pub api_path: String,
}
```

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
    pub penetrate_url: Option<String>,
    // ... 其他配置
}
```

**委托供应商标识**: `config_json.plugin_id` 非空

## 关键约束

1. **独立进程**: 插件运行在独立进程，崩溃不影响 Router
2. **心跳机制**: 每 30 秒发送心跳，90 秒无心跳标记离线
3. **密钥同步**: 插件检测到密钥变化时主动推送
4. **模型管理**: 插件注册时提供模型列表，Router 存储到 models 表
5. **生命周期**: 注册 → 确认 → 服务 → 离线/删除

## 错误处理

| 场景 | 行为 |
|------|------|
| WebSocket 连接断开 | 插件自动重连，Router 标记离线 |
| 心跳超时 | 标记 `status=offline`，`providers.enabled=0` |
| 密钥同步失败 | 记录 warn 日志，继续服务 |
| 插件重复注册 | 更新已有记录，不创建新 Provider |
| 插件删除 | 删除 Provider + Models + API Keys |

## 实现位置

- `src-tauri/src/plugin/mod.rs` - 插件管理器
- `src-tauri/src/plugin/registry.rs` - 注册逻辑
- `src-tauri/src/plugin/keys.rs` - 密钥同步
- `src-tauri/src/plugin/health.rs` - 心跳检测
- `src-tauri/src/plugin/types.rs` - 类型定义
- `src-tauri/src/api/handlers/websocket.rs` - WebSocket 处理

## 测试要求

1. **单元测试**: 注册、心跳、密钥同步逻辑
2. **集成测试**: 插件注册 → 服务 → 离线完整流程
3. **并发测试**: 多插件同时连接
4. **故障测试**: 插件崩溃、网络断开

## 完成标准

- [x] WebSocket 连接管理
- [x] 插件注册（创建 Provider + Models + API Keys）
- [x] 心跳检测（30s 间隔，90s 超时）
- [x] 密钥同步（keys_update 消息）
- [x] 离线标记（`status=offline`，`enabled=0`）
- [x] 插件删除（级联删除 Provider + Models + Keys）
- [x] 通过所有单元测试和集成测试
