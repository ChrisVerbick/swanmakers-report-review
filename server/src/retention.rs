//! Closed projects are deleted, rows and bundles, once the retention period has passed.

use rusqlite::params;

use crate::AppState;
use crate::error::Result;

/// Delete every project closed more than `days` days before `now`. Returns how many went.
pub fn run_once(st: &AppState, now: i64, days: i64) -> Result<usize> {
    let cutoff = now.saturating_sub(days.saturating_mul(86_400));
    let due: Vec<String> = st.db.with(|c| {
        Ok(c.prepare("SELECT id FROM project WHERE closed IS NOT NULL AND closed <= ?1")?
            .query_map([cutoff], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })?;
    for project in &due {
        let versions: Vec<String> = st.db.with(|c| {
            Ok(c.prepare("SELECT v.id FROM version v JOIN document d ON d.id = v.document_id WHERE d.project_id = ?1")?
                .query_map([project], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })?;
        for v in &versions {
            let dir = st.data_dir.join("versions").join(v);
            if dir.is_dir()
                && let Err(e) = std::fs::remove_dir_all(&dir)
            {
                tracing::warn!("retention: {}: {e}", dir.display());
            }
        }
        st.db.tx(|c| {
            for sql in [
                "DELETE FROM decision WHERE round_id IN (SELECT id FROM round WHERE project_id = ?1)",
                "DELETE FROM round_doc WHERE round_id IN (SELECT id FROM round WHERE project_id = ?1)",
                "DELETE FROM round WHERE project_id = ?1",
                "DELETE FROM comment WHERE project_id = ?1",
                "DELETE FROM session WHERE member_id IN (SELECT id FROM member WHERE project_id = ?1)",
                "DELETE FROM login_token WHERE project_id = ?1",
                "DELETE FROM share_link WHERE project_id = ?1",
                "DELETE FROM member WHERE project_id = ?1",
                "DELETE FROM version WHERE document_id IN (SELECT id FROM document WHERE project_id = ?1)",
                "DELETE FROM document WHERE project_id = ?1",
                "DELETE FROM event WHERE project_id = ?1",
                "DELETE FROM project WHERE id = ?1",
            ] {
                c.execute(sql, params![project])?;
            }
            Ok(())
        })?;
        tracing::info!("retention: project {project} deleted ({} version(s))", versions.len());
    }
    Ok(due.len())
}
