//! Every mutating module action is recorded in `audit_log`.

use anyhow::Result;
use rusqlite::{params, Connection};

/// Append an audit entry. `actor` is the module name (e.g. "harvester"),
/// `action` the verb (e.g. "allocate", "rename"), `detail` free text.
pub fn audit(conn: &Connection, actor: &str, action: &str, detail: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_log(actor, action, detail) VALUES (?1, ?2, ?3)",
        params![actor, action, detail],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn writes_audit_rows() {
        let db = Db::open_memory().unwrap();
        audit(&db.conn, "harvester", "allocate", "AC-SIR-0001").unwrap();
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
