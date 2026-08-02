//! Tag management over `entry_tags` — the slicing mechanism for chronology
//! views. Tags are free-form lowercase (`health`, `witness:sally`, ...);
//! the conventional presets live in `views.rs`.

use anyhow::Result;
use clap::ArgMatches;
use evidence_core::audit::audit;
use evidence_core::sir::parse_sir;
use evidence_core::{Db, SirEntry};
use regex::Regex;
use rusqlite::params;

use crate::{print_sub_help, ACTOR};

/// Conventional preset tags (excluding the open-ended `witness:<name>`).
/// Used by `views`; kept here so tag/untag can stay convention-agnostic.
pub(crate) const PRESET_TAGS: [&str; 8] = [
    "health",
    "contractual",
    "grievance",
    "adjustments",
    "race",
    "disciplinary",
    "litigation",
    "disclosure",
];

/// Normalise a user-supplied tag: trim + lowercase; reject anything outside
/// `[a-z0-9:_-]`. Returns None for invalid input.
pub(crate) fn normalize_tag(raw: &str) -> Option<String> {
    let t = raw.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    let re = Regex::new(r"^[a-z0-9][a-z0-9:_-]*$").unwrap();
    re.is_match(&t).then_some(t)
}

/// Normalise a user-supplied SIR: trim + uppercase; must match the
/// `{PREFIX}-SIR-{seq:04}` shape.
fn normalize_sir(raw: &str) -> Option<String> {
    let t = raw.trim().to_uppercase();
    parse_sir(&t).map(|_| t)
}

/// All tags currently on an entry, sorted. (Test helper.)
#[cfg(test)]
pub(crate) fn tags_of(conn: &rusqlite::Connection, sir: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT tag FROM entry_tags WHERE sir_no = ?1 ORDER BY tag")?;
    let rows = stmt.query_map(params![sir], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<Vec<String>, _>>()?)
}

