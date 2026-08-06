//! HTTP/WS API 层入口。
//!
//! 路由定义在 `router`，各 handler 按实体分布在 `handlers/*`，代理逻辑在
//! `proxy`。本文件仅做模块声明与 re-export，对外保持 `crate::api::build_router` 不变。

pub mod handlers;
pub mod proxy;
pub mod router;

pub use router::build_router;
