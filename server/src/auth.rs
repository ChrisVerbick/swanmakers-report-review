//! Who a reviewer is: magic links for invited members, name + password for guests of public
//! links, sessions in a `__Host-` cookie.

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use axum::Json;
use axum::extract::{Extension, Form, Query, State};
use axum::http::header::{LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::error::{AppError, Result};
use crate::events::record;
use crate::security::ClientIp;
use crate::util::{self, html_escape, normalize_email, now, random_token, sha256_hex, valid_email};

pub const COOKIE: &str = "__Host-sid";
const SESSION_ABSOLUTE: i64 = 12 * 3600;
const SESSION_IDLE: i64 = 3600;
const LOGIN_TTL: i64 = 15 * 60;

/// A logged-in member of a project.
#[derive(Clone, Debug, Serialize)]
pub struct Me {
    pub id: String,
    #[serde(skip)]
    pub project_id: String,
    pub email: String,
    pub name: String,
    pub role: String,
}

/// A share link that is live (not revoked, not expired).
#[derive(Clone, Debug)]
pub struct Link {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub mode: String,
    pub password_hash: Option<String>,
    pub allow_download: bool,
}

/// The member the request's session cookie belongs to, if it has a live one.
pub fn session(st: &AppState, headers: &HeaderMap) -> Result<Option<Me>> {
    let Some(sid) = util::cookie(headers, COOKIE) else { return Ok(None) };
    let h = sha256_hex(&sid);
    let now = now();
    st.db.with(|c| {
        let row = c
            .query_row(
                "SELECT s.member_id, s.last_seen, s.expires, m.project_id, m.email, m.name, m.role FROM session s JOIN member m ON m.id = s.member_id WHERE s.id_hash = ?1",
                [&h],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?)),
            )
            .optional()?;
        let Some((id, last_seen, expires, project_id, email, name, role)) = row else { return Ok(None) };
        if expires < now || last_seen + SESSION_IDLE < now {
            c.execute("DELETE FROM session WHERE id_hash = ?1", [&h])?;
            return Ok(None);
        }
        c.execute("UPDATE session SET last_seen = ?2 WHERE id_hash = ?1", params![h, now])?;
        Ok(Some(Me { id, project_id, email, name, role }))
    })
}

pub fn link_by_token(st: &AppState, token: &str) -> Result<Link> {
    if token.is_empty() || token.len() > 64 {
        return Err(AppError::NotFound("no such review link".into()));
    }
    let h = sha256_hex(token);
    let now = now();
    st.db.with(|c| {
        c.query_row(
            "SELECT l.id, l.project_id, p.name, l.mode, l.password_hash, p.allow_download FROM share_link l JOIN project p ON p.id = l.project_id \
             WHERE l.token_hash = ?1 AND l.revoked IS NULL AND (l.expires IS NULL OR l.expires > ?2)",
            params![h, now],
            |r| {
                Ok(Link {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    project_name: r.get(2)?,
                    mode: r.get(3)?,
                    password_hash: r.get(4)?,
                    allow_download: r.get::<_, i64>(5)? != 0,
                })
            },
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound("no such review link".into()))
    })
}

fn cookie_header(sid: &str, max_age: i64) -> String {
    format!("{COOKIE}={sid}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={max_age}")
}

/// A new session for `member_id`: the Set-Cookie header value.
pub fn new_session(st: &AppState, member_id: &str) -> Result<String> {
    let sid = random_token()?;
    let now = now();
    st.db.with(|c| {
        c.execute(
            "INSERT INTO session (id_hash, member_id, created, last_seen, expires) VALUES (?1, ?2, ?3, ?3, ?4)",
            params![sha256_hex(&sid), member_id, now, now + SESSION_ABSOLUTE],
        )?;
        Ok(())
    })?;
    Ok(cookie_header(&sid, SESSION_ABSOLUTE))
}