/// Shared handler for `chronology tag` / `chronology untag`.
/// Missing/invalid args print usage or an error and still return Ok(true) —
/// dispatch must never panic on user input.
pub(crate) fn cmd_tags(db: &Db, m: &ArgMatches, remove: bool) -> Result<bool> {
    let verb = if remove { "untag" } else { "tag" };
    let Some(sir_raw) = m.get_one::<String>("sir") else {
        print_sub_help(verb);
        return Ok(true);
    };
    let raw_tags: Vec<&String> = m
        .get_many::<String>("tags")
        .map(|v| v.collect())
        .unwrap_or_default();
    if raw_tags.is_empty() {
        print_sub_help(verb);
        return Ok(true);
    }
    let Some(sir) = normalize_sir(sir_raw) else {
        eprintln!("error: invalid SIR '{sir_raw}' (expected e.g. AC-SIR-0147)");
        return Ok(true);
    };
    if SirEntry::get(&db.conn, &sir)?.is_none() {
        eprintln!("error: {sir} is not in the Master Chronology");
        return Ok(true);
    }

    let mut changed = 0usize;
    for raw in raw_tags {
        let Some(tag) = normalize_tag(raw) else {
            eprintln!(
                "error: invalid tag '{raw}' (lowercase letters/digits; may contain : _ -)"
            );
            continue;
        };
        let n = if remove {
            db.conn.execute(
                "DELETE FROM entry_tags WHERE sir_no = ?1 AND tag = ?2",
                params![sir, tag],
            )?
        } else {
            db.conn.execute(
                "INSERT OR IGNORE INTO entry_tags(sir_no, tag) VALUES (?1, ?2)",
                params![sir, tag],
            )?
        };
        if n > 0 {
            changed += 1;
            let sign = if remove { "-" } else { "+" };
            audit(&db.conn, ACTOR, verb, &format!("{sir} {sign}{tag}"))?;
            println!("{sir}: {sign}{tag}");
        } else {
            let state = if remove { "not present" } else { "already tagged" };
            println!("{sir}: {tag} {state}");
        }
    }
    if changed == 0 {
        println!("no changes");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags;

    fn seed(db: &Db) {
        SirEntry {
            sir_no: "AC-SIR-0001".into(),
            seq: 1,
            prefix: "AC".into(),
            doc_date: Some("2026-07-03".into()),
            doc_type: Some("Email".into()),
            description: Some("Covering email".into()),
            status: "draft".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
    }

    fn matches_for(args: &[&str]) -> clap::ArgMatches {
        clap::Command::new("x")
            .subcommand(crate::command())
            .get_matches_from(args)
    }

    fn audit_count(db: &Db) -> i64 {
        db.conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn normalizes_tags() {
        assert_eq!(normalize_tag(" Health "), Some("health".into()));
        assert_eq!(normalize_tag("WITNESS:Sally"), Some("witness:sally".into()));
        assert_eq!(normalize_tag("fit-note_2"), Some("fit-note_2".into()));
        assert_eq!(normalize_tag(""), None);
        assert_eq!(normalize_tag("has space"), None);
        assert_eq!(normalize_tag("bad!char"), None);
        assert_eq!(normalize_tag(":leading"), None);
    }

    #[test]
    fn normalizes_sirs() {
        assert_eq!(normalize_sir("ac-sir-0007"), Some("AC-SIR-0007".into()));
        assert_eq!(normalize_sir(" KGS-SIR-0012 "), Some("KGS-SIR-0012".into()));
        assert_eq!(normalize_sir("nonsense"), None);
        assert_eq!(normalize_sir("AC-SIR-1"), None);
    }

    #[test]
    fn tag_untag_roundtrip_with_audit() {
        let db = Db::open_memory().unwrap();
        seed(&db);

        let m = matches_for(&["x", "chronology", "tag", "AC-SIR-0001", "Health", "grievance"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("tag", sm)) = cm.subcommand() else { panic!() };
        assert!(cmd_tags(&db, sm, false).unwrap());
        assert_eq!(tags_of(&db.conn, "AC-SIR-0001").unwrap(), vec!["grievance", "health"]);
        assert_eq!(audit_count(&db), 2);

        // Re-tagging is a no-op: no new rows, no new audit entries.
        let m = matches_for(&["x", "chronology", "tag", "AC-SIR-0001", "health"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("tag", sm)) = cm.subcommand() else { panic!() };
        assert!(cmd_tags(&db, sm, false).unwrap());
        assert_eq!(tags_of(&db.conn, "AC-SIR-0001").unwrap().len(), 2);
        assert_eq!(audit_count(&db), 2);

        let m = matches_for(&["x", "chronology", "untag", "AC-SIR-0001", "HEALTH", "absent"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("untag", sm)) = cm.subcommand() else { panic!() };
        assert!(cmd_tags(&db, sm, true).unwrap());
        assert_eq!(tags_of(&db.conn, "AC-SIR-0001").unwrap(), vec!["grievance"]);
        assert_eq!(audit_count(&db), 3);

        // Audit rows carry the module name and the signed tag detail.
        let detail: String = db
            .conn
            .query_row(
                "SELECT detail FROM audit_log WHERE actor='chronology' AND action='untag'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(detail, "AC-SIR-0001 -health");
    }

    #[test]
    fn rejects_missing_and_unknown_args_gracefully() {
        let db = Db::open_memory().unwrap();
        seed(&db);

        // No SIR at all.
        let m = matches_for(&["x", "chronology", "tag"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("tag", sm)) = cm.subcommand() else { panic!() };
        assert!(cmd_tags(&db, sm, false).unwrap());

        // SIR present but no tags.
        let m = matches_for(&["x", "chronology", "tag", "AC-SIR-0001"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("tag", sm)) = cm.subcommand() else { panic!() };
        assert!(cmd_tags(&db, sm, false).unwrap());

        // Unknown SIR.
        let m = matches_for(&["x", "chronology", "tag", "AC-SIR-9999", "health"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("tag", sm)) = cm.subcommand() else { panic!() };
        assert!(cmd_tags(&db, sm, false).unwrap());
        assert!(tags_of(&db.conn, "AC-SIR-9999").unwrap().is_empty());

        // Malformed SIR.
        let m = matches_for(&["x", "chronology", "tag", "not-a-sir", "health"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("tag", sm)) = cm.subcommand() else { panic!() };
        assert!(cmd_tags(&db, sm, false).unwrap());

        assert_eq!(audit_count(&db), 0);
    }

    #[test]
    fn preset_tags_are_lowercase_conventions() {
        for t in tags::PRESET_TAGS {
            assert_eq!(normalize_tag(t).as_deref(), Some(t));
        }
    }
}
