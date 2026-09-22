//! 插件 WebSocket（注册/心跳/config 消息循环）+ REST 管理接口。
//!
//! V24 契约：Router 不再为插件管理密钥。register 携带 `keys` 字段或收到
//! `keys_update` 消息一律**严格拒绝**（回 `keys_not_supported` 错误并断开），
//! 强制插件方按新契约升级。

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Json, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error};

use crate::gateway::server::AppState;
use crate::plugin::PluginRegisterMsg;

pub(crate) async fn plugin_ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(client_addr): ConnectInfo<std::net::SocketAddr>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    info!("Plugin WS: incoming connection from {}", client_addr);
    ws.on_upgrade(move |socket| handle_plugin_ws(socket, state, client_addr))
}

/// register 首条消息的校验拒绝原因（供回错误消息与单测断言）。
#[derive(Debug, PartialEq)]
pub(crate) enum RegisterReject {
    /// 携带已废除的 `keys` 字段（无论空数组、null 还是有值——字段存在即拒绝）
    KeysNotSupported,
    NotRegister,
    EmptyPluginId,
    BadJson,
}

/// 解析并校验 register 首条消息（纯函数，便于内联单测）。
/// 注意：serde 默认忽略未知字段，`keys` 的存在性必须在原始 `Value` 上判定。
pub(crate) fn parse_register_msg(v: &serde_json::Value) -> Result<PluginRegisterMsg, RegisterReject> {
    if v.get("type").and_then(|t| t.as_str()) != Some("register") {
        return Err(RegisterReject::NotRegister);
    }
    // 严格口径：`keys` 字段存在即拒绝，强制插件方立即升级到无密钥契约
    if v.get("keys").is_some() {
        return Err(RegisterReject::KeysNotSupported);
    }
    let msg: PluginRegisterMsg = serde_json::from_value(v.clone()).map_err(|_| RegisterReject::BadJson)?;
    if msg.plugin_id.trim().is_empty() {
        return Err(RegisterReject::EmptyPluginId);
    }
    Ok(msg)
}

async fn handle_plugin_ws(mut socket: WebSocket, state: Arc<AppState>, client_addr: std::net::SocketAddr) {
    // First message must be "register"
    let plugin_id = match socket.recv().await {
        Some(Ok(Message::Text(text))) => {
            let msg: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Plugin WS: invalid first message: {}", e);
                    return;
                }
            };
            let reg_msg = match parse_register_msg(&msg) {
                Ok(m) => m,
                Err(reject) => {
                    let reason = match reject {
                        RegisterReject::KeysNotSupported => {
                            warn!("Plugin WS: rejected register with legacy 'keys' field from {}", client_addr);
                            "keys_not_supported"
                        }
                        RegisterReject::NotRegister => {
                            warn!("Plugin WS: first message must be 'register'");
                            "expected_register"
                        }
                        RegisterReject::EmptyPluginId => {
                            warn!("Plugin WS: rejected register with empty plugin_id from {}", client_addr);
                            "empty_plugin_id"
                        }
                        RegisterReject::BadJson => {
                            warn!("Plugin WS: invalid register message from {}", client_addr);
                            "invalid_register"
                        }
                    };
                    let _ = socket.send(Message::Text(
                        serde_json::json!({"type": "error", "reason": reason}).to_string().into()
                    )).await;
                    return;
                }
            };

            match state.plugins.register(reg_msg.clone()) {
                Ok((provider_id, needs_confirmation)) => {
                    let resp = if needs_confirmation {
                        serde_json::json!({
                            "type": "registered",
                            "provider_id": provider_id,
                            "status": "pending_confirmation"
                        })
                    } else {
                        serde_json::json!({
                            "type": "reconnected",
                            "provider_id": provider_id
                        })
                    };
                    let _ = socket.send(Message::Text(resp.to_string().into())).await;
                    info!("Plugin WS: registered, provider={}", provider_id);
                    // 注意：循环里的 plugin_id 必须是插件名（plugins 表主键），
                    // 不能是 provider_id（UUID）——否则 is_registered() 永远查不到，
                    // 会把每次心跳误判为「插件已被删除」而踢掉连接。
                    reg_msg.plugin_id
                }
                Err(e) => {
                    error!("Plugin WS: register failed: {}", e);
                    return;
                }
            }
        }
        _ => {
            warn!("Plugin WS: expected text register message");
            return;
        }
    };

    // Message loop
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                let msg: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");

                // 插件已被用户删除（如忽略对话框）→ 关闭连接，让插件重连后重新注册、重新弹窗
                if !state.plugins.is_registered(&plugin_id) {
                    info!("Plugin {} was deleted by user, closing connection for re-registration", plugin_id);
                    let _ = socket.send(Message::Text(
                        serde_json::json!({"type": "deleted", "reason": "plugin_ignored"}).to_string().into()
                    )).await;
                    break;
                }

                match msg_type {
                    "heartbeat" => {
                        state.plugins.heartbeat(&plugin_id);
                    }
                    // V24：密钥同步已废除，收到即回错并断开（强制插件方升级）
                    "keys_update" => {
                        warn!("Plugin WS: rejected keys_update from {}, closing connection", plugin_id);
                        let _ = socket.send(Message::Text(
                            serde_json::json!({"type": "error", "reason": "keys_not_supported"}).to_string().into()
                        )).await;
                        break;
                    }
                    "config_update" => {
                        let base_url = msg.get("base_url").and_then(|v| v.as_str()).map(String::from);
                        let api_path = msg.get("api_path").and_then(|v| v.as_str()).map(String::from);
                        state.plugins.handle_config_update(&plugin_id, base_url, api_path);
                        info!("Plugin WS: config_update for {}", plugin_id);
                    }
                    _ => {
                        // Unknown message type, ignore
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => {
                break;
            }
            _ => {}
        }
    }

    // Plugin disconnected
    state.plugins.disconnect(&plugin_id);
    info!("Plugin WS: {} disconnected", plugin_id);
}

