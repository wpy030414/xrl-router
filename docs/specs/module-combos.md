# Spec: 组合别名（Combo）

## 目标

允许用户把多个模型别名（`models.display_name`）按顺序捆绑成一个新的「组合别名」。客户端用组合别名连接路由时，路由按成员顺序依次尝试，直到找到可用模型——相当于显式构建的退路链。

## 数据模型（V18）

```sql
CREATE TABLE combos (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,          -- 组合别名（暴露给客户端）
    enabled INTEGER DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE combo_members (
    id TEXT PRIMARY KEY,
    combo_id TEXT NOT NULL,
    member_alias TEXT NOT NULL,         -- TEXT 软引用 models.display_name
    position INTEGER NOT NULL DEFAULT 0, -- 尝试顺序
    FOREIGN KEY (combo_id) REFERENCES combos(id) ON DELETE CASCADE,
    UNIQUE(combo_id, member_alias)
);
```

关键语义：

- **成员只能是模型别名（叶子）**，不允许嵌套组合 → 天然无环，无需运行时环检测。
- **`member_alias` 是 TEXT 软引用**（`display_name` 非唯一，无法建硬 FK）：成员别名被删除/禁用 → 运行时跳过；组合行本身不受影响。
- **名称冲突双向校验**（handler 层）：
  - 创建/更新组合时：`name` 不得撞任意 `models.display_name`（`db.model_display_name_exists`）。
  - 创建/更新模型 `display_name` 时：不得撞任意 `combos.name`（`db.combo_name_exists`，见 `api/handlers/models.rs`）。
  - 大小写敏感（SQLite BINARY 排序规则）：`"MyCombo" ≠ "mycombo"`。
  - 并发 TOCTOU 由 `combos.name UNIQUE` 兜底（rusqlite 2067 → 400 "name already exists"）。

## 解析与回退语义

### 展开（`api/proxy/route.rs::resolve_combo`）

- `None` → 名字不是 **enabled** 组合（不存在或被禁用）→ 调用方回落普通别名解析（向后兼容：任何非组合名走原路径）。
- `Some(vec)` → 是组合，按 `position` 展开全部候选；vec 可为空（全成员不可解析）→ 调用方映射 400 `"Model not found or not available"`。
- 逐成员调用 `resolve_route_candidates`（成员自身的跨 provider 候选、`sort_order` 排序、插件委托覆盖全部继承），拼接后按 **(provider_id, real_model_id)** 跨成员去重保序。
  - **不能按 provider_id 去重**：组合 `[opus, sonnet]` 同在一个 provider 时要保留两条。
- 实现注意：先在一个块内收集成员别名（释放 `MutexGuard` 后再 `await`），`std::sync::Mutex` guard 跨 await 会死锁。

### 回退触发（`api/proxy/stream.rs` 双循环）

组合命中后强制 `failover = true`（`global_failover || is_combo_req`）——**成员间回退不受全局「故障转移」开关影响**；普通别名保持现有行为。外层循环所有 `continue 'provider` 门（冷却跳过、密钥耗尽、网络错误、响应头超时、5xx）自动生效。

| 上游行为 | 分类 | 组合处理 |
|---|---|---|
| 连接失败 / 响应头超时 | 供应商级 | 换下一个成员 |
| 5xx | 供应商级 | 换下一个成员 |
| 401 / 402 / 403 / 429 | 供应商级 | 换 key，密钥池耗尽后换下一个成员 |
| 400 + 配额错误体（`quota`/`insufficient`） | 供应商级 | 换 key，密钥池耗尽后换下一个成员 |
| 200 + 空流（0 个 SSE 事件，未发内容） | 供应商级 | 换 key / 换下一个成员 |
| 200 + SSE error event / 非 SSE JSON 错误体（未发内容） | 供应商级 | 换 key / 换下一个成员 |
| **普通 400（非配额）** | **请求级** | **立即透传，不试下一个成员**（`break 'provider`） |

失败兜底：全部成员耗尽后按 ProviderFailure / last_resp / last_resp_body 透传（沿用现有机制），每次失败写 `usage_log`。

### 冷却与同 provider 跳过

组合内同一 provider 的后续成员，在 provider 级失败（5xx/网络/超时）后会被 `mark_provider_failed` 的 **60s 冷却**跳过（`is_provider_cooling`）。这是期望语义——provider 是失败单元，同一坏 provider 上的第二个成员继续打没有意义。**不是 bug，不要"修复"**。

### 服务密钥白名单（零后端改动）

`api/proxy/handler.rs` 白名单是纯字符串比对：**授予组合名 = 授予其全部成员**；只授予成员名而调用组合 → 403。前端 `KeysView` 权限对话框把组合列为独立分组。

