//! `sentinel propose` — for every NAMING-VIOLATION in a folder, work out the
//! compliant standard name. Dry run: prints an old -> new map, changes nothing.
//!
//! Resolution chain for a violating file: SIR token in the old name (preferred
//! prefix first), else SHA-256 match against the register, else UNRESOLVABLE.
//! The candidate name is built with `sir::standard_filename` from register
//! metadata. A name that would collide with an existing file (or with an
//! earlier proposal) is never proposed — it is marked COLLISION.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use evidence_core::{sir, Db, SirEntry};

use crate::files::{collect_files, FileInfo};
use crate::register::Register;

/// What the resolver settled on for one file, before any collision check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// (sir_no, compliant basename).
    Resolved { sir_no: String, name: String },
    /// Why no compliant name can be formed.
    Unresolvable { reason: String },
}

/// Printed outcome for one violating file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Proposal {
    Proposed { sir_no: String, name: String },
    Unresolvable { reason: String },
    Collision { sir_no: String, name: String, reason: String },
}

#[derive(Debug)]
pub(crate) struct ProposalItem {
    pub path: PathBuf,
    pub outcome: Proposal,
}

pub(crate) fn run(db: &Db, dir: &Path, prefix: &str) -> Result<()> {
    let files = collect_files(dir)?;
    let register = Register::load(db)?;
    let items = propose_items(&files, &register, prefix);

    println!("SENTINEL PROPOSE {} (dry run — no changes made)", dir.display());
    if items.is_empty() {
        println!("no naming violations found");
        return Ok(());
    }
    let (mut proposed, mut unresolvable, mut collision) = (0usize, 0usize, 0usize);
    for item in &items {
        match &item.outcome {
            Proposal::Proposed { name, .. } => {
                println!("{} -> {}", item.path.display(), name);
                proposed += 1;
            }
            Proposal::Unresolvable { reason } => {
                println!("{} -> UNRESOLVABLE ({reason})", item.path.display());
                unresolvable += 1;
            }
            Proposal::Collision { name, reason, .. } => {
                println!("{} -> COLLISION: {name} ({reason})", item.path.display());
                collision += 1;
            }
        }
    }
    println!("SUMMARY: {proposed} proposed | {unresolvable} unresolvable | {collision} collision");
    Ok(())
}

/// Map every violating file to a proposal outcome. In-place rename targets are
/// per-directory, so collision tracking works on full paths.
pub(crate) fn propose_items(
    files: &[FileInfo],
    register: &Register,
    prefix: &str,
) -> Vec<ProposalItem> {
    let mut taken: BTreeSet<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    let mut items = Vec::new();
    for f in files.iter().filter(|f| !f.compliant) {
        let outcome = match resolve_name(f, register, prefix) {
            Resolution::Unresolvable { reason } => Proposal::Unresolvable { reason },
            Resolution::Resolved { sir_no, name } => {
                let target = sibling(&f.path, &name);
                if taken.contains(&target) || target.exists() {
                    Proposal::Collision {
                        sir_no,
                        name,
                        reason: "target name already exists".into(),
                    }
                } else {
                    taken.insert(target);
                    Proposal::Proposed { sir_no, name }
                }
            }
        };
        items.push(ProposalItem {
            path: f.path.clone(),
            outcome,
        });
    }
    items
}

/// Resolve one file to its compliant name via the register. Pure lookup — no
/// collision logic, so `apply` can reuse it with its own target directories.
pub(crate) fn resolve_name(f: &FileInfo, register: &Register, prefix: &str) -> Resolution {
    if f.ext.is_empty() {
        return Resolution::Unresolvable {
            reason: "no file extension — cannot form a compliant name".into(),
        };
    }
    match pick_entry(f, register, prefix) {
        Some(e) => Resolution::Resolved {
            sir_no: e.sir_no.clone(),
            name: sir::standard_filename(
                e.doc_type.as_deref().unwrap_or("Document"),
                &e.sir_no,
                e.source_batch.as_deref(),
                e.doc_date.as_deref(),
                e.description.as_deref().unwrap_or("document"),
                &f.ext,
            ),
        },
        None => Resolution::Unresolvable {
            reason: format!(
                "no SIR in the filename and no sha256 match in the register — register the file (prefix {prefix}) first"
            ),
        },
    }
}

/// SIR token from the old name (tokens with the preferred prefix first, rest
/// in order of appearance), else SHA-256 lookup.
fn pick_entry<'r>(f: &FileInfo, register: &'r Register, prefix: &str) -> Option<&'r SirEntry> {
    let mut ordered: Vec<&String> = f.sirs.iter().filter(|s| has_prefix(s, prefix)).collect();
    ordered.extend(f.sirs.iter().filter(|s| !has_prefix(s, prefix)));
    ordered
        .into_iter()
        .find_map(|s| register.get(s))
        .or_else(|| register.find_by_sha(&f.sha256))
}

