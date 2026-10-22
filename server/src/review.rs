//! The reviewer's side, under `/api/r/{token}`: the share page, the bundle's files, comments,
//! replies, the event stream. Everything is scoped to the link's project.

use std::collections::HashMap;
use std::convert::Infallible;

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::{Stream, StreamExt};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;

use crate::AppState;
use crate::auth::{Link, Me, link_by_token, session};
use crate::error::{AppError, Result};
use crate::events::{self, record};
use crate::security::ClientIp;
use crate::util::{self, new_id, now};

const KINDS: [&str; 5] = ["pin", "highlight", "strike", "replace", "note"];
const MAX_BODY: usize = 10_000;

struct Access {
    link: Link,
    me: Option<Me>,
}

/// Who is asking, and through which link. An invite-only link needs a session of its project.
fn access(st: &AppState, headers: &HeaderMap, token: &str) -> Result<Access> {
    let link = link_by_token(st, token)?;
    let me = session(st, headers)?.filter(|m| m.project_id == link.project_id && m.role != "removed");
    if link.mode == "invite" && me.is_none() {
        return Err(AppError::Unauthorized(json!({"error": "login", "mode": "invite", "project": link.project_name})));
    }
    Ok(Access { link, me })
}

fn commenter(a: &Access) -> Result<&Me> {
    match &a.me {
        Some(m) if m.role == "viewer" => Err(AppError::Forbidden("viewers can't comment".into())),
        Some(m) => Ok(m),
        None => Err(AppError::Unauthorized(json!({"error": "login", "mode": a.link.mode, "project": a.link.project_name}))),
    }
}

/// A comment with its author, as the API shows it.
pub fn comment_json(c: &Connection, id: &str) -> Result<Value> {
    c.query_row(
        "SELECT c.id, c.document_id, c.version_id, c.kind, c.page, c.story, c.cell, c.footnote, c.start, c.end, c.quote_exact, c.quote_prefix, c.quote_suffix, \
         c.rects, c.x, c.y, c.body, c.replacement, c.status, c.created, c.modified, c.in_reply_to, m.id, m.name, m.email, m.role \
         FROM comment c JOIN member m ON m.id = c.author_id WHERE c.id = ?1",
        [id],
        |r| {
            let json_col = |i: usize| -> rusqlite::Result<Value> { Ok(r.get::<_, Option<String>>(i)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)) };
            Ok(json!({
                "id": r.get::<_, String>(0)?, "document": r.get::<_, String>(1)?, "version": r.get::<_, String>(2)?,
                "kind": r.get::<_, String>(3)?, "page": r.get::<_, i64>(4)?,
                "story": r.get::<_, Option<i64>>(5)?, "cell": json_col(6)?, "footnote": r.get::<_, Option<i64>>(7)?,
                "start": r.get::<_, Option<i64>>(8)?, "end": r.get::<_, Option<i64>>(9)?,
                "quote": {"exact": r.get::<_, Option<String>>(10)?, "prefix": r.get::<_, Option<String>>(11)?, "suffix": r.get::<_, Option<String>>(12)?},
                "rects": json_col(13)?, "x": r.get::<_, Option<f64>>(14)?, "y": r.get::<_, Option<f64>>(15)?,
                "body": r.get::<_, String>(16)?, "replacement": r.get::<_, Option<String>>(17)?, "status": r.get::<_, String>(18)?,
                "created": r.get::<_, i64>(19)?, "modified": r.get::<_, i64>(20)?, "inReplyTo": r.get::<_, Option<String>>(21)?,
                "author": {"id": r.get::<_, String>(22)?, "name": r.get::<_, String>(23)?, "email": r.get::<_, String>(24)?, "role": r.get::<_, String>(25)?}
            }))
        },
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound("no such comment".into()))
}