#[derive(Serialize)]
struct PluginListItem {
    id: String,
    provider_id: Option<String>,
    status: String,
    last_heartbeat_at: Option<i64>,
    connected: bool,
    work_dir: Option<String>,
    autostart: bool,
}

pub(crate) async fn list_plugins(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let connected = state.plugins.list_connected();
    let connected_ids: std::collections::HashSet<String> = connected.iter()
        .map(|c| c.plugin_id.clone())
        .collect();

    // Get all plugins from DB
    let conn = state.database.conn();
    let mut stmt = match conn.prepare(
        "SELECT id, provider_id, status, last_heartbeat_at, work_dir, autostart FROM plugins"
    ) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response();
        }
    };
    let plugins: Vec<PluginListItem> = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        Ok(PluginListItem {
            connected: connected_ids.contains(&id),
            id,
            provider_id: row.get(1)?,
            status: row.get(2)?,
            last_heartbeat_at: row.get(3)?,
            work_dir: row.get(4)?,
            autostart: row.get::<_, i64>(5)? != 0,
        })
    }).unwrap().filter_map(|r| r.ok()).collect();

    Json(plugins).into_response()
}

pub(crate) async fn confirm_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    state.plugins.confirm(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()})))
    })?;
    Ok(Json(serde_json::json!({"status": "confirmed", "plugin_id": id})))
}