pub fn hash_password(pw: &str) -> Result<String> {
    let salt = util::random_bytes::<16>()?;
    Ok(Argon2::default().hash_password_with_salt(pw.as_bytes(), &salt).map_err(|e| AppError::Internal(format!("hash: {e}")))?.to_string())
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).ok().is_some_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

#[derive(Deserialize)]
pub struct RequestIn {
    email: String,
    link: String,
}

/// Mail a magic link to a member of the link's project. A stranger gets the same answer, and no
/// mail.
pub async fn request(State(st): State<AppState>, Extension(ClientIp(ip)): Extension<ClientIp>, Json(b): Json<RequestIn>) -> Result<StatusCode> {
    let email = normalize_email(&b.email);
    if !st.limiter.allow(&format!("auth-ip:{ip}"), 20, 900) || !st.limiter.allow(&format!("auth-email:{email}"), 5, 900) {
        return Err(AppError::TooMany);
    }
    if !valid_email(&email) {
        return Err(AppError::BadRequest("email: not an address".into()));
    }
    let link = link_by_token(&st, &b.link)?;
    let member: Option<String> = st.db.with(|c| {
        Ok(c.query_row("SELECT name FROM member WHERE project_id = ?1 AND email = ?2", params![link.project_id, email], |r| r.get(0)).optional()?)
    })?;
    let Some(name) = member else {
        tracing::info!("login requested for an address that isn't a member");
        return Ok(StatusCode::OK);
    };
    let token = random_token()?;
    st.db.with(|c| {
        c.execute(
            "INSERT INTO login_token (token_hash, email, project_id, link_id, expires) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![sha256_hex(&token), email, link.project_id, link.id, now() + LOGIN_TTL],
        )?;
        record(c, &st.bus, &link.project_id, "login.requested", &email, &ip, json!({}))?;
        Ok(())
    })?;
    let url = format!("{}/auth?token={token}&to=/r/{}", st.public_origin, b.link);
    let greeting = if name.is_empty() { "Hello".to_string() } else { format!("Hello {name}") };
    let body = format!(
        "{greeting},\n\nOpen this link to review \"{}\":\n\n{url}\n\nIt works once and expires in 15 minutes. If you didn't ask for it, ignore this mail.\n",
        link.project_name
    );
    st.mailer.send(&email, &format!("Review: {}", link.project_name), &body).await?;
    Ok(StatusCode::OK)
}

/// Where to go after login: a review page on this site, never anywhere else.
fn safe_to(to: Option<&str>) -> String {
    match to {
        Some(t) if t.starts_with("/r/") && t.chars().all(|c| c.is_ascii_alphanumeric() || "/-_".contains(c)) => t.to_string(),
        _ => "/".to_string(),
    }
}

#[derive(Deserialize)]
pub struct LandingQuery {
    token: Option<String>,
    to: Option<String>,
}

/// The page a magic link opens: a button that posts the token, so a mail scanner that fetches
/// the link doesn't use it up.
pub async fn landing(Query(q): Query<LandingQuery>) -> Html<String> {
    let token = html_escape(q.token.as_deref().unwrap_or(""));
    let to = html_escape(&safe_to(q.to.as_deref()));
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Sign in</title><style>body{{font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem}}\
         button{{font:inherit;padding:.6rem 1.2rem}}</style></head><body><h1>Sign in to the review</h1>\
         <p>Press the button to continue. The link works once and expires after 15 minutes.</p>\
         <form method=\"post\" action=\"/api/auth/consume\"><input type=\"hidden\" name=\"token\" value=\"{token}\">\
         <input type=\"hidden\" name=\"to\" value=\"{to}\"><button type=\"submit\">Continue</button></form></body></html>"
    ))
}

#[derive(Deserialize)]
pub struct ConsumeIn {
    token: String,
    to: Option<String>,
}