### GET /v1/models

追加 enabled 组合条目（`id`/`display_name`=组合名、`owned_by:"combo"`、`tier:"combo"`、数值字段 0/空数组）。不校验运行时可解析性（只列 enabled 组合），解析失败时调用方在请求时收到 400。白名单过滤按 `display_name` 天然覆盖。

## 管理 API 契约

`GET/POST /api/combos`、`GET/PUT/DELETE /api/combos/:id`（仅 loopback，参照 `/api/models`）。

### POST /api/combos

```json
// 请求
{"name": "my-combo", "members": ["my-opus", "my-sonnet"], "enabled": true}
// 201 响应
{"id": "...", "name": "my-combo", "enabled": true, "members": ["my-opus", "my-sonnet"], "created_at": 123, "updated_at": 123}
```

校验规则（create/update 共用）：

- `name`：trim 非空；不撞任何 `models.display_name` → 400。
- `members`：trim、非空、保序去重；全部必须是现存 `display_name` → 未知成员 400 并列出（如 `"Members not found as model aliases: x, y"`）。
- 不校验成员 `enabled`（成员可能暂时禁用，用户可能先建组合后启用；运行时全空 → 400）。

错误码：不存在 → 404；冲突/未知成员/空成员 → 400；`save_combo` 返回 UNIQUE 违例（2067）→ 400；其他 DB 错误 → 500。

## 统计口径

`usage_log` 零改动：`model_display_name` = 客户端请求用的别名（即组合名），`model_id` = 实际命中的成员模型行。因此：

- `top_model` 按 `model_id` 聚合 → 组合用量计入**实际成员模型**。
- 请求日志 `model_display_name` 显示组合名。

## 管理 UI

- 侧边栏「供应商」下方新增「组合」入口（`nav.combos`，mdiSetMerge）。
- `CombosView.tsx`：卡片列表（组合名 + 禁用 chip + 带序号成员 chips + 编辑/删除菜单）。
- `ComboFormView.tsx`：名称 + 按 provider 分组的成员多选（勾选顺序 = 尝试顺序，可上移/下移/移除）+ 启用开关。
- `KeysView` 权限对话框：组合独立分组。

## 测试矩阵

| 场景 | 期望 |
|---|---|
| 组合按成员顺序展开 | 候选顺序 = position 顺序 |
| 同 provider 两个不同成员 | 两条都保留（不按 provider_id 去重） |
| 非组合名字 | `resolve_combo` = None，回落普通解析 |
| 组合被禁用 | `resolve_combo` = None → 400 |
| 成员全部不可解析 | `Some(vec![])` → 400 |
| 成员别名被删/禁用 | 运行时跳过该成员 |
| 插件离线的委托成员 | 跳过（继承 `resolve_route_candidates` 语义） |
| 全局 failover 关闭 + 组合 5xx | 仍换成员（强制回退） |
| 组合 + 普通 400 | 立即透传错误体，不换成员 |
| 组合 + 400 配额错误 | 换成员 |
| 白名单含组合名 | 放行 |
| 白名单只含成员名 | 403 |
| /v1/models | 含 `owned_by:"combo"` 条目 |
| 组合名撞 display_name / 反向 | 创建/更新被 400 拒绝 |
| 数据导出/重置 | combos / combo_members 在表清单中 |

## 实现位置

- `src-tauri/src/db/schema.rs` - V18 迁移
- `src-tauri/src/db/combos.rs` - CRUD（save_combo 事务重插成员）
- `src-tauri/src/api/proxy/route.rs` - `resolve_combo` + 单测
- `src-tauri/src/api/proxy/stream.rs` - 解析段组合优先 + 普通 400 `break 'provider`
- `src-tauri/src/api/handlers/combos.rs` - 管理 API + 校验
- `src-tauri/src/api/handlers/models.rs` - display_name 反向冲突校验
- `src/views/CombosView.tsx` / `ComboFormView.tsx` - 管理 UI
- `src-tauri/src/gateway/server.rs` - `test_combo_end_to_end` E2E

## 完成标准

- [x] V18 迁移 + combos / combo_members CRUD（含事务重插、级联删除测试）
- [x] `resolve_combo` 展开 + 单测（顺序/去重/跳过/禁用/空）
- [x] 组合强制回退（全局开关关闭时 E2E 验证）
- [x] 普通 400 立即透传（E2E 验证）
- [x] 400 + 配额换成员（E2E 验证）
- [x] 白名单按组合名授予（E2E 验证）
- [x] /v1/models 含组合条目（E2E 验证）
- [x] 名称双向冲突校验
- [x] 侧边栏「组合」入口 + 列表/表单 UI + 权限对话框组合分组
- [x] 数据导出/重置表清单更新
