//! 统计聚合 + 应用设置（mcp_websearch / mcp_webfetch / failover 开关）handler。

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::Deserialize;

use crate::gateway::server::AppState;

#[derive(Deserialize)]
pub(crate) struct StatsQuery {
    from: Option<i64>,
    to: Option<i64>,
    /// "hour" -> hourly buckets, anything else -> daily buckets.
    granularity: Option<String>,
    /// Local timezone offset in seconds (e.g. UTC+8 = 28800), so buckets align
    /// to local day/hour boundaries instead of UTC.
    tz_offset: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct LogQuery {
    page: Option<i64>,
    page_size: Option<i64>,
}

/// GET /api/stats/requests — 请求日志分页（时间逆序），每页默认 10 条。
pub(crate) async fn get_stats_requests(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LogQuery>,
) -> impl IntoResponse {
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(10).clamp(1, 100);
    let (total, data) = state
        .database
        .get_usage_log_page(page, page_size)
        .unwrap_or_else(|_| (0, Vec::new()));
    Json(serde_json::json!({
        "total": total,
        "page": page,
        "page_size": page_size,
        "data": data,
    }))
}

pub(crate) async fn get_stats(
    State(state): State<Arc<AppState>>,
    Query(params): Query<StatsQuery>,
) -> impl IntoResponse {
    let now = Utc::now().timestamp();
    // Defaults to the last 24h when the client omits the range.
    let from = params.from.unwrap_or(now - 86400);
    let to = params.to.unwrap_or(now);
    let bucket_seconds: i64 = match params.granularity.as_deref() {
        Some("hour") => 3600,
        _ => 86400,
    };
    let tz_offset = params.tz_offset.unwrap_or(0);
    let data = state
        .database
        .get_usage_by_day_and_key(from, to, bucket_seconds, tz_offset)
        .unwrap_or_default();
    let model_usage = state
        .database
        .get_usage_by_model(from, to)
        .unwrap_or_default();
    let top_model = model_usage.first().cloned();

    Json(serde_json::json!({ "data": data, "top_model": top_model }))
}

pub(crate) async fn get_settings(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let theme = state.database.get_setting("theme").ok().flatten().unwrap_or_else(|| "system".to_string());
    let hue = state.database.get_setting("hue").ok().flatten().unwrap_or_else(|| "264".to_string());
    let locale = state.database.get_setting("locale").ok().flatten().unwrap_or_else(|| "zh-CN".to_string());

    Json(serde_json::json!({
        "mcp_websearch": state.mcp_websearch.load(std::sync::atomic::Ordering::Relaxed),
        "mcp_webfetch": state.mcp_webfetch.load(std::sync::atomic::Ordering::Relaxed),
        "failover_enabled": state.failover_enabled.load(std::sync::atomic::Ordering::Relaxed),
        "theme": theme,
        "hue": hue.parse::<i32>().unwrap_or(264),
        "locale": locale,
    }))
}

#[derive(Deserialize)]
pub(crate) struct UpdateSettingsRequest {
    mcp_websearch: Option<bool>,
    mcp_webfetch: Option<bool>,
    failover_enabled: Option<bool>,
    theme: Option<String>,
    hue: Option<i32>,
    locale: Option<String>,
}

pub(crate) async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if let Some(v) = req.mcp_websearch {
        state.mcp_websearch.store(v, std::sync::atomic::Ordering::Relaxed);
        let val = if v { "true" } else { "false" };
        if let Err(e) = state.database.set_setting("mcp_websearch", val) {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ));
        }
    }
    if let Some(v) = req.mcp_webfetch {
        state.mcp_webfetch.store(v, std::sync::atomic::Ordering::Relaxed);
        let val = if v { "true" } else { "false" };
        if let Err(e) = state.database.set_setting("mcp_webfetch", val) {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ));
        }
    }
    if let Some(v) = req.failover_enabled {
        state.failover_enabled.store(v, std::sync::atomic::Ordering::Relaxed);
        let val = if v { "true" } else { "false" };
        if let Err(e) = state.database.set_setting("failover_enabled", val) {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ));
        }
    }
    if let Some(ref v) = req.theme {
        if let Err(e) = state.database.set_setting("theme", v) {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ));
        }
    }
    if let Some(v) = req.hue {
        if let Err(e) = state.database.set_setting("hue", &v.to_string()) {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ));
        }
    }
    if let Some(ref v) = req.locale {
        if let Err(e) = state.database.set_setting("locale", v) {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ));
        }
    }
    Ok(Json(serde_json::json!({"status": "ok"})))
}

/// GET /api/ui-settings — 公开端点，返回 UI 设置（主题/令牌色/语言）。
/// 供 LAN 浏览器 install 页面读取管理端配置，无需 loopback 限制。
pub(crate) async fn get_ui_settings(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let theme = state.database.get_setting("theme").ok().flatten().unwrap_or_else(|| "system".to_string());
    let hue = state.database.get_setting("hue").ok().flatten().unwrap_or_else(|| "264".to_string());
    let locale = state.database.get_setting("locale").ok().flatten().unwrap_or_else(|| "zh-CN".to_string());

    Json(serde_json::json!({
        "theme": theme,
        "hue": hue.parse::<i32>().unwrap_or(264),
        "locale": locale,
    }))
}
