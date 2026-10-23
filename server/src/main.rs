//! DesignCraft Review: the service reviewers' browsers and the app talk to.
//!
//! One binary, one SQLite file, one folder of review bundles. Owners (the app, with the admin
//! token) create projects, invite members, make share links and upload versions; reviewers log in
//! by magic link (or as guests of a public link), read the bundle and comment; comments stream
//! back to everyone (SSE) and to the app (`comments.json`). Every change is an audit event.
#![forbid(unsafe_code)]

mod admin;
mod auth;
mod bundle;
mod db;
mod error;
mod events;
mod mail;
mod review;
mod rounds;
mod security;
#[cfg(test)]
mod tests;
mod util;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::header::HeaderValue;
use axum::middleware;
use axum::routing::{any, delete, get, patch, post};
use base64::prelude::*;
use sha2::Digest;
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::db::Db;
use crate::error::AppError;
use crate::events::Event;
use crate::mail::Mailer;
use crate::security::RateLimiter;

/// What every handler needs.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub data_dir: PathBuf,
    /// Scheme and host reviewers use, e.g. `https://review.example.com`; share links and the
    /// Origin check are built from it.
    pub public_origin: String,
    pub admin_token: String,
    pub mailer: Mailer,
    pub bus: broadcast::Sender<Event>,
    pub limiter: Arc<RateLimiter>,
    /// Behind Caddy (or another proxy that sets X-Forwarded-For).
    pub trust_proxy: bool,
    /// The built reviewer client, when there is one to serve.
    pub web: Option<Arc<WebAssets>>,
}

/// The reviewer client as Trunk built it (`dist/`), and the Content-Security-Policy its page
/// needs: the inline loader scripts are hashed into it, so no `unsafe-inline`.
pub struct WebAssets {
    pub dir: PathBuf,
    pub csp: HeaderValue,
}

impl AppState {
    pub fn new(data_dir: &Path, public_origin: String, admin_token: String, mailer: Mailer, trust_proxy: bool) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir.join("versions")).map_err(|e| format!("{}: {e}", data_dir.display()))?;
        let db = Db::open(&data_dir.join("db.sqlite")).map_err(|e| e.to_string())?;
        let (bus, _) = broadcast::channel(256);
        Ok(AppState {
            db,
            data_dir: data_dir.to_path_buf(),
            public_origin: public_origin.trim_end_matches('/').to_string(),
            admin_token,
            mailer,
            bus,
            limiter: Arc::new(RateLimiter::default()),
            trust_proxy,
            web: None,
        })
    }

    /// Serve the client built into `dir` (Trunk's `dist`) for every path that isn't the API.
    pub fn with_web_dir(mut self, dir: &Path) -> Result<Self, String> {
        let index = dir.join("index.html");
        let html = std::fs::read_to_string(&index).map_err(|e| format!("{}: {e}", index.display()))?;
        let hashes: String =
            inline_scripts(&html).iter().map(|s| format!(" 'sha256-{}'", BASE64_STANDARD.encode(sha2::Sha256::digest(s.as_bytes())))).collect();
        let csp = format!(
            "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'{hashes}; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; \
             frame-ancestors 'none'; base-uri 'self'; form-action 'self'"
        );
        let csp = HeaderValue::from_str(&csp).map_err(|e| format!("content security policy: {e}"))?;
        self.web = Some(Arc::new(WebAssets { dir: dir.to_path_buf(), csp }));
        Ok(self)
    }
}

/// The bodies of the page's inline `<script>` elements (those without `src`).
fn inline_scripts(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("<script") {
        let after = &rest[i..];
        let Some(gt) = after.find('>') else { break };
        let (tag, body) = (&after[..gt], &after[gt + 1..]);
        let Some(end) = body.find("</script>") else { break };
        if !tag.contains("src=") {
            out.push(body[..end].to_string());
        }
        rest = &body[end..];
    }
    out
}

async fn api_miss() -> AppError {
    AppError::NotFound("no such route".into())
}

