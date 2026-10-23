//! The SQLite file: one connection behind a mutex (one instance, low traffic), WAL for
//! Litestream, migrations by `user_version`.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;

use crate::error::Result;

// ponytail: one connection, one lock; a pool if the service ever serves more than one box.
#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE project (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, client TEXT NOT NULL DEFAULT '',
    allow_download INTEGER NOT NULL DEFAULT 1, created INTEGER NOT NULL);
CREATE TABLE document (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id),
    app_document_id TEXT NOT NULL UNIQUE, title TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE version (
    id TEXT PRIMARY KEY, document_id TEXT NOT NULL REFERENCES document(id), n INTEGER NOT NULL,
    label TEXT NOT NULL DEFAULT '', uploaded INTEGER NOT NULL, page_count INTEGER NOT NULL,
    checksum TEXT NOT NULL, UNIQUE(document_id, n));
CREATE TABLE member (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id), email TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '', role TEXT NOT NULL, created INTEGER NOT NULL,
    UNIQUE(project_id, email));
CREATE TABLE share_link (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id),
    token_hash TEXT NOT NULL UNIQUE, mode TEXT NOT NULL, password_hash TEXT,
    expires INTEGER, revoked INTEGER, created INTEGER NOT NULL);
CREATE TABLE login_token (
    token_hash TEXT PRIMARY KEY, email TEXT NOT NULL, project_id TEXT NOT NULL,
    link_id TEXT NOT NULL, expires INTEGER NOT NULL, used INTEGER);
CREATE TABLE session (
    id_hash TEXT PRIMARY KEY, member_id TEXT NOT NULL REFERENCES member(id),
    created INTEGER NOT NULL, last_seen INTEGER NOT NULL, expires INTEGER NOT NULL);
CREATE TABLE comment (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id),
    document_id TEXT NOT NULL REFERENCES document(id), version_id TEXT NOT NULL REFERENCES version(id),
    author_id TEXT NOT NULL REFERENCES member(id), kind TEXT NOT NULL, page INTEGER NOT NULL,
    story INTEGER, cell TEXT, footnote INTEGER, start INTEGER, end INTEGER,
    quote_exact TEXT, quote_prefix TEXT, quote_suffix TEXT, rects TEXT, x REAL, y REAL,
    body TEXT NOT NULL DEFAULT '', replacement TEXT, status TEXT NOT NULL DEFAULT 'open',
    created INTEGER NOT NULL, modified INTEGER NOT NULL, in_reply_to TEXT REFERENCES comment(id));
CREATE INDEX comment_document ON comment(document_id, created);
CREATE TABLE event (
    seq INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, kind TEXT NOT NULL,
    actor TEXT NOT NULL, ip TEXT NOT NULL DEFAULT '', payload TEXT NOT NULL, at INTEGER NOT NULL);
CREATE INDEX event_project ON event(project_id, seq);
"#,
    r#"
CREATE TABLE round (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id), n INTEGER NOT NULL,
    name TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', opened INTEGER NOT NULL, due INTEGER,
    locked INTEGER, reminded INTEGER, UNIQUE(project_id, n));
CREATE TABLE round_doc (
    round_id TEXT NOT NULL REFERENCES round(id), document_id TEXT NOT NULL REFERENCES document(id),
    version_id TEXT NOT NULL REFERENCES version(id), PRIMARY KEY (round_id, document_id));
CREATE TABLE decision (
    round_id TEXT NOT NULL REFERENCES round(id), document_id TEXT NOT NULL REFERENCES document(id),
    member_id TEXT NOT NULL REFERENCES member(id), value TEXT NOT NULL, note TEXT NOT NULL DEFAULT '',
    at INTEGER NOT NULL, PRIMARY KEY (round_id, document_id, member_id));
ALTER TABLE comment ADD COLUMN round_id TEXT REFERENCES round(id);
ALTER TABLE share_link ADD COLUMN token TEXT;
"#,
    r#"
ALTER TABLE project ADD COLUMN closed INTEGER;
ALTER TABLE project ADD COLUMN digested INTEGER NOT NULL DEFAULT 0;
ALTER TABLE comment ADD COLUMN path TEXT;
"#,
];

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let c = Connection::open(path)?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let db = Db(Arc::new(Mutex::new(c)));
        db.with(|c| {
            let have: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            for (i, sql) in MIGRATIONS.iter().enumerate() {
                if (i as i64) < have {
                    continue;
                }
                c.execute_batch(&format!("BEGIN; {sql} COMMIT;"))?;
                c.pragma_update(None, "user_version", i as i64 + 1)?;
            }
            Ok(())
        })?;
        Ok(db)
    }

    /// Run `f` with the connection.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let c = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        f(&c)
    }

    /// Run `f` in a transaction that commits when it returns `Ok`.
    pub fn tx<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let c = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        c.execute_batch("BEGIN IMMEDIATE")?;
        match f(&c) {
            Ok(v) => {
                c.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = c.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}
