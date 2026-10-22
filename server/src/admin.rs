//! The owner's side, for the app: projects, members, share links, versions, every comment with
//! its anchors. Authenticated with the admin token, never a cookie.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{AppError, Result};
use crate::events::record;
use crate::review::comment_json;
use crate::security::ClientIp;
use crate::util::{self, new_id, normalize_email, now, random_token, same_secret, sha256_hex, valid_email};

const ACTOR: &str = "owner";

fn require_admin(st: &AppState, headers: &HeaderMap) -> Result<()> {
    let given = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim).unwrap_or("");
    if given.is_empty() || !same_secret(given, &st.admin_token) {
        return Err(AppError::Unauthorized(json!({"error": "the admin token"})));
    }
    Ok(())
}

fn project_json(c: &Connection, id: &str) -> Result<Value> {
    c.query_row("SELECT id, name, client, allow_download, created FROM project WHERE id = ?1", [id], |r| {
        Ok(json!({"id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "client": r.get::<_, String>(2)?, "allowDownload": r.get::<_, i64>(3)? != 0, "created": r.get::<_, i64>(4)?}))
    })
    .optional()?
    .ok_or_else(|| AppError::NotFound("no such project".into()))
}

#[derive(Deserialize)]
pub struct ProjectIn {
    name: String,
    #[serde(default)]
    client: String,
}

pub async fn create_project(
    State(st): State<AppState>,
    headers: HeaderMap,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(p): Json<ProjectIn>,
) -> Result<(StatusCode, Json<Value>)> {
    require_admin(&st, &headers)?;
    let name = util::text(&p.name, 200, "name")?;
    if name.is_empty() {
        return Err(AppError::BadRequest("name: give the project a name".into()));
    }
    let client = util::text(&p.client, 200, "client")?;
    let id = new_id();
    let v = st.db.tx(|c| {
        c.execute("INSERT INTO project (id, name, client, created) VALUES (?1, ?2, ?3, ?4)", params![id, name, client, now()])?;
        record(c, &st.bus, &id, "project.created", ACTOR, &ip, json!({"name": name}))?;
        project_json(c, &id)
    })?;
    Ok((StatusCode::CREATED, Json(v)))
}

pub async fn list_projects(State(st): State<AppState>, headers: HeaderMap) -> Result<Json<Value>> {
    require_admin(&st, &headers)?;
    let v = st.db.with(|c| {
        let ids: Vec<String> =
            c.prepare("SELECT id FROM project ORDER BY created DESC")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        ids.iter().map(|id| project_json(c, id)).collect::<Result<Vec<_>>>()
    })?;
    Ok(Json(Value::Array(v)))
}

fn documents_json(c: &Connection, project: &str) -> Result<Vec<Value>> {
    let mut docs = c.prepare("SELECT id, app_document_id, title, created FROM document WHERE project_id = ?1 ORDER BY created")?;
    let docs: Vec<(String, String, String, i64)> =
        docs.query_map([project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (id, app_id, title, created) in docs {
        let mut vs = c.prepare("SELECT id, n, label, uploaded, page_count FROM version WHERE document_id = ?1 ORDER BY n")?;
        let versions: Vec<Value> = vs
            .query_map([&id], |r| Ok(json!({"id": r.get::<_, String>(0)?, "n": r.get::<_, i64>(1)?, "label": r.get::<_, String>(2)?, "uploaded": r.get::<_, i64>(3)?, "pages": r.get::<_, i64>(4)?})))?
            .collect::<rusqlite::Result<_>>()?;
        out.push(json!({"id": id, "appDocumentId": app_id, "title": title, "created": created, "versions": versions}));
    }
    Ok(out)
}

pub async fn project_detail(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Result<Json<Value>> {
    require_admin(&st, &headers)?;
    let v = st.db.with(|c| {
        let mut p = project_json(c, &id)?;
        let members: Vec<Value> = c
            .prepare("SELECT id, email, name, role, created FROM member WHERE project_id = ?1 ORDER BY created")?
            .query_map([&id], |r| Ok(json!({"id": r.get::<_, String>(0)?, "email": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?, "role": r.get::<_, String>(3)?, "created": r.get::<_, i64>(4)?})))?
            .collect::<rusqlite::Result<_>>()?;
        let links: Vec<Value> = c
            .prepare("SELECT id, mode, password_hash IS NOT NULL, expires, revoked, created FROM share_link WHERE project_id = ?1 ORDER BY created")?
            .query_map([&id], |r| {
                Ok(json!({"id": r.get::<_, String>(0)?, "mode": r.get::<_, String>(1)?, "password": r.get::<_, bool>(2)?, "expires": r.get::<_, Option<i64>>(3)?, "revoked": r.get::<_, Option<i64>>(4)?, "created": r.get::<_, i64>(5)?}))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let events: Vec<Value> = c
            .prepare("SELECT seq, kind, actor, ip, payload, at FROM event WHERE project_id = ?1 ORDER BY seq DESC LIMIT 200")?
            .query_map([&id], |r| {
                Ok(json!({"seq": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?, "actor": r.get::<_, String>(2)?, "ip": r.get::<_, String>(3)?,
                    "payload": serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(Value::Null), "at": r.get::<_, i64>(5)?}))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if let Some(o) = p.as_object_mut() {
            o.insert("documents".into(), Value::Array(documents_json(c, &id)?));
            o.insert("members".into(), Value::Array(members));
            o.insert("links".into(), Value::Array(links));
            o.insert("events".into(), Value::Array(events));
        }
        Ok(p)
    })?;
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPatch {
    name: Option<String>,
    client: Option<String>,
    allow_download: Option<bool>,
}

pub async fn patch_project(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(p): Json<ProjectPatch>,
) -> Result<Json<Value>> {
    require_admin(&st, &headers)?;
    let v = st.db.tx(|c| {
        project_json(c, &id)?;
        if let Some(name) = &p.name {
            let name = util::text(name, 200, "name")?;
            if name.is_empty() {
                return Err(AppError::BadRequest("name: give the project a name".into()));
            }
            c.execute("UPDATE project SET name = ?2 WHERE id = ?1", params![id, name])?;
        }
        if let Some(client) = &p.client {
            c.execute("UPDATE project SET client = ?2 WHERE id = ?1", params![id, util::text(client, 200, "client")?])?;
        }
        if let Some(d) = p.allow_download {
            c.execute("UPDATE project SET allow_download = ?2 WHERE id = ?1", params![id, i64::from(d)])?;
        }
        record(c, &st.bus, &id, "project.updated", ACTOR, &ip, json!({"name": p.name, "client": p.client, "allowDownload": p.allow_download}))?;
        project_json(c, &id)
    })?;
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct MemberIn {
    email: String,
    #[serde(default)]
    name: String,
    role: String,
}

pub async fn add_member(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(m): Json<MemberIn>,
) -> Result<(StatusCode, Json<Value>)> {
    require_admin(&st, &headers)?;
    let email = normalize_email(&m.email);
    if !valid_email(&email) {
        return Err(AppError::BadRequest("email: not an address".into()));
    }
    if !["owner", "reviewer", "viewer"].contains(&m.role.as_str()) {
        return Err(AppError::BadRequest("role: owner, reviewer or viewer".into()));
    }
    let name = util::text(&m.name, 100, "name")?;
    let id = new_id();
    let v = st.db.tx(|c| {
        project_json(c, &project)?;
        let taken: Option<String> =
            c.query_row("SELECT id FROM member WHERE project_id = ?1 AND email = ?2", params![project, email], |r| r.get(0)).optional()?;
        if taken.is_some() {
            return Err(AppError::Conflict(format!("{email} is already a member")));
        }
        c.execute(
            "INSERT INTO member (id, project_id, email, name, role, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, project, email, name, m.role, now()],
        )?;
        record(c, &st.bus, &project, "member.added", ACTOR, &ip, json!({"member": id, "email": email, "role": m.role}))?;
        Ok(json!({"id": id, "email": email, "name": name, "role": m.role}))
    })?;
    Ok((StatusCode::CREATED, Json(v)))
}

pub async fn remove_member(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((project, member)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> Result<StatusCode> {
    require_admin(&st, &headers)?;
    st.db.tx(|c| {
        let n = c.execute(
            "DELETE FROM member WHERE id = ?1 AND project_id = ?2 AND NOT EXISTS (SELECT 1 FROM comment WHERE author_id = ?1)",
            params![member, project],
        )?;
        if n == 0 {
            // Members who commented stay (their comments need an author); they just lose access.
            let n = c.execute("UPDATE member SET role = 'removed' WHERE id = ?1 AND project_id = ?2", params![member, project])?;
            if n == 0 {
                return Err(AppError::NotFound("no such member".into()));
            }
        }
        c.execute("DELETE FROM session WHERE member_id = ?1", [&member])?;
        record(c, &st.bus, &project, "member.removed", ACTOR, &ip, json!({"member": member}))?;
        Ok(())
    })?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkIn {
    mode: String,
    password: Option<String>,
    expires_days: Option<i64>,
}

pub async fn create_link(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(l): Json<LinkIn>,
) -> Result<(StatusCode, Json<Value>)> {
    require_admin(&st, &headers)?;
    if l.mode != "invite" && l.mode != "public" {
        return Err(AppError::BadRequest("mode: invite or public".into()));
    }
    let password_hash = match l.password.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) if p.chars().count() < 6 => return Err(AppError::BadRequest("password: at least 6 characters".into())),
        Some(p) => Some(crate::auth::hash_password(p)?),
        None => None,
    };
    let expires = match l.expires_days {
        Some(d) if (1..=3650).contains(&d) => Some(now() + d * 86_400),
        Some(_) => return Err(AppError::BadRequest("expiresDays: 1 to 3650".into())),
        None => None,
    };
    let (id, token) = (new_id(), random_token()?);
    st.db.tx(|c| {
        project_json(c, &project)?;
        c.execute(
            "INSERT INTO share_link (id, project_id, token_hash, mode, password_hash, expires, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, project, sha256_hex(&token), l.mode, password_hash, expires, now()],
        )?;
        record(c, &st.bus, &project, "link.created", ACTOR, &ip, json!({"link": id, "mode": l.mode, "expires": expires}))?;
        Ok(())
    })?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "token": token, "url": format!("{}/r/{token}", st.public_origin), "mode": l.mode, "expires": expires})),
    ))
}

pub async fn revoke_link(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> Result<StatusCode> {
    require_admin(&st, &headers)?;
    st.db.tx(|c| {
        let project: Option<String> = c.query_row("SELECT project_id FROM share_link WHERE id = ?1", [&id], |r| r.get(0)).optional()?;
        let project = project.ok_or_else(|| AppError::NotFound("no such link".into()))?;
        c.execute("UPDATE share_link SET revoked = ?2 WHERE id = ?1 AND revoked IS NULL", params![id, now()])?;
        record(c, &st.bus, &project, "link.revoked", ACTOR, &ip, json!({"link": id}))?;
        Ok(())
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// A bundle zip as a new version of its document. A document belongs to the project it was
/// first uploaded to.
pub async fn upload_version(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>)> {
    require_admin(&st, &headers)?;
    st.db.with(|c| project_json(c, &project))?;
    let version_id = new_id();
    let dir = st.data_dir.join("versions").join(&version_id);
    let bundle = match tokio::task::spawn_blocking({
        let (body, dir) = (body.clone(), dir.clone());
        move || crate::bundle::install(&body, &dir)
    })
    .await
    {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(AppError::Internal(format!("unpacking: {e}")));
        }
    };
    let result = st.db.tx(|c| {
        let bound: Option<(String, String)> = c.query_row("SELECT id, project_id FROM document WHERE app_document_id = ?1", [&bundle.app_document_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let document = match bound {
            Some((id, p)) if p == project => {
                c.execute("UPDATE document SET title = ?2 WHERE id = ?1", params![id, bundle.title])?;
                id
            }
            Some((_, p)) => {
                let name: String = c.query_row("SELECT name FROM project WHERE id = ?1", [&p], |r| r.get(0))?;
                return Err(AppError::Conflict(format!("this document belongs to the project “{name}”; detach it in the app (review.detach) to start a review of its own")));
            }
            None => {
                let id = new_id();
                c.execute("INSERT INTO document (id, project_id, app_document_id, title, created) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, project, bundle.app_document_id, bundle.title, now()])?;
                id
            }
        };
        let n: i64 = c.query_row("SELECT COALESCE(MAX(n), 0) + 1 FROM version WHERE document_id = ?1", [&document], |r| r.get(0))?;
        let uploaded = now();
        c.execute(
            "INSERT INTO version (id, document_id, n, label, uploaded, page_count, checksum) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![version_id, document, n, format!("Review v{n}"), uploaded, bundle.page_count, bundle.checksum],
        )?;
        record(c, &st.bus, &project, "version.uploaded", ACTOR, &ip, json!({"document": document, "version": version_id, "n": n, "pages": bundle.page_count}))?;
        Ok(json!({
            "document": {"id": document, "appDocumentId": bundle.app_document_id, "title": bundle.title},
            "version": {"id": version_id, "n": n, "label": format!("Review v{n}"), "uploaded": uploaded, "pages": bundle.page_count, "checksum": bundle.checksum}
        }))
    });
    match result {
        Ok(v) => Ok((StatusCode::CREATED, Json(v))),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// Every comment of the project with its anchors, for the app's `review.sync`.
pub async fn comments_json(State(st): State<AppState>, headers: HeaderMap, Path(project): Path<String>) -> Result<Json<Value>> {
    require_admin(&st, &headers)?;
    let v = st.db.with(|c| {
        let p = project_json(c, &project)?;
        let ids: Vec<String> = c
            .prepare("SELECT id FROM comment WHERE project_id = ?1 ORDER BY created")?
            .query_map([&project], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let comments = ids.iter().map(|id| comment_json(c, id)).collect::<Result<Vec<_>>>()?;
        Ok(json!({"project": p, "documents": documents_json(c, &project)?, "comments": comments}))
    })?;
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct StatusIn {
    status: String,
}

/// The app marks what it did with a comment: `applied`, `orphaned`, or back to `open`/`resolved`.
pub async fn patch_comment(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(s): Json<StatusIn>,
) -> Result<Json<Value>> {
    require_admin(&st, &headers)?;
    if !["open", "resolved", "applied", "orphaned"].contains(&s.status.as_str()) {
        return Err(AppError::BadRequest("status: open, resolved, applied or orphaned".into()));
    }
    let v = st.db.tx(|c| {
        let project: Option<String> = c.query_row("SELECT project_id FROM comment WHERE id = ?1", [&id], |r| r.get(0)).optional()?;
        let project = project.ok_or_else(|| AppError::NotFound("no such comment".into()))?;
        c.execute("UPDATE comment SET status = ?2, modified = ?3 WHERE id = ?1", params![id, s.status, now()])?;
        record(c, &st.bus, &project, "comment.updated", ACTOR, &ip, json!({"comment": id, "status": s.status}))?;
        comment_json(c, &id)
    })?;
    Ok(Json(v))
}
