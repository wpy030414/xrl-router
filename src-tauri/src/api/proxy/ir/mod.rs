//! IR（中间表示）模块 — 三种 LLM 协议的统一抽象层。
//!
//! 子模块按转换方向组织：
//! - `from_anthropic` / `from_chat` / `from_responses`：上游格式 → IR
//! - `to_anthropic` / `to_chat` / `to_responses`：IR → 客户端格式
//! - `usage`：三种格式的 token usage 提取
//!
//! 所有内部工具只操作 IR 类型，与具体协议格式解耦。

pub mod types;

pub mod from_anthropic;
pub mod from_chat;
pub mod from_responses;
pub mod to_anthropic;
pub mod to_chat;
pub mod to_responses;
pub mod usage;

// Re-export 常用类型
pub use types::{
    IrContentBlock, IrContentBlockStart, IrContentDelta, IrError, IrImageSource, IrMessage,
    IrRequest, IrRole, IrStopReason, IrStreamEvent, IrSystemContent, IrThinkingConfig, IrTool,
    IrToolChoice, IrToolResultContent, IrUsage,
};
