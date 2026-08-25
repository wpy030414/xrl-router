//! 插件 WebSocket（注册/心跳/keys/config 消息循环）+ REST 管理接口。

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Json, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;
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
            if msg.get("type").and_then(|t| t.as_str()) != Some("register") {
                warn!("Plugin WS: first message must be 'register'");
                return;
            }
            let reg_msg: PluginRegisterMsg = match serde_json::from_value(msg.clone()) {
                Ok(m) => m,
                Err(e) => {
                    warn!("Plugin WS: invalid register message from {}: {}", client_addr, e);
                    return;
                }
            };

            // 空 plugin_id 视为无效注册，拒绝但不弹窗（截图里「发现插件：」冒号后为空即此情况）
            if reg_msg.plugin_id.trim().is_empty() {
                warn!("Plugin WS: rejected register with empty plugin_id from {}", client_addr);
                let _ = socket.send(Message::Text(
                    serde_json::json!({"type": "error", "reason": "empty plugin_id"}).to_string().into()
                )).await;
                return;
            }

            let keys: Vec<String> = serde_json::from_value(
                msg.get("keys").cloned().unwrap_or(serde_json::json!([]))
            ).unwrap_or_default();

            match state.plugins.register(reg_msg.clone(), keys, &state.master_key, &state.keys) {
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
                    "keys_update" => {
                        let keys: Vec<String> = serde_json::from_value(
                            msg.get("keys").cloned().unwrap_or(serde_json::json!([]))
                        ).unwrap_or_default();
                        match state.plugins.handle_keys_update(
                            &plugin_id, keys.clone(), &state.master_key, &state.keys
                        ) {
                            Ok(added) => {
                                let _ = socket.send(Message::Text(
                                    serde_json::json!({
                                        "type": "keys_ack",
                                        "count": keys.len(),
                                        "added": added
                                    }).to_string().into()
                                )).await;
                                info!("Plugin WS: keys_update for {}, added={}", plugin_id, added);
                            }
                            Err(e) => {
                                warn!("Plugin WS: keys_update failed: {}", e);
                            }
                        }
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
        "SELECT id, provider_id, status, last_heartbeat_at FROM plugins"
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

/// GET /api/plugins/:id — 返回插件完整预填数据（provider + models + key_count），
/// 供前端 ProviderNewView 以插件模式渲染表单。
pub(crate) async fn get_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // 1. 查 plugins 表拿 provider_id + 状态（conn 锁在块内释放，Mutex 不可重入）
    let (provider_id, status) = {
        let conn = state.database.conn();
        let plugin_row = conn.query_row(
            "SELECT provider_id, status FROM plugins WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
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

    // 4. 查密钥数量
    let key_count: i64 = {
        let conn = state.database.conn();
        conn.query_row(
            "SELECT COUNT(*) FROM api_keys WHERE provider_id = ?1",
            rusqlite::params![&provider.id],
            |row| row.get(0),
        ).unwrap_or(0)
    };

    // 5. 插件是否在线
    let connected = state.plugins.is_connected(&id);

    Ok(Json(serde_json::json!({
        "plugin_id": id,
        "status": status,
        "connected": connected,
        "provider": {
            "id": provider.id,
            "name": provider.name,
            "kind": provider.kind.to_string(),
            "base_url": provider.base_url,
            "api_path": provider.api_path,
        },
        "models": models,
        "key_count": key_count,
    })))
}
