//! `chronology views`: the preset tag-based slices over the Master
//! Chronology, with current entry counts. `witness:<name>` views are
//! open-ended, so they are listed from the tags actually present.

use anyhow::Result;
use evidence_core::Db;
use rusqlite::Connection;

use crate::tags::PRESET_TAGS;

/// (view name, entry count) pairs: the eight fixed presets followed by every
/// distinct `witness:*` tag in use, alphabetically.
pub(crate) fn view_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut out = Vec::new();
    for t in PRESET_TAGS {
        let n: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT sir_no) FROM entry_tags WHERE tag = ?1",
            [t],
            |r| r.get(0),
        )?;
        out.push((t.to_string(), n));
    }
    let mut stmt = conn.prepare(
        "SELECT tag, COUNT(DISTINCT sir_no) FROM entry_tags
         WHERE tag LIKE 'witness:%' GROUP BY tag ORDER BY tag",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn cmd_views(db: &Db) -> Result<bool> {
    let counts = view_counts(&db.conn)?;
    let width = counts
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    println!("{:<width$}  ENTRIES", "VIEW", width = width);
    println!("{}  -------", "-".repeat(width));
    for (name, n) in &counts {
        println!("{name:<width$}  {n}", width = width);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use evidence_core::SirEntry;

    fn seed(db: &Db) {
        for (i, sir) in ["AC-SIR-0001", "AC-SIR-0002", "AC-SIR-0003"].iter().enumerate() {
            SirEntry {
                sir_no: sir.to_string(),
                seq: (i + 1) as i64,
                prefix: "AC".into(),
                status: "draft".into(),
                ..Default::default()
            }
            .insert(&db.conn)
            .unwrap();
        }
        let tag = |sir: &str, t: &str| {
            db.conn
                .execute(
                    "INSERT INTO entry_tags(sir_no, tag) VALUES (?1, ?2)",
                    rusqlite::params![sir, t],
                )
                .unwrap();
        };
        tag("AC-SIR-0001", "health");
        tag("AC-SIR-0002", "health");
        tag("AC-SIR-0002", "grievance");
        tag("AC-SIR-0003", "witness:sally");
        tag("AC-SIR-0001", "witness:sally");
        tag("AC-SIR-0003", "witness:amir");
    }

    #[test]
    fn counts_presets_and_dynamic_witness_views() {
        let db = Db::open_memory().unwrap();
        seed(&db);
        let counts = view_counts(&db.conn).unwrap();
        let get = |name: &str| counts.iter().find(|(n, _)| n == name).map(|(_, c)| *c);

        assert_eq!(get("health"), Some(2));
        assert_eq!(get("grievance"), Some(1));
        assert_eq!(get("race"), Some(0)); // presets always listed
        assert_eq!(get("disclosure"), Some(0));
        assert_eq!(get("witness:amir"), Some(1));
        assert_eq!(get("witness:sally"), Some(2));
        assert_eq!(counts.len(), PRESET_TAGS.len() + 2);

        // Fixed presets first, witnesses alphabetical at the end.
        assert_eq!(counts[8].0, "witness:amir");
        assert_eq!(counts[9].0, "witness:sally");
    }

    #[test]
    fn empty_register_still_lists_all_presets() {
        let db = Db::open_memory().unwrap();
        let counts = view_counts(&db.conn).unwrap();
        assert_eq!(counts.len(), PRESET_TAGS.len());
        assert!(counts.iter().all(|(_, n)| *n == 0));
    }
}
