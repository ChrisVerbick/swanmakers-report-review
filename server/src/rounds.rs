//! Review rounds: which version of each document reviewers see, until when, and what they
//! decided. A round locks when the owner says so; its versions and comments stay readable.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{AppError, Result};
use crate::events::record;
use crate::util::{self, new_id, now};

/// The project's rounds, oldest first, each with its documents and decisions.
/// id, n, name, note, opened, due, locked
type RoundRow = (String, i64, String, String, i64, Option<i64>, Option<i64>);

pub fn rounds_json(c: &Connection, project: &str, me: Option<&str>) -> Result<Vec<Value>> {
    let mut q = c.prepare("SELECT id, n, name, note, opened, due, locked FROM round WHERE project_id = ?1 ORDER BY n")?;
    let rounds: Vec<RoundRow> = q
        .query_map([project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (id, n, name, note, opened, due, locked) in rounds {
        let documents: Vec<Value> = c
            .prepare(
                "SELECT rd.document_id, rd.version_id, d.title, v.n, v.label FROM round_doc rd JOIN document d ON d.id = rd.document_id JOIN version v ON v.id = rd.version_id \
                 WHERE rd.round_id = ?1 ORDER BY d.created",
            )?
            .query_map([&id], |r| Ok(json!({"document": r.get::<_, String>(0)?, "version": r.get::<_, String>(1)?, "title": r.get::<_, String>(2)?, "n": r.get::<_, i64>(3)?, "label": r.get::<_, String>(4)?})))?
            .collect::<rusqlite::Result<_>>()?;
        let decisions: Vec<Value> = c
            .prepare("SELECT dc.document_id, dc.member_id, m.name, m.role, dc.value, dc.note, dc.at FROM decision dc JOIN member m ON m.id = dc.member_id WHERE dc.round_id = ?1 ORDER BY dc.at")?
            .query_map([&id], |r| {
                Ok(json!({"document": r.get::<_, String>(0)?, "member": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?, "role": r.get::<_, String>(3)?, "value": r.get::<_, String>(4)?, "note": r.get::<_, String>(5)?, "at": r.get::<_, i64>(6)?}))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mine: serde_json::Map<String, Value> = me
            .map(|m| {
                decisions
                    .iter()
                    .filter(|d| d["member"] == m)
                    .map(|d| (d["document"].as_str().unwrap_or("").to_string(), d["value"].clone()))
                    .collect()
            })
            .unwrap_or_default();
        out.push(json!({"id": id, "n": n, "name": name, "note": note, "opened": opened, "due": due, "locked": locked, "documents": documents, "decisions": decisions, "mine": mine}));
    }
    Ok(out)
}

/// The latest round's id, if any.
pub fn current(c: &Connection, project: &str) -> Result<Option<String>> {
    Ok(c.query_row("SELECT id FROM round WHERE project_id = ?1 ORDER BY n DESC LIMIT 1", [project], |r| r.get(0)).optional()?)
}

/// The latest round that shows `version` of `document`: its id and whether it is locked.
pub fn round_of_version(c: &Connection, document: &str, version: &str) -> Result<Option<(String, bool)>> {
    Ok(c.query_row(
        "SELECT r.id, r.locked IS NOT NULL FROM round_doc rd JOIN round r ON r.id = rd.round_id WHERE rd.document_id = ?1 AND rd.version_id = ?2 ORDER BY r.n DESC LIMIT 1",
        params![document, version],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
    )
    .optional()?)
}

/// The round a new comment on `version` goes in: none outside rounds; refused in a locked one.
pub fn round_for_comment(c: &Connection, document: &str, version: &str) -> Result<Option<String>> {
    match round_of_version(c, document, version)? {
        Some((_, true)) => Err(AppError::Forbidden("this round is locked; comments go on the current version".into())),
        Some((id, false)) => Ok(Some(id)),
        None => Ok(None),
    }
}

/// A new round: `documents` as `[{document, version?}]`, or every document at its latest version.
pub struct NewRound<'a> {
    pub name: Option<&'a str>,
    pub due: Option<i64>,
    pub note: &'a str,
    /// `[{document, version?}]`; every document at its latest version when absent.
    pub documents: Option<&'a [Value]>,
}

pub fn create(
    c: &Connection,
    bus: &tokio::sync::broadcast::Sender<crate::events::Event>,
    project: &str,
    new: &NewRound,
    actor: &str,
    ip: &str,
) -> Result<Value> {
    let NewRound { name, due, note, documents } = *new;
    let n: i64 = c.query_row("SELECT COALESCE(MAX(n), 0) + 1 FROM round WHERE project_id = ?1", [project], |r| r.get(0))?;
    let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => util::text(n, 200, "name")?,
        None => format!("Round {n}"),
    };
    let id = new_id();
    c.execute(
        "INSERT INTO round (id, project_id, n, name, note, opened, due) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, project, n, name, note, now(), due],
    )?;
    let latest = |document: &str| -> Result<Option<String>> {
        Ok(c.query_row("SELECT id FROM version WHERE document_id = ?1 ORDER BY n DESC LIMIT 1", [document], |r| r.get(0)).optional()?)
    };
    let mut shown = 0;
    match documents {
        Some(list) => {
            for d in list {
                let document = d["document"].as_str().ok_or_else(|| AppError::BadRequest("documents: [{document, version?}]".into()))?;
                let in_project: Option<String> =
                    c.query_row("SELECT id FROM document WHERE id = ?1 AND project_id = ?2", params![document, project], |r| r.get(0)).optional()?;
                if in_project.is_none() {
                    return Err(AppError::BadRequest(format!("documents: {document} isn't in this project")));
                }
                let version = match d["version"].as_str() {
                    Some(v) => {
                        let ok: Option<String> = c
                            .query_row("SELECT id FROM version WHERE id = ?1 AND document_id = ?2", params![v, document], |r| r.get(0))
                            .optional()?;
                        ok.ok_or_else(|| AppError::BadRequest(format!("documents: {v} isn't a version of {document}")))?
                    }
                    None => latest(document)?.ok_or_else(|| AppError::BadRequest(format!("documents: {document} has no version yet")))?,
                };
                c.execute(
                    "INSERT OR REPLACE INTO round_doc (round_id, document_id, version_id) VALUES (?1, ?2, ?3)",
                    params![id, document, version],
                )?;
                shown += 1;
            }
        }
        None => {
            let docs: Vec<String> = c
                .prepare("SELECT id FROM document WHERE project_id = ?1 ORDER BY created")?
                .query_map([project], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for document in docs {
                if let Some(version) = latest(&document)? {
                    c.execute("INSERT INTO round_doc (round_id, document_id, version_id) VALUES (?1, ?2, ?3)", params![id, document, version])?;
                    shown += 1;
                }
            }
        }
    }
    if shown == 0 {
        return Err(AppError::BadRequest("a round needs a document with a version".into()));
    }
    record(c, bus, project, "round.opened", actor, ip, json!({"round": id, "n": n, "name": name, "due": due}))?;
    round_json(c, &id)
}

pub fn round_json(c: &Connection, id: &str) -> Result<Value> {
    let project: Option<String> = c.query_row("SELECT project_id FROM round WHERE id = ?1", [id], |r| r.get(0)).optional()?;
    let project = project.ok_or_else(|| AppError::NotFound("no such round".into()))?;
    rounds_json(c, &project, None)?.into_iter().find(|r| r["id"] == id).ok_or_else(|| AppError::NotFound("no such round".into()))
}

pub fn lock(c: &Connection, bus: &tokio::sync::broadcast::Sender<crate::events::Event>, id: &str, actor: &str, ip: &str) -> Result<Value> {
    let project: Option<String> = c.query_row("SELECT project_id FROM round WHERE id = ?1", [id], |r| r.get(0)).optional()?;
    let project = project.ok_or_else(|| AppError::NotFound("no such round".into()))?;
    c.execute("UPDATE round SET locked = ?2 WHERE id = ?1 AND locked IS NULL", params![id, now()])?;
    record(c, bus, &project, "round.locked", actor, ip, json!({"round": id}))?;
    round_json(c, id)
}

/// A reviewer's decision on a document of an open round; a second decision replaces the first.
pub struct Decision<'a> {
    pub round: &'a str,
    pub document: &'a str,
    pub member: &'a str,
    pub value: &'a str,
    pub note: &'a str,
}

pub fn decide(c: &Connection, bus: &tokio::sync::broadcast::Sender<crate::events::Event>, project: &str, d: &Decision, ip: &str) -> Result<Value> {
    let Decision { round, document, member, value, note } = *d;
    if value != "approved" && value != "changes" {
        return Err(AppError::BadRequest("value: approved or changes".into()));
    }
    let row: Option<(String, Option<i64>)> =
        c.query_row("SELECT project_id, locked FROM round WHERE id = ?1", [round], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let Some((p, locked)) = row else { return Err(AppError::NotFound("no such round".into())) };
    if p != project {
        return Err(AppError::NotFound("no such round".into()));
    }
    if locked.is_some() {
        return Err(AppError::Forbidden("this round is locked".into()));
    }
    let shown: Option<String> = c
        .query_row("SELECT version_id FROM round_doc WHERE round_id = ?1 AND document_id = ?2", params![round, document], |r| r.get(0))
        .optional()?;
    if shown.is_none() {
        return Err(AppError::BadRequest("document: not in this round".into()));
    }
    let at = now();
    c.execute(
        "INSERT INTO decision (round_id, document_id, member_id, value, note, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(round_id, document_id, member_id) DO UPDATE SET value = excluded.value, note = excluded.note, at = excluded.at",
        params![round, document, member, value, note, at],
    )?;
    record(c, bus, project, "decision.made", member, ip, json!({"round": round, "document": document, "value": value}))?;
    let name: String = c.query_row("SELECT name FROM member WHERE id = ?1", [member], |r| r.get(0))?;
    Ok(json!({"round": round, "document": document, "member": member, "name": name, "value": value, "note": note, "at": at}))
}

/// The project's invite link that can be mailed (its token is kept for invite links), newest.
fn mailable_link(c: &Connection, project: &str, origin: &str) -> Result<Option<String>> {
    let token: Option<String> = c
        .query_row(
            "SELECT token FROM share_link WHERE project_id = ?1 AND token IS NOT NULL AND revoked IS NULL ORDER BY created DESC LIMIT 1",
            [project],
            |r| r.get(0),
        )
        .optional()?;
    Ok(token.map(|t| format!("{origin}/r/{t}")))
}

/// Members to mail: invited ones with an address of their own (not guests, not the owner the
/// app replies as).
fn mailable_members(c: &Connection, project: &str) -> Result<Vec<(String, String, String)>> {
    Ok(c.prepare("SELECT id, email, name FROM member WHERE project_id = ?1 AND role IN ('owner', 'reviewer') AND email <> 'owner@review.local' ORDER BY created")?
        .query_map([project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Reminders for rounds due within a day that haven't had one: to every member who hasn't
/// decided on every document. Returns how many mails went.
pub async fn remind_once(st: &AppState, now: i64) -> Result<usize> {
    let due_soon: Vec<(String, String, String, i64, String)> = st.db.with(|c| {
        Ok(c.prepare(
            "SELECT r.id, r.project_id, r.name, r.due, p.name FROM round r JOIN project p ON p.id = r.project_id \
             WHERE r.due IS NOT NULL AND r.locked IS NULL AND r.reminded IS NULL AND r.due > ?1 AND r.due - ?1 <= 86400",
        )?
        .query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?)
    })?;
    let mut sent = 0;
    for (round, project, name, due, project_name) in due_soon {
        let (members, url) = st.db.with(|c| {
            let docs: i64 = c.query_row("SELECT COUNT(*) FROM round_doc WHERE round_id = ?1", [&round], |r| r.get(0))?;
            let mut waiting = Vec::new();
            for (id, email, member_name) in mailable_members(c, &project)? {
                let decided: i64 =
                    c.query_row("SELECT COUNT(*) FROM decision WHERE round_id = ?1 AND member_id = ?2", params![round, id], |r| r.get(0))?;
                if decided < docs {
                    waiting.push((email, member_name, docs - decided));
                }
            }
            Ok((waiting, mailable_link(c, &project, &st.public_origin)?))
        })?;
        for (email, member_name, open) in members {
            let greeting = if member_name.is_empty() { "Hello".to_string() } else { format!("Hello {member_name}") };
            let body = format!(
                "{greeting},\n\n\"{project_name}\": {name} is due {} ({}). {open} document{} still wait{} for your decision.\n\n{}\n",
                util::date(due),
                "tomorrow",
                if open == 1 { "" } else { "s" },
                if open == 1 { "s" } else { "" },
                url.clone().unwrap_or_else(|| "Open the review link you were sent.".into())
            );
            st.mailer.send(&email, &format!("Review due tomorrow: {project_name}"), &body).await?;
            sent += 1;
        }
        st.db.with(|c| {
            c.execute("UPDATE round SET reminded = ?2 WHERE id = ?1", params![round, now])?;
            record(c, &st.bus, &project, "reminder.sent", "service", "", json!({"round": round, "sent": sent}))?;
            Ok(())
        })?;
    }
    Ok(sent)
}

/// Mail an invite link to the project's members (those in `only`, when given). Returns how
/// many mails went.
pub async fn invite(st: &AppState, link_id: &str, only: Option<&[String]>, ip: &str) -> Result<usize> {
    let (project, url, project_name) = st.db.with(|c| {
        let row: Option<(String, Option<String>)> = c
            .query_row("SELECT project_id, token FROM share_link WHERE id = ?1 AND revoked IS NULL", [link_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((project, token)) = row else { return Err(AppError::NotFound("no such link".into())) };
        let token = token
            .ok_or_else(|| AppError::BadRequest("only invite links can be mailed: the service keeps no copy of a public link's token".into()))?;
        let name: String = c.query_row("SELECT name FROM project WHERE id = ?1", [&project], |r| r.get(0))?;
        Ok((project, format!("{}/r/{token}", st.public_origin), name))
    })?;
    let members = st.db.with(|c| mailable_members(c, &project))?;
    let wanted: Option<Vec<String>> = only.map(|l| l.iter().map(|e| e.trim().to_lowercase()).collect());
    let mut sent = 0;
    for (_, email, name) in members.iter().filter(|(_, e, _)| wanted.as_ref().is_none_or(|w| w.contains(&e.to_lowercase()))) {
        let greeting = if name.is_empty() { "Hello".to_string() } else { format!("Hello {name}") };
        let body = format!(
            "{greeting},\n\nYou are invited to review \"{project_name}\":\n\n{url}\n\nOpen it, enter this address, and a sign-in link comes by mail.\n"
        );
        st.mailer.send(email, &format!("Review: {project_name}"), &body).await?;
        sent += 1;
    }
    st.db.with(|c| {
        record(c, &st.bus, &project, "invite.sent", "owner", ip, json!({"link": link_id, "sent": sent}))?;
        Ok(())
    })?;
    Ok(sent)
}
