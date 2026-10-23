//! Response headers, the cross-site check on cookie-authenticated writes, the client IP and a
//! small rate limiter.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{HeaderValue, ORIGIN};
use axum::http::{HeaderName, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::AppError;
use crate::util;

const DEFAULT_CSP: &str = "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

pub async fn headers_mw(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    let csp = st.web.as_ref().map_or_else(|| HeaderValue::from_static(DEFAULT_CSP), |w| w.csp.clone());
    h.insert(HeaderName::from_static("content-security-policy"), csp);
    h.insert(HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff"));
    h.insert(HeaderName::from_static("referrer-policy"), HeaderValue::from_static("no-referrer"));
    h.insert(HeaderName::from_static("x-frame-options"), HeaderValue::from_static("DENY"));
    r
}

/// A write that carries the session cookie must come from our own origin: a page elsewhere can
/// make the browser send the cookie, but not forge the Origin header.
pub async fn origin_mw(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let writes = matches!(*req.method(), Method::POST | Method::PATCH | Method::PUT | Method::DELETE);
    if writes && util::cookie(req.headers(), crate::auth::COOKIE).is_some() {
        let origin = req.headers().get(ORIGIN).and_then(|v| v.to_str().ok());
        let site = req.headers().get("sec-fetch-site").and_then(|v| v.to_str().ok());
        if origin.is_some_and(|o| o != st.public_origin) || site == Some("cross-site") {
            return AppError::Forbidden("cross-site request".into()).into_response();
        }
    }
    next.run(req).await
}

/// The client's address, for the audit log and rate limits.
#[derive(Clone, Debug)]
pub struct ClientIp(pub String);

pub async fn ip_mw(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let forwarded = if st.trust_proxy {
        req.headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let ip = forwarded.or_else(|| req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string())).unwrap_or_default();
    req.extensions_mut().insert(ClientIp(ip));
    next.run(req).await
}

// ponytail: in memory, per instance; a shared store if there is ever a second instance.
#[derive(Default)]
pub struct RateLimiter(Mutex<HashMap<String, (u32, i64)>>);

impl RateLimiter {
    /// Whether `key` may act again: at most `limit` times per `window` seconds.
    pub fn allow(&self, key: &str, limit: u32, window: i64) -> bool {
        let mut m = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let now = util::now();
        if m.len() > 10_000 {
            m.retain(|_, (_, start)| now - *start < window);
        }
        let e = m.entry(key.to_string()).or_insert((0, now));
        if now - e.1 >= window {
            *e = (0, now);
        }
        e.0 = e.0.saturating_add(1);
        e.0 <= limit
    }
}

/// Checks a Turnstile token for an IP: true when the challenge passed.
pub type Verify = Arc<dyn Fn(String, String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Cloudflare Turnstile on the sign-in and join forms, when configured.
#[derive(Clone)]
pub struct Turnstile {
    pub site_key: String,
    pub verify: Verify,
}

impl Turnstile {
    pub fn cloudflare(site_key: &str, secret: &str) -> Self {
        let secret = secret.to_string();
        let verify: Verify = Arc::new(move |token: String, ip: String| {
            let secret = secret.clone();
            Box::pin(async move { tokio::task::spawn_blocking(move || siteverify(&secret, &token, &ip)).await.unwrap_or(false) })
        });
        Self { site_key: site_key.to_string(), verify }
    }
}

/// Ask Cloudflare whether `token` is good for `ip`.
fn siteverify(secret: &str, token: &str, ip: &str) -> bool {
    let sent = ureq::post("https://challenges.cloudflare.com/turnstile/v0/siteverify").send_form([
        ("secret", secret),
        ("response", token),
        ("remoteip", ip),
    ]);
    let body = match sent {
        Ok(mut r) => r.body_mut().read_to_string().unwrap_or_default(),
        Err(e) => {
            tracing::warn!("turnstile: {e}");
            return false;
        }
    };
    serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| v["success"].as_bool()).unwrap_or(false)
}