pub(crate) async fn delete_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // 先回收 Router 托管的伴生进程（若有），再断连、删记录
    state.plugin_host.kill(&id);

    // Disconnect if connected
    if state.plugins.is_connected(&id) {
        state.plugins.disconnect(&id);
    }

    // Get provider_id + delete plugin record（conn 锁在块内释放，Mutex 不可重入）
    let provider_id: Option<String> = {
        let conn = state.database.conn();
        let pid: Option<String> = conn.query_row(
            "SELECT provider_id FROM plugins WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        ).ok().flatten();

        conn.execute("DELETE FROM plugins WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))?;

        pid
    }; // conn 锁在此释放

    // Delete associated provider (cascades to keys + models)
    if let Some(pid) = provider_id {
        let _ = state.database.delete_provider(&pid);
        // 同步内存 registry + KeyPool
        state.providers.remove(&pid);
        state.keys.remove_provider(&pid);
    }

    Ok(Json(serde_json::json!({"status": "deleted", "plugin_id": id})))
}

/// GET /api/plugins/:id — 返回插件完整预填数据（provider + models + 伴生启动状态），
/// 供前端 ProviderNewView 以插件模式渲染表单。
pub(crate) async fn get_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // 1. 查 plugins 表拿 provider_id + 状态 + work_dir/autostart（conn 锁在块内释放，Mutex 不可重入）
    let (provider_id, status, work_dir, autostart) = {
        let conn = state.database.conn();
        let plugin_row = conn.query_row(
            "SELECT provider_id, status, work_dir, autostart FROM plugins WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)? != 0,
            )),
        );
        match plugin_row {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err((StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Plugin not found"}))));
            }
            Err(e) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))));
            }
        }
    }; // conn 锁在此释放

    // 2. 查关联 provider
    let provider = match provider_id.as_deref().and_then(|pid| state.providers.get(pid)) {
        Some(p) => p,
        None => {
            return Err((StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Provider not found"}))));
        }
    };

    // 3. 查该 provider 的模型
    let models: Vec<serde_json::Value> = state.database.list_all_models()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.provider_id == provider.id)
        .map(|m| serde_json::json!({
            "model_id": m.model_id,
            "display_name": m.display_name,
            "tier": m.tier,
        }))
        .collect();

    // 4. 插件是否在线
    let connected = state.plugins.is_connected(&id);

    Ok(Json(serde_json::json!({
        "plugin_id": id,
        "status": status,
        "connected": connected,
        "work_dir": work_dir,
        "autostart": autostart,
        "provider": {
            "id": provider.id,
            "name": provider.name,
            "kind": provider.kind.to_string(),
            "base_url": provider.base_url,
            "api_path": provider.api_path,
        },
        "models": models,
    })))
}

/// GET /api/plugins/runtime — 系统内 node / pnpm 可用性（伴生启动前置条件）。
/// 值为版本串（如 "v20.11.1"），null 表示不可用。
pub(crate) async fn get_plugin_runtime(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let rt = state.plugin_host.check_runtime(false).await;
    Json(serde_json::json!({ "node": rt.node, "pnpm": rt.pnpm }))
}

#[derive(Deserialize, Default)]
pub struct UpdatePluginBody {
    autostart: Option<bool>,
    work_dir: Option<String>,
}

/// PATCH /api/plugins/:id — 更新伴生启动开关 / 项目目录。
pub(crate) async fn update_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdatePluginBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // 确认插件存在
    if state.plugins.get_plugin_record(&id).is_err() {
        return Err((StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Plugin not found"}))));
    }

    if let Some(dir) = body.work_dir.as_deref() {
        let trimmed = dir.trim();
        if !trimmed.is_empty() && !std::path::Path::new(trimmed).is_dir() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "work_dir_not_found", "detail": trimmed})),
            ));
        }
        let v = if trimmed.is_empty() { None } else { Some(trimmed) };
        state.plugins.set_plugin_workdir(&id, v).map_err(internal_err)?;
    }
    if let Some(on) = body.autostart {
        state.plugins.set_plugin_autostart(&id, on).map_err(internal_err)?;
    }

    let record = state.plugins.get_plugin_record(&id).map_err(internal_err)?;
    Ok(Json(serde_json::json!({
        "plugin_id": id,
        "autostart": record.autostart,
        "work_dir": record.work_dir,
    })))
}

/// POST /api/plugins/:id/login — 在插件项目目录运行 `pnpm run login`，
/// login 脚本契约要求自行弹出系统浏览器完成登录（不要求客户端到位）。
pub(crate) async fn login_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let record = state.plugins.get_plugin_record(&id).map_err(internal_err)?;
    let work_dir = record
        .work_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let work_dir = match work_dir {
        Some(d) => d,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "work_dir_missing"})),
            ));
        }
    };

    let rt = state.plugin_host.check_runtime(false).await;
    if rt.node.is_none() || rt.pnpm.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "runtime_unavailable",
                "node": rt.node,
                "pnpm": rt.pnpm,
            })),
        ));
    }

    state.plugin_host.spawn_login(&id, &work_dir).map_err(internal_err)?;
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({"status": "launched"}))))
}

fn internal_err(e: anyhow::Error) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()})))
}