pub async fn share_page(State(st): State<AppState>, Path(token): Path<String>, headers: HeaderMap) -> Result<Json<Value>> {
    let a = access(&st, &headers, &token)?;
    let v = st.db.with(|c| {
        let (name, client): (String, String) = c.query_row("SELECT name, client FROM project WHERE id = ?1", [&a.link.project_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut docs = c.prepare("SELECT id, title FROM document WHERE project_id = ?1 ORDER BY created")?;
        let docs: Vec<(String, String)> = docs.query_map([&a.link.project_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let mut documents = Vec::new();
        for (id, title) in docs {
            let versions: Vec<Value> = c
                .prepare("SELECT id, n, label, uploaded, page_count FROM version WHERE document_id = ?1 ORDER BY n")?
                .query_map([&id], |r| Ok(json!({"id": r.get::<_, String>(0)?, "n": r.get::<_, i64>(1)?, "label": r.get::<_, String>(2)?, "uploaded": r.get::<_, i64>(3)?, "pages": r.get::<_, i64>(4)?})))?
                .collect::<rusqlite::Result<_>>()?;
            if !versions.is_empty() {
                documents.push(json!({"id": id, "title": title, "versions": versions}));
            }
        }
        Ok(json!({"project": {"id": a.link.project_id, "name": name, "client": client}, "mode": a.link.mode, "allowDownload": a.link.allow_download, "password": a.link.password_hash.is_some(), "me": a.me, "documents": documents}))
    })?;
    Ok(Json(v))
}

/// The client's own id for a comment (a lower-case UUID), or a fresh one.
fn own_id(given: Option<&str>) -> Result<String> {
    match given {
        None => Ok(new_id()),
        Some(id)
            if id.len() == 36
                && id
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() && !b.is_ascii_uppercase() }) =>
        {
            Ok(id.to_string())
        }
        Some(_) => Err(AppError::BadRequest("id: a lower-case UUID".into())),
    }
}

/// The comment already stored under a client's id, if any; another project's id is refused.
fn existing(c: &Connection, id: &str, project: &str) -> Result<Option<Value>> {
    let owner: Option<String> = c.query_row("SELECT project_id FROM comment WHERE id = ?1", [id], |r| r.get(0)).optional()?;
    match owner {
        None => Ok(None),
        Some(p) if p == project => comment_json(c, id).map(Some),
        Some(_) => Err(AppError::Conflict("id: taken".into())),
    }
}

fn version_in_project(c: &Connection, version: &str, project: &str) -> Result<(String, i64)> {
    c.query_row(
        "SELECT v.document_id, v.page_count FROM version v JOIN document d ON d.id = v.document_id WHERE v.id = ?1 AND d.project_id = ?2",
        params![version, project],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound("no such version".into()))
}

/// One of the bundle's files.
pub async fn file(State(st): State<AppState>, Path((token, version, path)): Path<(String, String, String)>, headers: HeaderMap) -> Result<Response> {
    let a = access(&st, &headers, &token)?;
    if !crate::bundle::allowed(&path) || version.len() > 64 || !version.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(AppError::NotFound("no such file".into()));
    }
    st.db.with(|c| version_in_project(c, &version, &a.link.project_id))?;
    if path == "document.pdf" && !a.link.allow_download {
        return Err(AppError::Forbidden("downloads are off for this review".into()));
    }
    let bytes =
        tokio::fs::read(st.data_dir.join("versions").join(&version).join(&path)).await.map_err(|_| AppError::NotFound("no such file".into()))?;
    let content_type = match path.rsplit('.').next() {
        Some("json") => "application/json",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    };
    Ok(([(CONTENT_TYPE, content_type), (CACHE_CONTROL, "private, max-age=86400")], bytes).into_response())
}

#[derive(Deserialize, Default)]
pub struct Quote {
    #[serde(default)]
    exact: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    suffix: String,
}

#[derive(Deserialize)]
pub struct CommentIn {
    /// The client's own id, so a retried request stores the comment once.
    id: Option<String>,
    document: String,
    version: String,
    kind: String,
    page: i64,
    story: Option<i64>,
    cell: Option<Value>,
    footnote: Option<i64>,
    start: Option<i64>,
    end: Option<i64>,
    quote: Option<Quote>,
    rects: Option<Value>,
    x: Option<f64>,
    y: Option<f64>,
    #[serde(default)]
    body: String,
    replacement: Option<String>,
}

pub async fn create_comment(
    State(st): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(c): Json<CommentIn>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = access(&st, &headers, &token)?;
    let me = commenter(&a)?;
    let bad = |m: &str| AppError::BadRequest(m.into());
    if !KINDS.contains(&c.kind.as_str()) {
        return Err(bad("kind: pin, highlight, strike, replace or note"));
    }
    let body = util::text(&c.body, MAX_BODY, "body")?;
    let replacement = c.replacement.as_deref().map(|r| util::text(r, MAX_BODY, "replacement")).transpose()?;
    let anchored = matches!(c.kind.as_str(), "highlight" | "strike" | "replace");
    if anchored {
        let (Some(story), Some(start), Some(end)) = (c.story, c.start, c.end) else { return Err(bad("story, start and end: where the text is")) };
        if story < 0 || start < 0 || end <= start {
            return Err(bad("start and end: a range of bytes, start before end"));
        }
        if c.quote.as_ref().is_none_or(|q| q.exact.trim().is_empty()) {
            return Err(bad("quote.exact: the text itself"));
        }
    }
    if c.kind == "replace" && replacement.is_none() {
        return Err(bad("replacement: the suggested text"));
    }
    if c.kind == "pin" && (c.x.is_none_or(|x| !x.is_finite()) || c.y.is_none_or(|y| !y.is_finite())) {
        return Err(bad("x and y: where the pin goes"));
    }
    if let Some(cell) = &c.cell
        && !(cell.is_object() && cell["table"].is_u64() && cell["row"].is_u64() && cell["col"].is_u64())
    {
        return Err(bad("cell: {table, row, col}"));
    }
    if c.rects.as_ref().is_some_and(|r| !r.is_array() || r.to_string().len() > 20_000) {
        return Err(bad("rects: a list of [left, top, right, bottom]"));
    }
    let id = own_id(c.id.as_deref())?;
    let v = st.db.tx(|conn| {
        if let Some(already) = existing(conn, &id, &a.link.project_id)? {
            return Ok(already);
        }
        let (document, pages) = version_in_project(conn, &c.version, &a.link.project_id)?;
        if document != c.document {
            return Err(bad("version: not of that document"));
        }
        if c.page < 0 || c.page >= pages {
            return Err(bad("page: not in the document"));
        }
        let q = c.quote.unwrap_or_default();
        let t = now();
        conn.execute(
            "INSERT INTO comment (id, project_id, document_id, version_id, author_id, kind, page, story, cell, footnote, start, end, quote_exact, quote_prefix, quote_suffix, rects, x, y, body, replacement, status, created, modified) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, 'open', ?21, ?21)",
            params![
                id, a.link.project_id, c.document, c.version, me.id, c.kind, c.page, c.story,
                c.cell.as_ref().map(Value::to_string), c.footnote, c.start, c.end,
                anchored.then_some(q.exact.as_str()), anchored.then_some(q.prefix.as_str()), anchored.then_some(q.suffix.as_str()),
                c.rects.as_ref().map(Value::to_string), c.x, c.y, body, replacement, t
            ],
        )?;
        record(conn, &st.bus, &a.link.project_id, "comment.created", &me.id, &ip, json!({"comment": id, "document": c.document, "version": c.version, "kind": c.kind, "page": c.page}))?;
        comment_json(conn, &id)
    })?;
    Ok((StatusCode::CREATED, Json(v)))
}

#[derive(Deserialize)]
pub struct ReplyIn {
    id: Option<String>,
    body: String,
}

pub async fn reply(
    State(st): State<AppState>,
    Path((token, parent)): Path<(String, String)>,
    headers: HeaderMap,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(r): Json<ReplyIn>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = access(&st, &headers, &token)?;
    let me = commenter(&a)?;
    let body = util::text(&r.body, MAX_BODY, "body")?;
    if body.is_empty() {
        return Err(AppError::BadRequest("body: say something".into()));
    }
    let id = own_id(r.id.as_deref())?;
    let v = st.db.tx(|c| {
        if let Some(already) = existing(c, &id, &a.link.project_id)? {
            return Ok(already);
        }
        let p: Option<(String, String, i64, Option<String>)> = c
            .query_row("SELECT document_id, version_id, page, in_reply_to FROM comment WHERE id = ?1 AND project_id = ?2", params![parent, a.link.project_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        let Some((document, version, page, in_reply_to)) = p else { return Err(AppError::NotFound("no such comment".into())) };
        let root = in_reply_to.unwrap_or_else(|| parent.clone());
        let t = now();
        c.execute(
            "INSERT INTO comment (id, project_id, document_id, version_id, author_id, kind, page, body, status, created, modified, in_reply_to) VALUES (?1, ?2, ?3, ?4, ?5, 'reply', ?6, ?7, 'open', ?8, ?8, ?9)",
            params![id, a.link.project_id, document, version, me.id, page, body, t, root],
        )?;
        record(c, &st.bus, &a.link.project_id, "comment.replied", &me.id, &ip, json!({"comment": root, "reply": id}))?;
        comment_json(c, &id)
    })?;
    Ok((StatusCode::CREATED, Json(v)))
}

#[derive(Deserialize)]
pub struct CommentPatch {
    body: Option<String>,
    replacement: Option<String>,
    status: Option<String>,
}

/// Edit one's own words; resolve or reopen (members, not guests).
pub async fn patch_comment(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(p): Json<CommentPatch>,
) -> Result<Json<Value>> {
    let a = access(&st, &headers, &token)?;
    let me = commenter(&a)?;
    let v = st.db.tx(|c| {
        let row: Option<(String, String)> = c
            .query_row("SELECT author_id, status FROM comment WHERE id = ?1 AND project_id = ?2", params![id, a.link.project_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let Some((author, status)) = row else { return Err(AppError::NotFound("no such comment".into())) };
        let t = now();
        if p.body.is_some() || p.replacement.is_some() {
            if author != me.id {
                return Err(AppError::Forbidden("only the author edits a comment".into()));
            }
            if let Some(b) = &p.body {
                c.execute("UPDATE comment SET body = ?2, modified = ?3 WHERE id = ?1", params![id, util::text(b, MAX_BODY, "body")?, t])?;
            }
            if let Some(r) = &p.replacement {
                c.execute(
                    "UPDATE comment SET replacement = ?2, modified = ?3 WHERE id = ?1",
                    params![id, util::text(r, MAX_BODY, "replacement")?, t],
                )?;
            }
        }
        if let Some(s) = &p.status {
            if s != "open" && s != "resolved" {
                return Err(AppError::BadRequest("status: open or resolved".into()));
            }
            if me.role == "guest" {
                return Err(AppError::Forbidden("guests don't resolve comments".into()));
            }
            if ["applied", "orphaned"].contains(&status.as_str()) && s == "resolved" {
                return Err(AppError::Conflict(format!("the comment is {status}")));
            }
            c.execute("UPDATE comment SET status = ?2, modified = ?3 WHERE id = ?1", params![id, s, t])?;
        }
        record(
            c,
            &st.bus,
            &a.link.project_id,
            "comment.updated",
            &me.id,
            &ip,
            json!({"comment": id, "status": p.status, "edited": p.body.is_some() || p.replacement.is_some()}),
        )?;
        comment_json(c, &id)
    })?;
    Ok(Json(v))
}

pub async fn list_comments(
    State(st): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>> {
    let a = access(&st, &headers, &token)?;
    let document = q.get("document").cloned().ok_or_else(|| AppError::BadRequest("document: which document".into()))?;
    let version = q.get("version").cloned();
    let v = st.db.with(|c| {
        let ids: Vec<String> = c
            .prepare("SELECT id FROM comment WHERE project_id = ?1 AND document_id = ?2 AND (?3 IS NULL OR version_id = ?3) ORDER BY created")?
            .query_map(params![a.link.project_id, document, version], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.iter().map(|id| comment_json(c, id)).collect::<Result<Vec<_>>>()
    })?;
    Ok(Json(Value::Array(v)))
}

/// The project's events after `since` (or `Last-Event-ID`), then live.
pub async fn events(
    State(st): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Sse<impl Stream<Item = std::result::Result<SseEvent, Infallible>>>> {
    let a = access(&st, &headers, &token)?;
    let since = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .or(q.get("since").map(String::as_str))
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let rx = st.bus.subscribe();
    let backlog = events::since(&st.db, &a.link.project_id, since, 500)?;
    let mut last = backlog.last().map_or(since, |e| e.seq);
    let project = a.link.project_id.clone();
    let live = BroadcastStream::new(rx).filter_map(move |r| {
        let ev = r.ok().filter(|ev| ev.project == project && ev.seq > last);
        if let Some(ev) = &ev {
            last = ev.seq;
        }
        std::future::ready(ev)
    });
    let stream = futures_util::stream::iter(backlog)
        .chain(live)
        .map(|ev| Ok(SseEvent::default().id(ev.seq.to_string()).event(ev.kind.clone()).data(serde_json::to_string(&ev).unwrap_or_default())));
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
