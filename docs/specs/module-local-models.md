# Spec: 本地模型管理（Local Models）

## 目标

把开源 GGUF 模型变成「零配置私有 Provider」：应用内浏览 HuggingFace → 一键下载（或导入本地已有
GGUF 文件）→ 自动启动 `llama-server` → 以普通 Provider/Model 身份注册进网关（`http://127.0.0.1:<port>/v1`），
与云端 Provider 一样参与路由、组合、密钥与统计体系。对应 PRD §10「本地模型规格」。

## 架构

```
前端                                  Rust
LocalModelsView.tsx ─┐                local/mod.rs     LocalManager（注册表 + 生命周期）
HfBrowseView.tsx   ─┼─ REST /api/local/* ├─ engine.rs   llama-server 二进制管理 + 健康检查
                    │                    ├─ backend.rs  GPU 后端检测（Metal/CUDA/Vulkan/ROCm/CPU）
                    │                    └─ hf.rs       HuggingFace API 客户端（搜索/详情/下载）
                    ▼
        注册后作为普通 Provider 进入网关（base_url = 本机端口）
```

- **引擎**：外部二进制 `llama-server`（llama.cpp 发行版），首次使用时下载/缓存到应用数据目录；
  每个模型一个进程，绑定 `127.0.0.1` 上的独立端口。
- **注册为 Provider**：本地模型启动后在 DB 中维护 `local_models` 记录，并映射出
  Provider/Model 语义（见 PRD §10.3）；网关侧对它的转发与云端 Provider 无差别。
- **状态**：`LocalManager` 持有引擎进程句柄与下载进度，纯内存（与密钥池同一哲学）。

## 输入契约（管理 API — `api/handlers/local.rs`，仅本机回环）

| 端点 | 功能 |
|------|------|
| `GET /api/local/models` | 列出本地模型（含运行状态/健康/端口） |
| `POST /api/local/models` | 注册本地模型（GGUF 文件 + 引擎参数） |
| `POST /api/local/models/:id/edit` | 编辑参数（别名、上下文长度、GPU 层、后端、自启动、思考模式） |
| `DELETE /api/local/models/:id` | 停止并删除注册 |
| `POST /api/local/models/:id/start` / `/stop` | 引擎生命周期控制 |
| `GET /api/local/backends` | GPU 后端检测结果 |
| HF 浏览 | 搜索仓库 / 仓库详情（GGUF 文件列表）/ 触发下载 |

## 行为约束

- **引擎生命周期（PRD §10.2）**：
  - 启动后健康检查轮询直至就绪或超时报错；进程退出后按「崩溃重启」策略拉起。
  - 「自启动」开关：应用启动（含 `--minimized`）时自动拉起标记为自启动的模型。
  - 停止模型必须回收子进程与端口占用。
- **下载**：HF 下载支持进度上报（前端 `stores/localModels.ts` 轮询/订阅状态）；
  下载中断不得留下被当作完整文件使用的半成品。
- **导入本地权重**：`POST /api/local/models` 携带 `local_path` 时跳过下载——校验文件存在且为
  `.gguf` → 拷贝到应用数据目录（models/{id}/model.gguf）→ 直接 `status=downloaded`；
  拷贝失败不产生半成品记录。前端入口为私有智能页「添加权重」按钮弹出的来源选择
  （导入本地权重 / 浏览 HuggingFace），文件选择走 Tauri dialog 插件（仅桌面端可用）。
- **端口**：默认自动分配，冲突时顺延；编辑后重启生效。
- **思考模式**：`local_models.thinking`（V21，0/1，默认 1）持久化用户开关；引擎启动时总是
  显式映射为 `llama-server --reasoning on|off`（不走 auto 第三态，保证开关状态与引擎一致）。
  新建模型默认开启；引擎运行中修改需下次启动生效（同 ctx_size / n_gpu_layers）。
- **路由优先级**：组合 > 私有智能 > 云智能。`resolve_route_candidates()` 按 `tier='local'` 优先排序，
  同名模型本地引擎优先于云端 Provider。组合别名解析（`resolve_combo()`）先展开成员，成员内部同样遵循此优先级。
- **管理面隔离**：`/api/providers` 不返回 `local-*` provider（云智能页面不显示本地引擎），但
  `/api/models` 和 `/api/keys` 仍返回全部记录（组合成员选择器和密钥白名单需要看到私有模型）。
  本地模型创建/编辑时通过 `alias_taken()` 检查名称冲突（同时检查 models 表和 combos 表）。
- **删除语义**：删除注册默认保留已下载的 GGUF 文件（磁盘清理由用户显式操作）。

## 输出契约

- 引擎未就绪时请求该模型的代理路径 → 与普通上游不可达同语义（触发 failover/错误返回）。
- `/v1/models` 中本地模型与云端模型统一呈现（见 module-proxy-handler）。

## 错误处理

| 情况 | 行为 |
|------|------|
| 引擎二进制缺失/下载失败 | 明确错误信息，不静默降级 |
| 端口占用、启动超时 | 记录日志并返回可展示的错误 |
| HF API 不可达 | 浏览/下载失败提示，不影响已注册模型运行 |

## 实现位置

- Rust：`src-tauri/src/local/{mod,engine,backend,hf}.rs` + `api/handlers/local.rs` + `db/local_models.rs`
- 前端：`src/views/LocalModelsView.tsx`、`src/views/HfBrowseView.tsx`、`src/stores/localModels.ts`
- 表结构：见 module-database（`local_models`，V20）

## 测试要求

- 注册 → 启动 → 健康检查 → 代理请求全链路冒烟。
- 崩溃重启、自启动恢复、停止回收的进程级验证。

## 完成标准

- [x] HF 浏览/搜索/下载，进度可见
- [x] 引擎自动下载、启动/停止/崩溃重启/自启动
- [x] GPU 后端自动检测并可手动覆盖
- [x] 注册后以普通 Provider 语义参与网关路由与统计
