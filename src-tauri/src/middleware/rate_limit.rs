use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// Token bucket rate limiter for service keys.
/// Default: 128 requests per minute per service key.
#[derive(Clone)]
pub struct RateLimiter {
    buckets: Arc<DashMap<String, TokenBucket>>,
    max_tokens: u32,
    refill_interval: Duration,
}

#[derive(Clone)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter with default settings (128 req/min).
    pub fn new() -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            max_tokens: 128,
            refill_interval: Duration::from_secs(60),
        }
    }

    /// Create a rate limiter with custom settings.
    pub fn with_limit(max_requests: u32, interval_secs: u64) -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            max_tokens: max_requests,
            refill_interval: Duration::from_secs(interval_secs),
        }
    }

    /// Check if a request is allowed for the given key.
    /// Returns true if allowed, false if rate limited.
    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let max = self.max_tokens as f64;

        let mut entry = self.buckets.entry(key.to_string()).or_insert(TokenBucket {
            tokens: max,
            last_refill: now,
        });

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(entry.last_refill);
        let refill_rate = max / self.refill_interval.as_secs_f64();
        let new_tokens = elapsed.as_secs_f64() * refill_rate;
        entry.tokens = (entry.tokens + new_tokens).min(max);
        entry.last_refill = now;

        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Get remaining tokens for a key.
    pub fn remaining(&self, key: &str) -> u32 {
        self.buckets
            .get(key)
            .map(|e| e.tokens.floor() as u32)
            .unwrap_or(self.max_tokens)
    }
}

/// Extract service key from request headers for rate limiting.
fn extract_service_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|s| s.to_string())
        })
}

/// Rate limiting middleware for /v1/* endpoints.
pub async fn rate_limit_middleware(
    State(limiter): State<RateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(key) = extract_service_key(request.headers()) {
        if !limiter.check(&key) {
            let retry_after = 60; // seconds until next token
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", retry_after.to_string())],
                axum::Json(json!({
                    "error": {
                        "type": "rate_limit_error",
                        "message": "Rate limit exceeded. Please retry after 60 seconds.",
                        "code": "rate_limited"
                    }
                })),
            )
                .into_response();
        }
    }

    next.run(request).await
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_rate_limit() {
        let limiter = RateLimiter::with_limit(3, 60);

        // First 3 requests should pass
        assert!(limiter.check("test-key"));
        assert!(limiter.check("test-key"));
        assert!(limiter.check("test-key"));

        // 4th request should be rate limited
        assert!(!limiter.check("test-key"));
    }

    #[test]
    fn test_different_keys_independent() {
        let limiter = RateLimiter::with_limit(1, 60);

        assert!(limiter.check("key-a"));
        assert!(!limiter.check("key-a")); // rate limited

        // Different key should still work
        assert!(limiter.check("key-b"));
    }

    #[test]
    fn test_remaining_tokens() {
        let limiter = RateLimiter::with_limit(5, 60);

        assert_eq!(limiter.remaining("test-key"), 5);
        limiter.check("test-key");
        assert_eq!(limiter.remaining("test-key"), 4);
    }
}
