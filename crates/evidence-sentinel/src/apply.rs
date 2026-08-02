//! `sentinel apply` — carry out the proposals: rename violating files to their
//! compliant names (or move them under `--move-to`), update the register and
//! audit every change.
//!
//! Atomic-ish contract: all operations are precomputed and validated up front.
//! If any target already exists (on disk or as another planned target) the
//! command aborts with zero changes — no renames, no register updates, no
//! audit rows.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use evidence_core::{audit::audit, Db};
use rusqlite::params;

use crate::files::collect_files;
use crate::propose::{resolve_name, Resolution};
use crate::register::Register;
use crate::ACTOR;

/// One rename/move that was actually performed.
#[derive(Debug)]
pub(crate) struct AppliedOp {
    pub sir_no: String,
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Default)]
pub(crate) struct ApplyOutcome {
    pub done: Vec<AppliedOp>,
    /// Files left in place, with the reason (unresolvable).
    pub skipped: Vec<(PathBuf, String)>,
}

pub(crate) fn run(db: &Db, dir: &Path, move_to: Option<&Path>) -> Result<()> {
    let outcome = apply(db, dir, move_to)?;

    println!("SENTINEL APPLY {}", dir.display());
    if let Some(mt) = move_to {
        println!("move-to: {}", mt.display());
    }
    if outcome.done.is_empty() && outcome.skipped.is_empty() {
        println!("nothing to do — no naming violations found");
        return Ok(());
    }
    let verb_past = if move_to.is_some() { "MOVED" } else { "RENAMED" };
    for op in &outcome.done {
        println!("  {verb_past} {} -> {}", op.from.display(), op.to.display());
    }
    for (path, reason) in &outcome.skipped {
        println!("  SKIP {} ({reason})", path.display());
    }
    println!(
        "MANIFEST: {} {} | {} skipped",
        outcome.done.len(),
        verb_past.to_lowercase(),
        outcome.skipped.len()
    );
    Ok(())
}

pub(crate) fn apply(db: &Db, dir: &Path, move_to: Option<&Path>) -> Result<ApplyOutcome> {
    let files = collect_files(dir)?;
    let register = Register::load(db)?;

    // Plan everything before touching the filesystem. NOTE: apply has no
    // --prefix flag, so SIR tokens in names prefer the default AC register.
    let mut planned: Vec<AppliedOp> = Vec::new();
    let mut skipped: Vec<(PathBuf, String)> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let mut targets: BTreeSet<PathBuf> = BTreeSet::new();

    for f in files.iter().filter(|f| !f.compliant) {
        match resolve_name(f, &register, "AC") {
            Resolution::Unresolvable { reason } => skipped.push((f.path.clone(), reason)),
            Resolution::Resolved { sir_no, name } => {
                let to = match move_to {
                    Some(mt) => mt.join(&name),
                    None => crate::propose::sibling(&f.path, &name),
                };
                if to.exists() {
                    blockers.push(format!(
                        "{} -> {} — target already exists",
                        f.path.display(),
                        to.display()
                    ));
                } else if !targets.insert(to.clone()) {
                    blockers.push(format!(
                        "{} -> {} — duplicate target in this run",
                        f.path.display(),
                        to.display()
                    ));
                } else {
                    planned.push(AppliedOp {
                        sir_no,
                        from: f.path.clone(),
                        to,
                    });
                }
            }
        }
    }

    if !blockers.is_empty() {
        bail!(
            "sentinel apply aborted — zero changes made:\n  {}",
            blockers.join("\n  ")
        );
    }

    if !planned.is_empty() {
        if let Some(mt) = move_to {
            std::fs::create_dir_all(mt).with_context(|| format!("creating {}", mt.display()))?;
        }
    }

    let verb = if move_to.is_some() { "move" } else { "rename" };
    let mut outcome = ApplyOutcome {
        skipped,
        ..Default::default()
    };
    for op in planned {
        rename_no_overwrite(&op.from, &op.to)?;
        audit(
            &db.conn,
            ACTOR,
            verb,
            &format!(
                "{} -> {} [{}]",
                op.from.display(),
                op.to.display(),
                op.sir_no
            ),
        )?;
        db.conn.execute(
            "UPDATE sir_entries SET file_path = ?1 WHERE sir_no = ?2",
            params![op.to.to_string_lossy().as_ref(), op.sir_no],
        )?;
        outcome.done.push(op);
    }
    Ok(outcome)
}

