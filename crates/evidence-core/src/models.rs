//! Row types mirroring the schema. Plain data; modules write their own SQL
//! for their own tables. `SirEntry` gets full CRUD here because every module
//! touches the Master Chronology.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, Default)]
pub struct SirEntry {
    pub sir_no: String,
    pub seq: i64,
    pub prefix: String,
    pub doc_date: Option<String>,
    pub doc_type: Option<String>,
    pub description: Option<String>,
    pub sender: Option<String>,
    pub recipients: Option<String>,
    pub root_source: Option<String>,
    pub source_batch: Option<String>,
    pub original_filename: Option<String>,
    pub standard_filename: Option<String>,
    pub file_path: Option<String>,
    pub sha256: Option<String>,
    pub parent_sir: Option<String>,
    pub status: String,
    pub created_at: String,
}

impl SirEntry {
    pub fn insert(&self, conn: &Connection) -> Result<()> {
        conn.execute(
            "INSERT INTO sir_entries
             (sir_no, seq, prefix, doc_date, doc_type, description, sender, recipients,
              root_source, source_batch, original_filename, standard_filename, file_path,
              sha256, parent_sir, status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                self.sir_no, self.seq, self.prefix, self.doc_date, self.doc_type,
                self.description, self.sender, self.recipients, self.root_source,
                self.source_batch, self.original_filename, self.standard_filename,
                self.file_path, self.sha256, self.parent_sir,
                if self.status.is_empty() { "draft" } else { &self.status },
            ],
        )?;
        Ok(())
    }

    pub fn get(conn: &Connection, sir_no: &str) -> Result<Option<Self>> {
        Ok(conn
            .query_row(
                "SELECT sir_no, seq, prefix, doc_date, doc_type, description, sender,
                        recipients, root_source, source_batch, original_filename,
                        standard_filename, file_path, sha256, parent_sir, status, created_at
                 FROM sir_entries WHERE sir_no = ?1",
                params![sir_no],
                Self::from_row,
            )
            .optional()?)
    }

    pub fn list(conn: &Connection) -> Result<Vec<Self>> {
        let mut stmt = conn.prepare(
            "SELECT sir_no, seq, prefix, doc_date, doc_type, description, sender,
                    recipients, root_source, source_batch, original_filename,
                    standard_filename, file_path, sha256, parent_sir, status, created_at
             FROM sir_entries ORDER BY prefix, seq",
        )?;
        let rows = stmt.query_map([], Self::from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn set_status(conn: &Connection, sir_no: &str, status: &str) -> Result<()> {
        conn.execute(
            "UPDATE sir_entries SET status = ?2 WHERE sir_no = ?1",
            params![sir_no, status],
        )?;
        Ok(())
    }

    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            sir_no: r.get(0)?,
            seq: r.get(1)?,
            prefix: r.get(2)?,
            doc_date: r.get(3)?,
            doc_type: r.get(4)?,
            description: r.get(5)?,
            sender: r.get(6)?,
            recipients: r.get(7)?,
            root_source: r.get(8)?,
            source_batch: r.get(9)?,
            original_filename: r.get(10)?,
            standard_filename: r.get(11)?,
            file_path: r.get(12)?,
            sha256: r.get(13)?,
            parent_sir: r.get(14)?,
            status: r.get(15)?,
            created_at: r.get(16)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Relationship {
    pub id: i64,
    pub from_kind: String,
    pub from_ref: String,
    pub rel_type: String,
    pub to_kind: String,
    pub to_ref: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Deadline {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub hard_date: Option<String>,
    pub internal_date: Option<String>,
    pub source_ref: Option<String>,
    pub calc_method: Option<String>,
    pub confidence: String,
    pub owner: Option<String>,
    pub status: String,
    pub consequence: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn sir_entry_roundtrip() {
        let db = Db::open_memory().unwrap();
        let e = SirEntry {
            sir_no: "AC-SIR-0001".into(),
            seq: 1,
            prefix: "AC".into(),
            doc_date: Some("2026-07-03".into()),
            doc_type: Some("Email".into()),
            description: Some("Covering email".into()),
            status: "draft".into(),
            ..Default::default()
        };
        e.insert(&db.conn).unwrap();
        let got = SirEntry::get(&db.conn, "AC-SIR-0001").unwrap().unwrap();
        assert_eq!(got.description.as_deref(), Some("Covering email"));
        assert_eq!(SirEntry::list(&db.conn).unwrap().len(), 1);
        SirEntry::set_status(&db.conn, "AC-SIR-0001", "confirmed").unwrap();
        let got = SirEntry::get(&db.conn, "AC-SIR-0001").unwrap().unwrap();
        assert_eq!(got.status, "confirmed");
        assert!(SirEntry::get(&db.conn, "AC-SIR-9999").unwrap().is_none());
    }
}
