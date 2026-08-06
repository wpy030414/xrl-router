use crate::config::Config;
use crate::db::Database;
use crate::keys::KeyPool;
use crate::middleware::RateLimiter;
use crate::models::ModelRegistry;
use crate::plugin::PluginManager;
use crate::providers::ProviderRegistry;
use crate::api::handlers::FmEngine;
use anyhow::Result;
use axum::http::HeaderValue;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};

/// Shared application state accessible by all handlers.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub database: Database,
    pub providers: ProviderRegistry,
    pub keys: KeyPool,
    pub models: ModelRegistry,
    pub rate_limiter: RateLimiter,
    pub master_key: crate::crypto::MasterKey,
    pub key_stats_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    /// WebSearch 劫持开关（运行时可改、无锁读）。
    pub websearch_hijack: Arc<std::sync::atomic::AtomicBool>,
    /// 故障转移开关：同一模型多 provider 时，主 provider 失败自动切换下一个（运行时可改）。
    pub failover_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// provider 级冷却表：provider_id → 冷却到期 unix 秒（纯内存，设计选择同密钥健康）。
    pub provider_cooldowns: Arc<std::sync::RwLock<std::collections::HashMap<String, i64>>>,
    /// Plugin manager: tracks connected plugins and their delegated providers.
    pub plugins: PluginManager,
    /// 共享 HTTP 客户端（reqwest 内部有连接池 + TLS 缓存，clone 只复制 Arc）。
    pub http_client: reqwest::Client,
    /// FM 广播电台引擎（单例，进程级广播）。
    pub fm: FmEngine,
}

impl AppState {
    pub fn new(config: Config, database: Database, master_key: crate::crypto::MasterKey) -> Self {
        let providers = ProviderRegistry::new(database.clone());
        let _ = providers.load_from_db();
        let models = ModelRegistry::new(database.clone());
        let _ = models.load_from_db();

        let (key_stats_tx, _) = tokio::sync::broadcast::channel(64);

        let mut keys = KeyPool::new();
        keys.set_database(database.clone());
        keys.load_all_keys_from_db(&database, &master_key);
        keys.set_key_stats_tx(key_stats_tx.clone());

        let rate_limiter = RateLimiter::new();
        let websearch_hijack = Arc::new(std::sync::atomic::AtomicBool::new(
            database
                .get_setting("websearch_hijack")
                .ok()
                .flatten()
                .map(|v| v == "true")
                .unwrap_or(false),
        ));
        let failover_enabled = Arc::new(std::sync::atomic::AtomicBool::new(
            database
                .get_setting("failover_enabled")
                .ok()
                .flatten()
                .map(|v| v == "true")
                .unwrap_or(false),
        ));
        let provider_cooldowns = Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));

        let plugins = PluginManager::new(database.clone(), providers.providers_map());

        // 共享 HTTP 客户端（reqwest 内部有连接池 + TLS 缓存，clone 只复制 Arc）。
        // 复用后同一上游的后续请求无需重新 TCP+TLS 握手，减少首次响应延迟。
        let http_client = crate::http::build_http_client()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build shared http client");

        // FM 广播电台引擎：使用共享 http_client 复用连接池。
        let fm = FmEngine::new(http_client.clone());

        Self {
            config,
            database,
            providers,
            keys,
            models,
            rate_limiter,
            master_key,
            key_stats_tx,
            websearch_hijack,
            failover_enabled,
            provider_cooldowns,
            plugins,
            http_client,
            fm,
        }
    }
}