fn has_prefix(sir_no: &str, prefix: &str) -> bool {
    sir::parse_sir(sir_no)
        .map(|(p, _)| p == prefix)
        .unwrap_or(false)
}

pub(crate) fn sibling(path: &Path, name: &str) -> PathBuf {
    path.parent()
        .map(|p| p.join(name))
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use evidence_core::util;

    fn seed(
        db: &Db,
        sir_no: &str,
        seq: i64,
        doc_type: &str,
        doc_date: Option<&str>,
        batch: Option<&str>,
        description: &str,
        sha: Option<String>,
    ) {
        SirEntry {
            sir_no: sir_no.into(),
            seq,
            prefix: "AC".into(),
            doc_date: doc_date.map(str::to_string),
            doc_type: Some(doc_type.into()),
            description: Some(description.into()),
            source_batch: batch.map(str::to_string),
            sha256: sha,
            status: "draft".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
    }

    #[test]
    fn propose_builds_exact_standard_name_from_sha_match() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().unwrap();
        let bytes = b"icu discharge summary scan";
        std::fs::write(dir.path().join("icu discharge SCAN.PDF"), bytes).unwrap();
        seed(
            &db,
            "AC-SIR-0148",
            148,
            "Medical record",
            Some("2026-06-18"),
            None,
            "ICU discharge summary",
            Some(util::sha256_bytes(bytes)),
        );

        let files = collect_files(dir.path()).unwrap();
        let register = Register::load(&db).unwrap();
        let items = propose_items(&files, &register, "AC");

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].outcome,
            Proposal::Proposed {
                sir_no: "AC-SIR-0148".into(),
                name: "M.AC-SIR-0148 18.06.2026 ICU discharge summary.pdf".into(),
            }
        );
    }

    #[test]
    fn propose_uses_sir_from_old_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().unwrap();
        std::fs::write(dir.path().join("AC-SIR-0042 covering email.PDF"), b"mail").unwrap();
        seed(
            &db,
            "AC-SIR-0042",
            42,
            "Email",
            Some("2026-07-03"),
            Some("PB-10"),
            "Covering email",
            None,
        );

        let files = collect_files(dir.path()).unwrap();
        let register = Register::load(&db).unwrap();
        let items = propose_items(&files, &register, "AC");

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].outcome,
            Proposal::Proposed {
                sir_no: "AC-SIR-0042".into(),
                name: "E.AC-SIR-0042 PB-10-03.07.2026 Covering email.pdf".into(),
            }
        );
    }

    #[test]
    fn propose_marks_unresolvable_files() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().unwrap();
        std::fs::write(dir.path().join("mystery.bin"), b"???").unwrap();
        std::fs::write(dir.path().join("extensionless"), b"???").unwrap();

        let files = collect_files(dir.path()).unwrap();
        let register = Register::load(&db).unwrap();
        let items = propose_items(&files, &register, "AC");

        assert_eq!(items.len(), 2);
        assert!(matches!(
            &items[0].outcome,
            Proposal::Unresolvable { reason } if reason.contains("no file extension")
        ));
        assert!(matches!(
            &items[1].outcome,
            Proposal::Unresolvable { reason } if reason.contains("no SIR in the filename")
        ));
    }

    #[test]
    fn propose_never_collides_with_existing_or_planned_names() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().unwrap();
        seed(
            &db,
            "AC-SIR-0042",
            42,
            "Email",
            Some("2026-07-03"),
            Some("PB-10"),
            "Covering email",
            None,
        );
        // A file already sitting on the canonical name.
        std::fs::write(
            dir.path().join("E.AC-SIR-0042 PB-10-03.07.2026 Covering email.pdf"),
            b"filed",
        )
        .unwrap();
        // Two violations resolving to that same name.
        std::fs::write(dir.path().join("AC-SIR-0042 a.pdf"), b"a").unwrap();
        std::fs::write(dir.path().join("AC-SIR-0042 b.pdf"), b"b").unwrap();

        let files = collect_files(dir.path()).unwrap();
        let register = Register::load(&db).unwrap();
        let items = propose_items(&files, &register, "AC");

        assert_eq!(items.len(), 2);
        assert!(matches!(
            &items[0].outcome,
            Proposal::Collision { name, .. }
                if name == "E.AC-SIR-0042 PB-10-03.07.2026 Covering email.pdf"
        ));
        assert!(matches!(&items[1].outcome, Proposal::Collision { .. }));
    }
}
