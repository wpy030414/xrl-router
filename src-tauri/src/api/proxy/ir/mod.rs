//! IR（中间表示）模块 — 三种 LLM 协议的统一抽象层。
//!
//! 子模块按转换方向组织：
//! - `from_messages` / `from_chat_completions` / `from_responses`：上游格式 → IR
//! - `to_messages` / `to_chat_completions` / `to_responses`：IR → 客户端格式
//! - `usage`：三种格式的 token usage 提取
//!
//! 所有内部工具只操作 IR 类型，与具体协议格式解耦。

pub mod types;

pub mod from_messages;
pub mod from_chat_completions;
pub mod from_responses;
pub mod to_messages;
pub mod to_chat_completions;
pub mod to_responses;
pub mod usage;

// SDK 合规验证已移至 crate 根 src/sdk_test/（见 lib.rs 的 #[cfg(test)] 挂载）