/// Start the gateway HTTP server as a background service.
///
/// 单 listener 绑 `0.0.0.0:port`，通过路径级 IP 中间件（`admin_ip_guard`）控制
/// 访问权限：`/api/*` 管理路径仅 loopback IP 可访问，其余路径对外开放。
/// `into_make_service_with_connect_info::<SocketAddr>()` 为中间件提供客户端 IP 提取。
pub async fn start_gateway(state: Arc<AppState>) -> Result<()> {
    let cors = build_cors_layer(&state.config);

    // 既有后台 task：每 5s 广播 usage_stats_changed（Key counts 已在 pool.rs 按变更广播）
    {
        let tx = state.key_stats_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                let _ = tx.send(serde_json::json!({
                    "type": "usage_stats_changed",
                    "timestamp": chrono::Utc::now().timestamp(),
                }));
            }
        });
    }

    // FM 广播电台引擎：后台持续拉取音源并广播给所有 /fm/live 订阅者。
    // app_handle 用于 emit fm-meta 事件通知前端切歌。
    // 注意：start_gateway 无法直接拿到 app_handle，由 lib.rs setup 中调用。

    // 既有后台 task：每 30s 检查插件心跳，断开 >90s 无心跳的插件
    {
        let plugins = state.plugins.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                plugins.check_heartbeats(90);
            }
        });
    }

    // 单 listener：0.0.0.0:port，承载全部路由（/api/* 由 admin_ip_guard 限制 loopback）
    let router = crate::api::build_router(state.clone()).layer(cors);
    let addr = state.config.addr();
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("HTTP server on http://{}", addr);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        {
            error!("HTTP server error: {}", e);
        }
    });

    Ok(())
}

