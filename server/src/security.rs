//! Response headers, the cross-site check on cookie-authenticated writes, the client IP and a
//! small rate limiter.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, PoisonError};

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