pub fn router(st: AppState) -> Router {
    // Bundles are big; everything else is small.
    let upload = Router::new().route("/api/projects/{id}/versions", post(admin::upload_version)).layer(DefaultBodyLimit::max(64 << 20));
    let r = Router::new()
        .route("/", get(|| async { "DesignCraft Review" }))
        .route("/auth", get(auth::landing))
        .route("/auth.css", get(auth::landing_css))
        .route("/api/auth/request", post(auth::request))
        .route("/api/auth/consume", post(auth::consume))
        .route("/api/auth/guest", post(auth::guest))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/projects", get(admin::list_projects).post(admin::create_project))
        .route("/api/projects/{id}", get(admin::project_detail).patch(admin::patch_project))
        .route("/api/projects/{id}/members", post(admin::add_member))
        .route("/api/projects/{id}/members/{member}", delete(admin::remove_member))
        .route("/api/projects/{id}/share-links", post(admin::create_link))
        .route("/api/share-links/{id}", delete(admin::revoke_link))
        .route("/api/projects/{id}/comments.json", get(admin::comments_json))
        .route("/api/comments/{id}", patch(admin::patch_comment))
        .route("/api/comments/{id}/replies", post(admin::reply))
        .route("/api/documents/{app}", get(admin::document_binding))
        .route("/api/projects/{id}/rounds", get(admin::list_rounds).post(admin::create_round))
        .route("/api/rounds/{id}/lock", post(admin::lock_round))
        .route("/api/share-links/{id}/invite", post(admin::invite))
        .route("/api/r/{token}/decisions", post(review::decide))
        .route("/api/r/{token}", get(review::share_page))
        .route("/api/r/{token}/v/{version}/{*path}", get(review::file))
        .route("/api/r/{token}/comments", get(review::list_comments).post(review::create_comment))
        .route("/api/r/{token}/comments/{id}", patch(review::patch_comment))
        .route("/api/r/{token}/comments/{id}/replies", post(review::reply))
        .route("/api/r/{token}/events", get(review::events))
        .merge(upload)
        .route("/api/{*rest}", any(api_miss));
    let r = match &st.web {
        Some(w) => r.fallback_service(ServeDir::new(&w.dir).fallback(ServeFile::new(w.dir.join("index.html")))),
        None => r.fallback(api_miss),
    };
    r.layer(DefaultBodyLimit::max(2 << 20))
        .layer(middleware::from_fn_with_state(st.clone(), security::origin_mw))
        .layer(middleware::from_fn_with_state(st.clone(), security::ip_mw))
        .layer(middleware::from_fn_with_state(st.clone(), security::headers_mw))
        .layer(TraceLayer::new_for_http())
        .with_state(st)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let Some(admin_token) = env("ADMIN_TOKEN").filter(|t| t.len() >= 32) else {
        eprintln!("ADMIN_TOKEN must be set (at least 32 characters; `openssl rand -base64 32`)");
        std::process::exit(2);
    };
    let data_dir = PathBuf::from(env("DATA_DIR").unwrap_or_else(|| "data".into()));
    let web_dir = env("WEB_DIR").map(PathBuf::from);
    let public_origin = env("PUBLIC_ORIGIN").unwrap_or_else(|| "http://localhost:8080".into());
    let bind = env("BIND").unwrap_or_else(|| "127.0.0.1:8080".into());
    let trust_proxy = env("TRUST_PROXY").is_some_and(|v| v == "1" || v == "true");
    let mailer = match Mailer::from_env() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("mail: {e}");
            std::process::exit(2);
        }
    };
    let st = match AppState::new(&data_dir, public_origin.clone(), admin_token, mailer, trust_proxy).and_then(|s| match &web_dir {
        Some(d) => s.with_web_dir(d),
        None => Ok(s),
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // Reminders the day before a round is due, checked every half hour.
    let reminders = st.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
            match rounds::remind_once(&reminders, util::now()).await {
                Ok(n) if n > 0 => tracing::info!("{n} reminder mail(s) sent"),
                Ok(_) => {}
                Err(e) => tracing::warn!("reminders: {e}"),
            }
        }
    });
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{bind}: {e}");
            std::process::exit(2);
        }
    };
    tracing::info!(
        "listening on {bind}, public origin {public_origin}, data in {}, client {}",
        data_dir.display(),
        web_dir.as_ref().map_or("none".to_string(), |d| d.display().to_string())
    );
    let served = axum::serve(listener, router(st).into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown()).await;
    if let Err(e) = served {
        eprintln!("server: {e}");
        std::process::exit(1);
    }
}

async fn shutdown() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
