//! The audit log, which is also what the event stream replays.

use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::db::Db;
use crate::error::Result;
use crate::util;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub seq: i64,
    pub project: String,
    pub kind: String,
    pub actor: String,
    pub payload: Value,
    pub at: i64,
}

/// Append an event and tell the live streams.
pub fn record(c: &Connection, bus: &broadcast::Sender<Event>, project: &str, kind: &str, actor: &str, ip: &str, payload: Value) -> Result<i64> {
    let at = util::now();
    c.execute(
        "INSERT INTO event (project_id, kind, actor, ip, payload, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![project, kind, actor, ip, payload.to_string(), at],
    )?;
    let seq = c.last_insert_rowid();
    let _ = bus.send(Event { seq, project: project.into(), kind: kind.into(), actor: actor.into(), payload, at });
    Ok(seq)
}

/// Up to `limit` events of `project` after `since`, oldest first.
pub fn since(db: &Db, project: &str, since: i64, limit: usize) -> Result<Vec<Event>> {
    db.with(|c| {
        let mut q = c.prepare("SELECT seq, kind, actor, payload, at FROM event WHERE project_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3")?;
        let rows = q.query_map(params![project, since, limit as i64], |r| {
            Ok(Event {
                seq: r.get(0)?,
                project: project.to_string(),
                kind: r.get(1)?,
                actor: r.get(2)?,
                payload: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
                at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
}
