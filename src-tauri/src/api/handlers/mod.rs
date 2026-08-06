//! Axum handler 按实体分组。各 handler 为 `pub(crate)`，由 `router` 引用。

pub mod health;
pub mod install;
pub mod keys;
pub mod models;
pub mod plugin;
pub mod providers;
pub mod service_keys;
pub mod stats;
pub mod websocket;
pub mod data;
pub mod fm;

pub(crate) use health::health_check;
pub(crate) use install::{get_local_ip, serve_install_page};
pub(crate) use keys::{create_key, delete_key, get_key, list_keys, update_key};
pub(crate) use models::{create_model, delete_model, get_model, list_models, proxy_fetch_models, update_model};
pub(crate) use plugin::{confirm_plugin, delete_plugin, get_plugin, list_plugins, plugin_ws_handler};
pub(crate) use providers::{create_provider, delete_provider, get_provider, list_providers, reorder_providers, update_provider};
pub(crate) use service_keys::{create_service_key, delete_service_key, list_service_keys, update_service_key};
pub(crate) use stats::{get_settings, get_stats, get_stats_requests, update_settings};
pub(crate) use websocket::ws_handler;
pub(crate) use data::{export_data, import_data, reset_data};
pub(crate) use fm::{fm_live, fm_current_meta, FmEngine};
