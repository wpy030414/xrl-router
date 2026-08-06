//! Admin IP guard middleware — restricts endpoints to loopback (localhost) access only.

use axum::extract::connect_info::ConnectInfo;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;
use tracing::warn;

/// Middleware that rejects non-loopback clients with 403 Forbidden.
///
/// Applied to `/api/*` management endpoints so that only the local machine
/// (Tauri WebView, localhost CLI tools) can reach admin APIs.
/// Requires the server to use `into_make_service_with_connect_info::<SocketAddr>()`.
pub async fn admin_ip_guard(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !addr.ip().is_loopback() {
        warn!(
            path = %request.uri().path(),
            client_ip = %addr.ip(),
            "admin endpoint rejected non-loopback request"
        );
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn app_with_guard() -> Router {
        Router::new()
            .route("/api/test", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(admin_ip_guard))
    }

    #[tokio::test]
    async fn test_loopback_allowed() {
        use axum::extract::connect_info::MockConnectInfo;

        let app = app_with_guard().layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_non_loopback_forbidden() {
        use axum::extract::connect_info::MockConnectInfo;

        let app = app_with_guard().layer(MockConnectInfo(SocketAddr::from(([192, 168, 1, 50], 0))));

        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
