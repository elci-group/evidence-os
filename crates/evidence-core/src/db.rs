use std::path::Path;

use anyhow::Context;
use rusqlite::Connection;

/// Schema v1. All modules share these tables. Additions go through a new
/// migration in `migrate`, never by editing shipped SQL in place.
pub const SCHEMA_V1: &str = r#"
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- The Master Chronology: one row per harvested evidence item.
CREATE TABLE IF NOT EXISTS sir_entries (
    sir_no            TEXT PRIMARY KEY,          -- e.g. AC-SIR-0147
    seq               INTEGER NOT NULL,
    prefix            TEXT NOT NULL,             -- e.g. AC, IG, DC, VO, KGS
    doc_date          TEXT,                      -- ISO yyyy-mm-dd
    doc_type          TEXT,                      -- Email, Letter, Medical, ...
    description       TEXT,
    sender            TEXT,
    recipients        TEXT,
    root_source       TEXT,                      -- where the evidence ultimately came from
    source_batch      TEXT,                      -- e.g. PB-10
    original_filename TEXT,
    standard_filename TEXT,
    file_path         TEXT,
    sha256            TEXT,
    parent_sir        TEXT REFERENCES sir_entries(sir_no),
    status            TEXT NOT NULL DEFAULT 'draft',  -- draft | confirmed | disclosed | bundle
    created_at        TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS entry_tags (
    sir_no TEXT NOT NULL REFERENCES sir_entries(sir_no),
    tag    TEXT NOT NULL,                        -- health, grievance, race, disclosure, witness:sally, ...
    PRIMARY KEY (sir_no, tag)
);

CREATE TABLE IF NOT EXISTS disclosure_items (
    id                 INTEGER PRIMARY KEY,
    request_wording    TEXT NOT NULL,
    issue              TEXT,
    date_requested     TEXT,                     -- ISO
    recipient          TEXT,
    response_deadline  TEXT,                     -- ISO
    response_received  TEXT,                     -- ISO date of response
    supplied_sir       TEXT REFERENCES sir_entries(sir_no),
    status             TEXT NOT NULL DEFAULT 'open',  -- open | answered | partial | objected | closed
    objection          TEXT,
    bundle_dest        TEXT,
    assists_client     INTEGER,                  -- 1 helps, 0 harms, NULL unknown
    follow_up_required INTEGER NOT NULL DEFAULT 0,
    notes              TEXT,
    created_at         TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Generic evidence graph edges: emails->attachments, witness->event,
-- event->allegation, policy->breach, request->response, doc->issue.
CREATE TABLE IF NOT EXISTS relationships (
    id         INTEGER PRIMARY KEY,
    from_kind  TEXT NOT NULL,   -- sir | person | event | allegation | issue | policy | request
    from_ref   TEXT NOT NULL,   -- e.g. AC-SIR-0147 or a free-text name
    rel_type   TEXT NOT NULL,   -- parent-of | attachment-of | witness-to | supports | breaches | responds-to | disclosed-as | about
    to_kind    TEXT NOT NULL,
    to_ref     TEXT NOT NULL,
    note       TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (from_kind, from_ref, rel_type, to_kind, to_ref)
);

CREATE TABLE IF NOT EXISTS deadlines (
    id            INTEGER PRIMARY KEY,
    kind          TEXT NOT NULL,   -- limitation | acas | order | disclosure | witness | medical | appeal | grievance | fitnote | prep | internal
    title         TEXT NOT NULL,
    hard_date     TEXT,            -- ISO, the true deadline
    internal_date TEXT,            -- ISO, SAS safety date before the true deadline
    source_ref    TEXT,            -- document/SIR the date comes from
    calc_method   TEXT,            -- human-readable workings; mandatory for limitation dates
    confidence    TEXT NOT NULL DEFAULT 'unconfirmed',  -- unconfirmed | confirmed
    owner         TEXT,
    status        TEXT NOT NULL DEFAULT 'open',  -- open | completed | changed | missed
    consequence   TEXT,            -- what happens if missed
    reminder_days TEXT,            -- e.g. "30,14,7,2"
    notes         TEXT,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS meetings (
    id          INTEGER PRIMARY KEY,
    title       TEXT,
    held_at     TEXT,              -- ISO
    source_path TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS actions (
    id               INTEGER PRIMARY KEY,
    meeting_id       INTEGER REFERENCES meetings(id),
    kind             TEXT NOT NULL DEFAULT 'action',  -- action | decision
    text             TEXT NOT NULL,
    owner            TEXT,
    due              TEXT,         -- ISO
    dependency       TEXT,
    issue            TEXT,
    register         TEXT,         -- lines-of-enquiry | campaign | disclosure | dashboard
    status           TEXT NOT NULL DEFAULT 'proposed',  -- proposed | approved | completed | superseded | reversed
    source_timestamp TEXT,         -- where in the recording/transcript it came from
    created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS content_blocks (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,      -- heading | parties | procedural | medical | disclosure-request | grievance | adjustment | caveats | ...
    title      TEXT NOT NULL,
    body       TEXT NOT NULL,      -- may contain {{placeholders}}
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Consistency auditor findings, one row per flag per run.
CREATE TABLE IF NOT EXISTS audit_findings (
    id       INTEGER PRIMARY KEY,
    run_at   TEXT NOT NULL DEFAULT (datetime('now')),
    severity TEXT NOT NULL,        -- info | warn | high
    rule     TEXT NOT NULL,        -- machine rule name, e.g. conflicting-date
    subject  TEXT,                 -- what it concerns
    detail   TEXT
);

CREATE TABLE IF NOT EXISTS audit_log (
    id      INTEGER PRIMARY KEY,
    ts      TEXT NOT NULL DEFAULT (datetime('now')),
    actor   TEXT NOT NULL,         -- module name, e.g. harvester
    action  TEXT NOT NULL,         -- e.g. rename, insert, allocate
    detail  TEXT
);
"#;

/// A case database. Wraps a single SQLite connection; cheap to clone-free
/// share by reference. All module crates receive `&Db`.
pub struct Db {
    pub conn: Connection,
}

impl Db {
    /// Open (creating if needed) the case database at `path` and migrate.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// In-memory database for tests.
    pub fn open_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(SCHEMA_V1)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_applies_twice_idempotently() {
        let db = Db::open_memory().unwrap();
        db.migrate().unwrap();
        let n: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(n >= 10, "expected all module tables, got {n}");
    }

    #[test]
    fn opens_file_backed_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("case.db");
        let db = Db::open(&path).unwrap();
        db.conn
            .execute("INSERT INTO meta(key, value) VALUES('k','v')", [])
            .unwrap();
        drop(db);
        let db2 = Db::open(&path).unwrap();
        let v: String = db2
            .conn
            .query_row("SELECT value FROM meta WHERE key='k'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "v");
    }
}
