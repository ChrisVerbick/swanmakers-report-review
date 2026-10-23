//! Mail about comments: an @mention at once, and a daily digest of what's new to the project's
//! owners.

use rusqlite::{Connection, params};
use serde_json::json;

use crate::AppState;
use crate::error::Result;
use crate::events::record;
use crate::rounds::{mailable_link, mailable_members};

/// The members `body` names with an @: by full name or first name, case not counting, the
/// author left out. Returns (id, email, name).
fn mentioned(c: &Connection, project: &str, body: &str, author: &str) -> Result<Vec<(String, String, String)>> {
    let lower = body.to_lowercase();
    if !lower.contains('@') {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for (id, email, name) in mailable_members(c, project)? {
        if id == author || name.trim().is_empty() {
            continue;
        }
        let full = name.trim().to_lowercase();
        let first = full.split_whitespace().next().unwrap_or("").to_string();
        let hit = [full.as_str(), first.as_str()].iter().any(|cand| {
            !cand.is_empty()
                && lower.match_indices(&format!("@{cand}")).any(|(i, m)| lower[i + m.len()..].chars().next().is_none_or(|ch| !ch.is_alphanumeric()))
        });
        if hit {
            out.push((id, email, name));
        }
    }
    Ok(out)
}

/// Mail the members a new comment or reply mentions. Failures are logged: the comment is
/// already stored.
pub async fn mention(st: &AppState, project: &str, comment: &str) {
    let found = st.db.with(|c| {
        let row: Option<(String, String, String, i64, String, String, String)> = c
            .query_row(
                "SELECT c.body, c.document_id, c.version_id, c.page, m.id, m.name, d.title FROM comment c JOIN member m ON m.id = c.author_id JOIN document d ON d.id = c.document_id WHERE c.id = ?1",
                [comment],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()?;
        let Some((body, document, version, page, author_id, author, title)) = row else { return Ok(None) };
        let who = mentioned(c, project, &body, &author_id)?;
        if who.is_empty() {
            return Ok(None);
        }
        let project_name: String = c.query_row("SELECT name FROM project WHERE id = ?1", [project], |r| r.get(0))?;
        let link = mailable_link(c, project, &st.public_origin)?.map(|l| format!("{l}/d/{document}?v={version}#c-{comment}"));
        Ok(Some((who, body, author, title, page, project_name, link)))
    });
    let Ok(Some((who, body, author, title, page, project_name, link))) = found else { return };
    for (_, email, name) in who {
        let text = format!(
            "Hello {name},\n\n{author} mentioned you on \"{project_name}\" ({title}, p. {}):\n\n{body}\n\n{}\n",
            page + 1,
            link.clone().unwrap_or_else(|| "Open the review link you were sent.".into())
        );
        if let Err(e) = st.mailer.send(&email, &format!("{author} mentioned you: {project_name}"), &text).await {
            tracing::warn!("mention mail to {email}: {e}");
        }
    }
}

/// One digest per project with comments newer than its last digest, to its owners (members
/// with role owner and an address of their own). Returns how many mails went.
pub async fn run_once(st: &AppState, now: i64) -> Result<usize> {
    let projects: Vec<(String, String, i64)> = st.db.with(|c| {
        Ok(c.prepare("SELECT id, name, digested FROM project WHERE closed IS NULL AND EXISTS (SELECT 1 FROM comment WHERE project_id = project.id AND created > project.digested)")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?)
    })?;
    let mut sent = 0;
    for (project, name, since) in projects {
        let (owners, lines, total, link) = st.db.with(|c| {
            let owners: Vec<(String, String)> =
                mailable_members(c, &project)?.into_iter().filter_map(|(id, email, n)| is_owner(c, &id).then_some((email, n))).collect();
            let rows: Vec<(String, i64, String, i64, String)> = c
                .prepare(
                    "SELECT d.title, c.page, m.name, c.created, c.body FROM comment c JOIN document d ON d.id = c.document_id JOIN member m ON m.id = c.author_id \
                     WHERE c.project_id = ?1 AND c.created > ?2 ORDER BY d.created, c.created",
                )?
                .query_map(params![project, since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let total = rows.len();
            let mut lines = String::new();
            let mut current = String::new();
            for (title, page, who, _, body) in rows.iter().take(40) {
                if *title != current {
                    lines.push_str(&format!("\n{title}\n"));
                    current = title.clone();
                }
                let short: String = body.chars().take(160).collect();
                lines.push_str(&format!("  - {who}, p. {}: {}\n", page + 1, short.replace('\n', " ")));
            }
            if total > 40 {
                lines.push_str(&format!("\n  … and {} more.\n", total - 40));
            }
            Ok((owners, lines, total, mailable_link(c, &project, &st.public_origin)?))
        })?;
        for (email, who) in &owners {
            let greeting = if who.is_empty() { "Hello".to_string() } else { format!("Hello {who}") };
            let text = format!(
                "{greeting},\n\n\"{name}\": {total} new comment{} since the last digest.\n{lines}\n{}\n",
                if total == 1 { "" } else { "s" },
                link.clone().unwrap_or_else(|| "Open the review in the app's Review panel.".into())
            );
            st.mailer.send(email, &format!("Review digest: {name}"), &text).await?;
            sent += 1;
        }
        st.db.with(|c| {
            c.execute("UPDATE project SET digested = ?2 WHERE id = ?1", params![project, now])?;
            if !owners.is_empty() {
                record(c, &st.bus, &project, "digest.sent", "service", "", json!({"comments": total, "to": owners.len()}))?;
            }
            Ok(())
        })?;
    }
    Ok(sent)
}

fn is_owner(c: &Connection, member: &str) -> bool {
    c.query_row("SELECT role FROM member WHERE id = ?1", [member], |r| r.get::<_, String>(0)).map(|r| r == "owner").unwrap_or(false)
}

use rusqlite::OptionalExtension;
