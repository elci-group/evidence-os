//! Data layer for the shared `disclosure_items` table.
//!
//! One row per disclosure request. Statuses follow the schema comment:
//! `open | answered | partial | objected | closed`.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub(crate) struct DisclosureItem {
    pub id: i64,
    pub request_wording: String,
    pub issue: Option<String>,
    pub date_requested: Option<String>,   // ISO yyyy-mm-dd
    pub recipient: Option<String>,
    pub response_deadline: Option<String>, // ISO
    pub response_received: Option<String>, // ISO
    pub supplied_sir: Option<String>,
    pub status: String,
    pub objection: Option<String>,
    pub bundle_dest: Option<String>,
    pub assists_client: Option<i64>, // 1 helps, 0 harms, NULL unknown
}

impl DisclosureItem {
    /// A fresh `open` request; `insert` assigns the real id.
    pub(crate) fn new(wording: &str) -> Self {
        Self {
            id: 0,
            request_wording: wording.to_string(),
            issue: None,
            date_requested: None,
            recipient: None,
            response_deadline: None,
            response_received: None,
            supplied_sir: None,
            status: "open".to_string(),
            objection: None,
            bundle_dest: None,
            assists_client: None,
        }
    }

    /// True while the other side still owes something.
    pub(crate) fn is_outstanding(&self) -> bool {
        matches!(self.status.as_str(), "open" | "partial" | "objected")
    }
}

const COLS: &str = "id, request_wording, issue, date_requested, recipient, \
                    response_deadline, response_received, supplied_sir, status, \
                    objection, bundle_dest, assists_client";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<DisclosureItem> {
    Ok(DisclosureItem {
        id: r.get(0)?,
        request_wording: r.get(1)?,
        issue: r.get(2)?,
        date_requested: r.get(3)?,
        recipient: r.get(4)?,
        response_deadline: r.get(5)?,
        response_received: r.get(6)?,
        supplied_sir: r.get(7)?,
        status: r.get(8)?,
        objection: r.get(9)?,
        bundle_dest: r.get(10)?,
        assists_client: r.get(11)?,
    })
}

/// Insert a new request and return its id.
pub(crate) fn insert(conn: &Connection, it: &DisclosureItem) -> Result<i64> {
    conn.execute(
        "INSERT INTO disclosure_items
         (request_wording, issue, date_requested, recipient, response_deadline,
          response_received, supplied_sir, status, objection, bundle_dest, assists_client)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            it.request_wording,
            it.issue,
            it.date_requested,
            it.recipient,
            it.response_deadline,
            it.response_received,
            it.supplied_sir,
            it.status,
            it.objection,
            it.bundle_dest,
            it.assists_client,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub(crate) fn get(conn: &Connection, id: i64) -> Result<Option<DisclosureItem>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM disclosure_items WHERE id = ?1"),
            params![id],
            from_row,
        )
        .optional()?)
}

/// All items, optionally filtered. `issue` matches case-insensitively as a
/// substring; `status` matches exactly. Ordered by id.
pub(crate) fn list(
    conn: &Connection,
    status: Option<&str>,
    issue: Option<&str>,
) -> Result<Vec<DisclosureItem>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {COLS} FROM disclosure_items ORDER BY id"))?;
    let rows = stmt
        .query_map([], from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let issue_q = issue.map(|q| q.to_lowercase());
    Ok(rows
        .into_iter()
        .filter(|it| status.map_or(true, |s| it.status == s))
        .filter(|it| {
            issue_q.as_ref().map_or(true, |q| {
                it.issue
                    .as_deref()
                    .map_or(false, |i| i.to_lowercase().contains(q.as_str()))
            })
        })
        .collect())
}

/// Record a response against item `id`. Fields passed as `None` are left
/// untouched; `status` is always written.
pub(crate) fn record_response(
    conn: &Connection,
    id: i64,
    date_iso: Option<&str>,
    sir: Option<&str>,
    status: &str,
    objection: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE disclosure_items SET
            response_received = COALESCE(?2, response_received),
            supplied_sir      = COALESCE(?3, supplied_sir),
            status            = ?4,
            objection         = COALESCE(?5, objection)
         WHERE id = ?1",
        params![id, date_iso, sir, status, objection],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use evidence_core::Db;

    #[test]
    fn insert_get_list_roundtrip() {
        let db = Db::open_memory().unwrap();
        let mut it = DisclosureItem::new("All rota documents for March 2026");
        it.issue = Some("rota".into());
        it.date_requested = Some("2026-06-01".into());
        it.response_deadline = Some("2026-06-15".into());
        it.bundle_dest = Some("PB-12".into());
        it.assists_client = Some(1);
        let id = insert(&db.conn, &it).unwrap();
        assert!(id >= 1);

        let got = get(&db.conn, id).unwrap().unwrap();
        assert_eq!(got.request_wording, "All rota documents for March 2026");
        assert_eq!(got.issue.as_deref(), Some("rota"));
        assert_eq!(got.status, "open");
        assert!(got.is_outstanding());
        assert!(get(&db.conn, 999).unwrap().is_none());

        let all = list(&db.conn, None, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(list(&db.conn, Some("answered"), None).unwrap().len(), 0);
        assert_eq!(list(&db.conn, None, Some("ROTA")).unwrap().len(), 1);
        assert_eq!(list(&db.conn, None, Some("health")).unwrap().len(), 0);
    }

    #[test]
    fn record_response_updates_only_given_fields() {
        let db = Db::open_memory().unwrap();
        // supplied_sir is FK-constrained: the SIR must exist in the chronology.
        evidence_core::SirEntry {
            sir_no: "AC-SIR-0001".into(),
            seq: 1,
            prefix: "AC".into(),
            status: "confirmed".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();

        let id = insert(&db.conn, &DisclosureItem::new("request")).unwrap();
        record_response(&db.conn, id, Some("2026-07-01"), None, "partial", None).unwrap();
        let got = get(&db.conn, id).unwrap().unwrap();
        assert_eq!(got.response_received.as_deref(), Some("2026-07-01"));
        assert_eq!(got.supplied_sir, None);
        assert_eq!(got.status, "partial");

        record_response(&db.conn, id, None, Some("AC-SIR-0001"), "answered", None).unwrap();
        let got = get(&db.conn, id).unwrap().unwrap();
        // date kept from the first call, sir newly set
        assert_eq!(got.response_received.as_deref(), Some("2026-07-01"));
        assert_eq!(got.supplied_sir.as_deref(), Some("AC-SIR-0001"));
        assert_eq!(got.status, "answered");
        assert!(!got.is_outstanding());
    }
}
