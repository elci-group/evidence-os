//! End-to-end tests through the module's public surface:
//! `command()` -> clap parse -> `try_dispatch()` against a memory DB.

use clap::Command;
use evidence_chronology::{command, try_dispatch};
use evidence_core::{Db, SirEntry};

fn dispatch(db: &Db, args: &[&str]) -> bool {
    let m = Command::new("evidenceos")
        .subcommand(command())
        .get_matches_from(args);
    try_dispatch(m.subcommand(), db).unwrap()
}

fn seed_entry(db: &Db, sir: &str, seq: i64, date: Option<&str>, desc: &str) {
    SirEntry {
        sir_no: sir.into(),
        seq,
        prefix: "AC".into(),
        doc_date: date.map(String::from),
        doc_type: Some("Email".into()),
        description: Some(desc.into()),
        sender: Some("alex@example.com".into()),
        source_batch: Some("PB-10".into()),
        status: "draft".into(),
        ..Default::default()
    }
    .insert(&db.conn)
    .unwrap();
}

fn audit_rows(db: &Db) -> Vec<(String, String, String)> {
    let mut stmt = db
        .conn
        .prepare("SELECT actor, action, detail FROM audit_log ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn tag_then_build_roundtrip() {
    let db = Db::open_memory().unwrap();
    seed_entry(&db, "AC-SIR-0001", 1, Some("2026-03-01"), "Grievance letter sent");
    seed_entry(&db, "AC-SIR-0002", 2, Some("2026-03-15"), "OH report received");

    assert!(dispatch(&db, &["evidenceos", "chronology", "tag", "ac-sir-0001", "Health", "grievance"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "tag", "AC-SIR-0002", "health"]));

    let tags: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM entry_tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tags, 3);
    let audits = audit_rows(&db);
    assert_eq!(audits.len(), 3);
    assert!(audits.iter().all(|(actor, action, _)| actor == "chronology" && action == "tag"));
    assert_eq!(audits[0].2, "AC-SIR-0001 +health");

    // Builds with every filter combination dispatch cleanly.
    assert!(dispatch(&db, &["evidenceos", "chronology", "build"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "build", "--tag", "health", "--tag", "grievance"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "build", "--from", "01/03/2026", "--to", "31/03/2026"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "build", "--format", "json"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "build", "--format", "csv", "--prefix", "ac"]));

    assert!(dispatch(&db, &["evidenceos", "chronology", "untag", "AC-SIR-0001", "health"]));
    let remaining: Vec<String> = db
        .conn
        .prepare("SELECT tag FROM entry_tags WHERE sir_no='AC-SIR-0001'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(remaining, ["grievance"]);
}

#[test]
fn build_out_writes_file_and_audits() {
    let db = Db::open_memory().unwrap();
    seed_entry(&db, "AC-SIR-0001", 1, Some("2026-07-03"), "Covering email");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("chronology.csv");
    let out_s = out.to_str().unwrap().to_string();

    assert!(dispatch(&db, &["evidenceos", "chronology", "build", "--format", "csv", "--out", &out_s]));
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.starts_with("sir,date,type,description,sender,batch,tags"));
    assert!(text.contains("AC-SIR-0001,03/07/2026,Email,Covering email"));

    let audits = audit_rows(&db);
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].1, "build");
    assert!(audits[0].2.contains("1 row(s)"));
}

#[test]
fn export_writes_full_register_csv_and_audits() {
    let db = Db::open_memory().unwrap();
    seed_entry(&db, "AC-SIR-0001", 1, Some("2026-07-03"), "Covering email");
    seed_entry(&db, "AC-SIR-0002", 2, None, "Undated note");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("register.csv");
    let out_s = out.to_str().unwrap().to_string();

    assert!(dispatch(&db, &["evidenceos", "chronology", "export", "--format", "csv", "--out", &out_s]));
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.starts_with(
        "sir,date,type,description,sender,recipients,batch,parent_sir,status,tags,sha256,standard_filename"
    ));
    assert_eq!(text.lines().count(), 3); // header + 2 rows
    assert!(text.contains("AC-SIR-0001,03/07/2026"));

    let audits = audit_rows(&db);
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].1, "export");

    // Missing --out or --format: usage, not a panic.
    assert!(dispatch(&db, &["evidenceos", "chronology", "export", "--format", "csv"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "export", "--out", &out_s]));
    // Unsupported format is a friendly error.
    assert!(dispatch(&db, &["evidenceos", "chronology", "export", "--format", "json", "--out", &out_s]));
    assert_eq!(audit_rows(&db).len(), 1); // nothing new audited
}

#[test]
fn check_dispatches_on_clean_and_dirty_registers() {
    let db = Db::open_memory().unwrap();
    seed_entry(&db, "AC-SIR-0001", 1, Some("2026-03-01"), "First event");
    seed_entry(&db, "AC-SIR-0002", 2, Some("2026-03-10"), "Second event");
    assert!(dispatch(&db, &["evidenceos", "chronology", "check"]));

    // Dirty it: dangling parent + dangling ref (FK off to plant the state
    // an integrity scan exists to catch).
    db.conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    SirEntry {
        sir_no: "AC-SIR-0003".into(),
        seq: 3,
        prefix: "AC".into(),
        doc_date: Some("2026-03-12".into()),
        description: Some("Refers to AC-SIR-0999".into()),
        parent_sir: Some("AC-SIR-0998".into()),
        status: "draft".into(),
        ..Default::default()
    }
    .insert(&db.conn)
    .unwrap();
    db.conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    assert!(dispatch(&db, &["evidenceos", "chronology", "check"]));
}

#[test]
fn views_dispatches() {
    let db = Db::open_memory().unwrap();
    seed_entry(&db, "AC-SIR-0001", 1, Some("2026-03-01"), "Event");
    assert!(dispatch(&db, &["evidenceos", "chronology", "tag", "AC-SIR-0001", "witness:sally"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "views"]));
}

#[test]
fn missing_args_print_usage_not_panic() {
    let db = Db::open_memory().unwrap();
    assert!(dispatch(&db, &["evidenceos", "chronology", "tag"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "tag", "AC-SIR-0001"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "untag"]));
    assert!(dispatch(&db, &["evidenceos", "chronology"]));
    assert!(audit_rows(&db).is_empty());
}

#[test]
fn bad_dates_and_formats_are_user_errors() {
    let db = Db::open_memory().unwrap();
    seed_entry(&db, "AC-SIR-0001", 1, Some("2026-03-01"), "Event");
    assert!(dispatch(&db, &["evidenceos", "chronology", "build", "--from", "not-a-date"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "build", "--to", "31/02/2026"]));
    assert!(dispatch(&db, &["evidenceos", "chronology", "build", "--format", "yaml"]));
    assert!(audit_rows(&db).is_empty());
}
