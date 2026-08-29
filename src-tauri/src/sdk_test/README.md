# sdk_test — IR SDK 合规性验证

用官方 SDK（openai / anthropic）验证 IR 层转换的 3×3 通道（三种客户端格式 ×
三种上游格式）输入输出合规：

- **req**：IR → 三种客户端格式请求体（`httpx.MockTransport` 捕获 SDK 实际发出的 JSON）
- **stream**：IR 事件 → 三种客户端 SSE（喂给 SDK 流解析器消费，验证可被官方客户端解析）
- **parse**：三种上游 chunk 序列 → IR 事件（验证 IR 结构化事件与 usage 提取正确）

## 运行

```bash
# 1. 导出 fixture（Rust 侧真实 IR 代码，测试构建）
cargo test --lib sdk_fixtures

# 2. 用官方 SDK 校验（venv 已在 src-tauri/src/sdk-test/.venv）
src-tauri/src/sdk_test/.venv/bin/python src-tauri/src/sdk_test/ir_sdk_verify.py
```

## 工作原理

- `src-tauri/src/sdk_test/fixtures.rs`（仅 test 构建，lib.rs 挂载）直接调用真实
  `crate::api::proxy::ir::` 转换函数，把输入/输出导出为
  `src-tauri/target/ir_fixtures/*.json`——不含任何转换逻辑复制。
- `src-tauri/src/sdk_test/ir_sdk_verify.py` 用官方 SDK + `httpx.MockTransport`
  双向验证：请求体经 SDK 客户端路径发出并比对字段保真；SSE 帧经 SDK 流解析器
  消费并断言最终结构化结果（文本拼接、工具参数、usage）。
- SDK 依赖装在 `src-tauri/src/sdk_test/.venv`（不入库，见 .gitignore），
  用 `python3 -m venv .venv && .venv/bin/pip install openai anthropic httpx` 初始化。

> 语义参考：官方 SDK 的类型定义即规范事实（`openai/types/**`、`anthropic/types/**`）。
> 例：Responses 的 `function_call_output.output` 是 content parts 数组而非字符串；
> `response.completed` 携带完整 `output`；reasoning 事件名是
> `response.reasoning_summary_text.delta` 等。IR 层改动后跑本脚本即可回归。