/// Use a magic link once: a session cookie, then the review page.
pub async fn consume(State(st): State<AppState>, Extension(ClientIp(ip)): Extension<ClientIp>, Form(f): Form<ConsumeIn>) -> Result<Response> {
    if !st.limiter.allow(&format!("consume-ip:{ip}"), 30, 900) {
        return Err(AppError::TooMany);
    }
    let invalid = || AppError::Unauthorized(json!({"error": "this sign-in link is not valid any more; ask for a new one"}));
    let h = sha256_hex(&f.token);
    let now = now();
    let member_id = st.db.tx(|c| {
        let row = c
            .query_row("SELECT email, project_id, expires, used FROM login_token WHERE token_hash = ?1", [&h], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?))
            })
            .optional()?;
        let Some((email, project_id, expires, used)) = row else { return Err(invalid()) };
        if used.is_some() || expires < now {
            return Err(invalid());
        }
        c.execute("UPDATE login_token SET used = ?2 WHERE token_hash = ?1", params![h, now])?;
        let member: Option<String> =
            c.query_row("SELECT id FROM member WHERE project_id = ?1 AND email = ?2", params![project_id, email], |r| r.get(0)).optional()?;
        let member = member.ok_or_else(invalid)?;
        record(c, &st.bus, &project_id, "login.succeeded", &email, &ip, json!({"member": member}))?;
        Ok(member)
    })?;
    let cookie = new_session(&st, &member_id)?;
    Ok(([(SET_COOKIE, cookie), (LOCATION, safe_to(f.to.as_deref()))], StatusCode::SEE_OTHER).into_response())
}

#[derive(Deserialize)]
pub struct GuestIn {
    link: String,
    name: String,
    email: String,
    password: Option<String>,
}

/// Join a public link as a guest: a name and address to sign comments with, and the link's
/// password when it has one.
pub async fn guest(State(st): State<AppState>, Extension(ClientIp(ip)): Extension<ClientIp>, Json(g): Json<GuestIn>) -> Result<Response> {
    if !st.limiter.allow(&format!("guest-ip:{ip}"), 20, 900) {
        return Err(AppError::TooMany);
    }
    let link = link_by_token(&st, &g.link)?;
    if link.mode != "public" {
        return Err(AppError::Forbidden("this review is by invitation: ask for a sign-in link".into()));
    }
    if let Some(hash) = &link.password_hash
        && !g.password.as_deref().is_some_and(|p| verify_password(p, hash))
    {
        return Err(AppError::Unauthorized(json!({"error": "wrong password", "mode": "public"})));
    }
    let email = normalize_email(&g.email);
    if !valid_email(&email) {
        return Err(AppError::BadRequest("email: not an address".into()));
    }
    let name = util::text(&g.name, 100, "name")?;
    if name.is_empty() {
        return Err(AppError::BadRequest("name: say who you are".into()));
    }
    let member_id = st.db.tx(|c| {
        let existing: Option<String> =
            c.query_row("SELECT id FROM member WHERE project_id = ?1 AND email = ?2", params![link.project_id, email], |r| r.get(0)).optional()?;
        let id = match existing {
            Some(id) => {
                c.execute("UPDATE member SET name = ?2 WHERE id = ?1 AND name = ''", params![id, name])?;
                id
            }
            None => {
                let id = util::new_id();
                c.execute(
                    "INSERT INTO member (id, project_id, email, name, role, created) VALUES (?1, ?2, ?3, ?4, 'guest', ?5)",
                    params![id, link.project_id, email, name, now()],
                )?;
                id
            }
        };
        record(c, &st.bus, &link.project_id, "guest.joined", &email, &ip, json!({"member": id, "link": link.id}))?;
        Ok(id)
    })?;
    let cookie = new_session(&st, &member_id)?;
    Ok(([(SET_COOKIE, cookie)], Json(json!({"ok": true}))).into_response())
}

pub async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Result<Response> {
    if let Some(sid) = util::cookie(&headers, COOKIE) {
        st.db.with(|c| {
            c.execute("DELETE FROM session WHERE id_hash = ?1", [sha256_hex(&sid)])?;
            Ok(())
        })?;
    }
    Ok(([(SET_COOKIE, cookie_header("", 0))], StatusCode::NO_CONTENT).into_response())
}