/// `fs::rename` that refuses to overwrite, with a copy+remove fallback when
/// source and target live on different filesystems (EXDEV, raw OS error 18).
fn rename_no_overwrite(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        bail!("refusing to overwrite {}", to.display());
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(18) => {
            std::fs::copy(from, to)
                .with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
            std::fs::remove_file(from)
                .with_context(|| format!("removing {}", from.display()))?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| {
            format!("renaming {} -> {}", from.display(), to.display())
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evidence_core::SirEntry;

    fn seed(db: &Db) {
        SirEntry {
            sir_no: "AC-SIR-0042".into(),
            seq: 42,
            prefix: "AC".into(),
            doc_date: Some("2026-07-03".into()),
            doc_type: Some("Email".into()),
            description: Some("Covering email".into()),
            source_batch: Some("PB-10".into()),
            status: "draft".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
    }

    const STANDARD: &str = "E.AC-SIR-0042 PB-10-03.07.2026 Covering email.pdf";

    #[test]
    fn apply_renames_updates_register_and_audits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = Db::open_memory().unwrap();
        seed(&db);
        let src = root.join("AC-SIR-0042 covering email.PDF");
        std::fs::write(&src, b"mail").unwrap();

        let outcome = apply(&db, root, None).unwrap();

        let expected = root.join(STANDARD);
        assert_eq!(outcome.done.len(), 1);
        assert_eq!(outcome.done[0].to, expected);
        assert!(!src.exists());
        assert!(expected.exists());

        // Register row now points at the renamed file.
        let entry = SirEntry::get(&db.conn, "AC-SIR-0042").unwrap().unwrap();
        assert_eq!(
            entry.file_path.as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );

        // Exactly one audit row, recording both paths and the SIR.
        let (actor, action, detail): (String, String, String) = db
            .conn
            .query_row(
                "SELECT actor, action, detail FROM audit_log",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(actor, "sentinel");
        assert_eq!(action, "rename");
        assert!(detail.contains("AC-SIR-0042 covering email.PDF"));
        assert!(detail.contains(STANDARD));
        assert!(detail.contains("[AC-SIR-0042]"));
    }

    #[test]
    fn apply_moves_files_into_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = Db::open_memory().unwrap();
        seed(&db);
        let src = root.join("AC-SIR-0042 covering email.PDF");
        std::fs::write(&src, b"mail").unwrap();
        let filing = root.join("filing");

        let outcome = apply(&db, root, Some(&filing)).unwrap();

        let expected = filing.join(STANDARD);
        assert_eq!(outcome.done.len(), 1);
        assert!(!src.exists());
        assert!(expected.exists());
        let entry = SirEntry::get(&db.conn, "AC-SIR-0042").unwrap().unwrap();
        assert_eq!(
            entry.file_path.as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );
        let action: String = db
            .conn
            .query_row("SELECT action FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(action, "move");
    }

    #[test]
    fn apply_aborts_on_existing_target_with_zero_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = Db::open_memory().unwrap();
        seed(&db);
        let src = root.join("AC-SIR-0042 covering email.PDF");
        std::fs::write(&src, b"new bytes").unwrap();
        let target = root.join(STANDARD);
        std::fs::write(&target, b"already filed").unwrap();

        let err = apply(&db, root, None).unwrap_err();
        assert!(err.to_string().contains("aborted"), "unexpected error: {err}");

        // Zero changes: source untouched, target untouched, no audit, no update.
        assert!(src.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"already filed");
        let audits: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(audits, 0);
        let entry = SirEntry::get(&db.conn, "AC-SIR-0042").unwrap().unwrap();
        assert!(entry.file_path.is_none());
    }

    #[test]
    fn apply_skips_unresolvable_and_leaves_compliant_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = Db::open_memory().unwrap();
        let compliant = root.join("E.AC-SIR-0001 01.01.2026 Fine.pdf");
        std::fs::write(&compliant, b"ok").unwrap();
        let mystery = root.join("mystery.pdf");
        std::fs::write(&mystery, b"?").unwrap();

        let outcome = apply(&db, root, None).unwrap();

        assert!(outcome.done.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].0, mystery);
        assert!(compliant.exists());
        assert!(mystery.exists());
    }
}