/// Build a CORS layer constrained to configured local origins (tightens the
/// previous `allow_origin: *` policy). Falls back to permissive only if the
/// origin list is explicitly empty.
fn build_cors_layer(config: &Config) -> CorsLayer {
    let mut layer = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any);
    if config.cors_origins.is_empty() {
        layer = layer.allow_origin(Any);
    } else {
        let origins: Vec<HeaderValue> = config
            .cors_origins
            .iter()
            .filter_map(|o| o.parse::<HeaderValue>().ok())
            .collect();
        layer = layer.allow_origin(origins);
    }
    layer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// 端到端冒烟测试：真实 TCP 起网关，实测全部路径。
    /// 覆盖 build_router → handlers/* → proxy 认证 → AppState（DB 迁移、
    /// providers/models 注册表、密钥池、插件管理器）→ admin_ip_guard 的完整链路。
    /// 单 listener 绑定 127.0.0.1（loopback），/api/* 管理路径应正常放行。
    #[tokio::test]
    async fn test_gateway_smoke_end_to_end() {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        let config = Config {
            port: 0,
            host: "127.0.0.1".to_string(),
            ..Default::default()
        };
        let state = Arc::new(AppState::new(config, db, [7u8; 32]));
        let router = crate::api::build_router(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let client = reqwest::Client::new();
        // 等服务器就绪
        for _ in 0..50 {
            if client.get(format!("http://{}/health", addr)).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // /health：完整链路（DB 连接、providers/models 注册表、key pool）
        let resp = client.get(format!("http://{}/health", addr)).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "xrl-router");
        assert_eq!(body["database"], "ok");

        // /api/providers：CRUD handler 路径（loopback → admin_ip_guard 放行，空库返回空数组）
        let resp = client.get(format!("http://{}/api/providers", addr)).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let providers: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(providers.as_array().map(|a| a.len()), Some(0));

        // /v1/models：proxy 认证路径（无 service key 应 401）
        let resp = client.get(format!("http://{}/v1/models", addr)).send().await.unwrap();
        assert_eq!(resp.status(), 401);

        // /v1/chat/completions：proxy 认证 + 路由解析路径（无 service key 应 401）
        let resp = client
            .post(format!("http://{}/v1/chat/completions", addr))
            .header("Content-Type", "application/json")
            .body(r#"{"model":"test","messages":[{"role":"user","content":"hi"}]}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        // /v1/user/balance：无 service key 应 401
        let resp = client
            .get(format!("http://{}/v1/user/balance", addr))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        // 配额 429：创建 service key（quota_5h=10）→ 写入 15 tokens 用量 → 请求应 429。
        let raw_key = "xrl-test-quota-key";
        let key_hash = crate::crypto::hash_service_key(raw_key).unwrap();
        state.database.save_service_key("sk-quota", "限额测试", &key_hash, "****uota").unwrap();
        let now = chrono::Utc::now().timestamp();
        state.database.insert_usage_log(
            now,
            "p1", "P1", "m1", "M1",
            Some("pk1"), "PK", "pk-masked",
            Some("sk-quota"), "限额测试", "****uota",
            "/v1/messages",
            10, 5, 10, true, None, 0,
        ).unwrap();
        state.database.update_service_key("sk-quota", None, None, Some(10), None).unwrap();
        let resp = client
            .post(format!("http://{}/v1/chat/completions", addr))
            .header("Content-Type", "application/json")
            .header("x-api-key", raw_key)
            .body(r#"{"model":"test","messages":[{"role":"user","content":"hi"}]}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 429);
        // retry-after 应存在且为正值（在消费 body 之前读取）
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok());
        assert!(retry_after.is_some(), "429 应携带 retry-after 头");
        assert!(retry_after.unwrap().parse::<i64>().unwrap() > 0);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["type"], "quota_error");

        // /v1/user/balance：CCSwitch TokenPlan（ZenMux 分支）兼容格式
        // （5h 设限 → quota_5_hour；7d 未设限 → 字段省略）
        let resp = client
            .get(format!("http://{}/v1/user/balance", addr))
            .header("x-api-key", raw_key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let zm: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(zm["success"], true);
        assert_eq!(zm["data"]["quota_5_hour"]["usage_percentage"], 1.5);
        assert!(
            zm["data"]["quota_5_hour"]["resets_at"].as_str().unwrap().contains("T"),
            "resets_at 应为 ISO 字符串（CCSwitch 用 as_str 解析）"
        );
        assert!(zm["data"].get("quota_7_day").is_none(), "未设限窗口应省略");

        // /install：静态页（公开路径，单 listener 下与 /api/* 共存）
        let resp = client.get(format!("http://{}/install", addr)).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let html = resp.text().await.unwrap();
        assert!(html.contains("客户分发 / Client Deploy"), "/install 应返回 install 页面 HTML");

        // /api/stats/requests：请求日志分页（冒烟测试早前插过 usage 行）
        let resp = client
            .get(format!("http://{}/api/stats/requests?page=1&page_size=10", addr))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let logs: serde_json::Value = resp.json().await.unwrap();
        assert!(logs["total"].as_i64().unwrap_or(0) >= 1, "日志应至少有一条");
        assert!(logs["data"].as_array().map(|a| !a.is_empty()).unwrap_or(false));
        assert!(logs["data"][0].get("success").is_some(), "日志应带 success 字段");
        assert!(logs["data"][0].get("timestamp").is_some(), "日志应带 timestamp 字段");
    }

    /// 故障转移 E2E：主 provider 5xx → 自动切到备选 provider 成功；开关关闭 → 透传 5xx。
    /// 两个本地假上游（A 返回 500、B 返回 200 SSE），同 display_name 两个 provider 各一 key。
    #[tokio::test]
    async fn test_failover_switches_provider_on_5xx() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::response::sse::{Event, KeepAlive, Sse};
        use axum::routing::post;
        use futures::stream;
        use std::convert::Infallible;

        // 假上游 A：一律 500
        let router_a = axum::Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (StatusCode::INTERNAL_SERVER_ERROR, "boom from A").into_response()
            }),
        );
        let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_a = listener_a.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener_a, router_a).await.unwrap();
        });

        // 假上游 B：200 SSE 流
        let router_b = axum::Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                let s = stream::iter(vec![
                    Ok::<_, Infallible>(Event::default().data("hello from B")),
                    Ok::<_, Infallible>(Event::default().data("[DONE]")),
                ]);
                Sse::new(s).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(60)))
            }),
        );
        let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_b = listener_b.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener_b, router_b).await.unwrap();
        });

        // 网关：两个 provider（A sort 0 主、B sort 1 备）+ 同 display_name 模型 + 各一 key
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        let master_key = [7u8; 32];
        let save_provider = |db: &Database, id: &str, sort: i64, base: &str| {
            db.save_provider(&crate::types::Provider {
                id: id.to_string(),
                name: format!("P-{}", id),
                kind: crate::types::ProviderKind::Openai,
                base_url: base.to_string(),
                api_path: "/v1/chat/completions".to_string(),
                config: serde_json::json!({}),
                enabled: true,
                created_at: 1,
                updated_at: 1,
                sort_order: sort,
            })
            .unwrap();
        };
        save_provider(&db, "pa", 0, &format!("http://{}", addr_a));
        save_provider(&db, "pb", 1, &format!("http://{}", addr_b));
        for (pid, key) in [("pa", "sk-test-a"), ("pb", "sk-test-b")] {
            db.save_model(&crate::types::Model {
                id: format!("m-{}", pid),
                provider_id: pid.to_string(),
                model_id: format!("real-{}", key),
                display_name: "gpt-x".to_string(),
                tier: "custom".to_string(),
                context_window: 128000,
                max_output_tokens: 4096,
                capabilities: "[\"text\"]".to_string(),
                enabled: true,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
            db.save_api_key(&crate::types::ApiKey {
                id: format!("k-{}", pid),
                provider_id: pid.to_string(),
                name: format!("K-{}", pid),
                key_hash: crate::crypto::encrypt(key, &master_key).unwrap(),
                key_masked: format!("***{}", &key[key.len() - 1..]),
                key_plain: None,
                status: "green".to_string(),
                last_error: None,
                last_error_code: None,
                last_error_time: None,
                last_used_at: None,
                balance: None,
                balance_updated_at: None,
                total_requests: 0,
                total_tokens: 0,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        }
        // service key（分发密钥）——空 allowed_models = 允许全部
        let raw_service_key = "xrl-test-failover-key";
        let sk_hash = crate::crypto::hash_service_key(raw_service_key).unwrap();
        db.save_service_key("sk-failover", "故障转移测试", &sk_hash, "***key").unwrap();

        // 开关开：AppState 构造前写入 settings（AppState::new 读取）
        db.set_setting("failover_enabled", "true").unwrap();
        let config = Config {
            port: 0,
            host: "127.0.0.1".to_string(),
            ..Default::default()
        };
        let state = Arc::new(AppState::new(config, db.clone(), master_key));
        let router = crate::api::build_router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let client = reqwest::Client::new();
        for _ in 0..50 {
            if client.get(format!("http://{}/health", addr)).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let body = r#"{"model":"gpt-x","messages":[{"role":"user","content":"hi"}]}"#;
        let send = || {
            client
                .post(format!("http://{}/v1/chat/completions", addr))
                .header("x-api-key", raw_service_key)
                .header("Content-Type", "application/json")
                .body(body.to_string())
        };

        // 开关开：A 500 → 自动切 B → 200 且内容来自 B
        let resp = send().send().await.unwrap();
        assert_eq!(resp.status(), 200, "failover 开启时 5xx 应自动切换备选 provider");
        let text = resp.text().await.unwrap();
        assert!(text.contains("hello from B"), "响应应来自备选 provider B: {}", text);

        // 开关关：主 provider A 的 500 直接透传（同时清掉冷却表，避免残留干扰）
        db.set_setting("failover_enabled", "false").unwrap();
        state.failover_enabled.store(false, std::sync::atomic::Ordering::Relaxed);
        state.provider_cooldowns.write().unwrap().clear();
        let resp = send().send().await.unwrap();
        assert_eq!(resp.status(), 500, "failover 关闭时 5xx 应透传");
    }
}
