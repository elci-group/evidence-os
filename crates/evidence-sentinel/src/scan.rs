//! `sentinel scan` — read-only audit of a folder against the naming rule and
//! the Master Chronology. Report only; nothing is written, nothing is renamed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use evidence_core::Db;

use crate::files::{collect_files, FileInfo};
use crate::register::Register;

/// Grouped findings of one scan. Counts in the printed summary are: files for
/// NAMING-VIOLATION, (file, SIR) pairs for UNKNOWN-SIR, groups for
/// DUPLICATE-SIR / EXACT-DUPLICATE / REVISED-VERSION, register rows for
/// MISSING-FILE.
#[derive(Debug, Default)]
pub(crate) struct ScanReport {
    pub files_scanned: usize,
    /// Files whose basename breaks the naming rule.
    pub naming_violations: Vec<PathBuf>,
    /// (file, SIR) where the SIR in the name is absent from the register.
    pub unknown_sirs: Vec<(PathBuf, String)>,
    /// SIR -> files, when two or more files carry the same SIR.
    pub duplicate_sirs: Vec<(String, Vec<PathBuf>)>,
    /// sha256 -> files, when two or more files hold identical bytes.
    pub exact_duplicates: Vec<(String, Vec<PathBuf>)>,
    /// (SIR, ext) -> files: same SIR, same extension, differing content.
    /// Needs a human decision on which revision is authoritative.
    pub revised_versions: Vec<(String, String, Vec<PathBuf>)>,
    /// (sir_no, stored path) for register rows whose file is gone from disk.
    pub missing_files: Vec<(String, String)>,
    /// Whether the MISSING-FILE check ran at all (only when DIR plausibly
    /// contains the case files).
    pub missing_check_active: bool,
}

pub(crate) fn run(db: &Db, dir: &Path) -> Result<()> {
    let report = scan(db, dir)?;
    print_report(&report, dir);
    Ok(())
}

pub(crate) fn scan(db: &Db, dir: &Path) -> Result<ScanReport> {
    let files = collect_files(dir)?;
    let register = Register::load(db)?;
    let mut report = ScanReport {
        files_scanned: files.len(),
        ..Default::default()
    };

    let mut by_sir: BTreeMap<&str, Vec<&FileInfo>> = BTreeMap::new();
    let mut by_sha: BTreeMap<&str, Vec<&FileInfo>> = BTreeMap::new();
    let mut by_sir_ext: BTreeMap<(&str, &str), Vec<&FileInfo>> = BTreeMap::new();

    for f in &files {
        if !f.compliant {
            report.naming_violations.push(f.path.clone());
        }
        for s in &f.sirs {
            if register.get(s).is_none() {
                report.unknown_sirs.push((f.path.clone(), s.clone()));
            }
            by_sir.entry(s.as_str()).or_default().push(f);
            by_sir_ext.entry((s.as_str(), f.ext.as_str())).or_default().push(f);
        }
        by_sha.entry(f.sha256.as_str()).or_default().push(f);
    }

    for (s, group) in &by_sir {
        if group.len() >= 2 {
            report
                .duplicate_sirs
                .push((s.to_string(), paths_of(group)));
        }
    }
    for (sha, group) in &by_sha {
        if group.len() >= 2 {
            report
                .exact_duplicates
                .push((sha.to_string(), paths_of(group)));
        }
    }
    for ((s, ext), group) in &by_sir_ext {
        let distinct: BTreeSet<&str> = group.iter().map(|f| f.sha256.as_str()).collect();
        if group.len() >= 2 && distinct.len() >= 2 {
            report
                .revised_versions
                .push((s.to_string(), ext.to_string(), paths_of(group)));
        }
    }

    // MISSING-FILE: only meaningful when DIR is (part of) the case folder, so
    // the check activates only if at least one registered path belongs to DIR.
    let mut belongs_any = false;
    let mut missing = Vec::new();
    for e in register.entries() {
        let Some(stored) = e
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        else {
            continue;
        };
        let Some(candidate) = resolve_in_dir(stored, dir) else {
            continue;
        };
        belongs_any = true;
        if !candidate.exists() {
            missing.push((e.sir_no.clone(), stored.to_string()));
        }
    }
    if belongs_any {
        report.missing_check_active = true;
        report.missing_files = missing;
    }

    Ok(report)
}

/// Resolve a stored `file_path` against the scanned DIR. Absolute paths belong
/// only when they sit under DIR; relative paths are resolved against DIR.
/// Returns None when the row is unrelated to DIR.
fn resolve_in_dir(stored: &str, dir: &Path) -> Option<PathBuf> {
    let p = Path::new(stored);
    if p.is_absolute() {
        p.starts_with(dir).then(|| p.to_path_buf())
    } else {
        Some(dir.join(p))
    }
}

fn paths_of(group: &[&FileInfo]) -> Vec<PathBuf> {
    group.iter().map(|f| f.path.clone()).collect()
}

fn print_report(report: &ScanReport, dir: &Path) {
    println!("SENTINEL SCAN {}", dir.display());
    println!("{} file(s) scanned", report.files_scanned);
    println!();

    if !report.naming_violations.is_empty() {
        println!("NAMING-VIOLATION ({})", report.naming_violations.len());
        for p in &report.naming_violations {
            println!("  {}", p.display());
        }
        println!();
    }
    if !report.unknown_sirs.is_empty() {
        println!("UNKNOWN-SIR ({})", report.unknown_sirs.len());
        for (p, s) in &report.unknown_sirs {
            println!("  {} — {s} not in the register", p.display());
        }
        println!();
    }
    if !report.duplicate_sirs.is_empty() {
        println!("DUPLICATE-SIR ({})", report.duplicate_sirs.len());
        for (s, files) in &report.duplicate_sirs {
            println!("  {s}:");
            for p in files {
                println!("    {}", p.display());
            }
        }
        println!();
    }
    if !report.exact_duplicates.is_empty() {
        println!("EXACT-DUPLICATE ({})", report.exact_duplicates.len());
        for (sha, files) in &report.exact_duplicates {
            println!("  sha256 {}…:", &sha[..12]);
            for p in files {
                println!("    {}", p.display());
            }
        }
        println!();
    }
    if !report.revised_versions.is_empty() {
        println!(
            "REVISED-VERSION ({}) — same SIR, same extension, different content; human decision required",
            report.revised_versions.len()
        );
        for (s, ext, files) in &report.revised_versions {
            println!("  {s} (.{ext}):");
            for p in files {
                println!("    {}", p.display());
            }
        }
        println!();
    }
    if report.missing_check_active && !report.missing_files.is_empty() {
        println!("MISSING-FILE ({})", report.missing_files.len());
        for (sir_no, stored) in &report.missing_files {
            println!("  {sir_no} — registered path gone: {stored}");
        }
        println!();
    }

    println!(
        "SUMMARY: {} scanned | {} naming-violation | {} unknown-sir | {} duplicate-sir | {} exact-duplicate | {} revised-version | {} missing-file",
        report.files_scanned,
        report.naming_violations.len(),
        report.unknown_sirs.len(),
        report.duplicate_sirs.len(),
        report.exact_duplicates.len(),
        report.revised_versions.len(),
        report.missing_files.len(),
    );
    if !report.missing_check_active {
        println!("note: MISSING-FILE check skipped — DIR does not appear to contain the case files");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evidence_core::SirEntry;

    fn seed(db: &Db, sir_no: &str, seq: i64, file_path: Option<String>) {
        SirEntry {
            sir_no: sir_no.into(),
            seq,
            prefix: "AC".into(),
            file_path,
            status: "draft".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
    }

    #[test]
    fn scan_finds_every_violation_class() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let db = Db::open_memory().unwrap();

        // Compliant and known — no findings.
        std::fs::write(
            root.join("E.AC-SIR-0092 PB-10-03.07.2026 Covering email.pdf"),
            b"email body",
        )
        .unwrap();
        seed(&db, "AC-SIR-0092", 92, None);

        // NAMING-VIOLATION.
        std::fs::write(root.join("random notes.txt"), b"notes").unwrap();

        // UNKNOWN-SIR: compliant shape, SIR absent from the register.
        std::fs::write(root.join("E.AC-SIR-9999 01.01.2026 Ghost.pdf"), b"ghost").unwrap();

        // DUPLICATE-SIR + REVISED-VERSION: same SIR, same ext, different bytes.
        std::fs::write(root.join("E.AC-SIR-0007 01.02.2026 Statement v1.pdf"), b"v1").unwrap();
        std::fs::write(
            root.join("E.AC-SIR-0007 02.02.2026 Statement v2.pdf"),
            b"v2 different",
        )
        .unwrap();
        seed(&db, "AC-SIR-0007", 7, None);

        // EXACT-DUPLICATE: identical bytes under two names (also DUPLICATE-SIR).
        std::fs::write(root.join("E.AC-SIR-0003 01.01.2026 Original.pdf"), b"same bytes").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("E.AC-SIR-0003 01.01.2026 Copy.pdf"), b"same bytes")
            .unwrap();
        seed(&db, "AC-SIR-0003", 3, None);

        // MISSING-FILE: registered path inside DIR, absent on disk.
        let gone = root.join("E.AC-SIR-0200 01.01.2026 Gone.pdf");
        seed(
            &db,
            "AC-SIR-0200",
            200,
            Some(gone.to_string_lossy().into_owned()),
        );

        let report = scan(&db, root).unwrap();

        assert_eq!(report.files_scanned, 7);

        assert_eq!(report.naming_violations.len(), 1);
        assert!(report.naming_violations[0].ends_with("random notes.txt"));

        assert_eq!(report.unknown_sirs.len(), 1);
        assert_eq!(report.unknown_sirs[0].1, "AC-SIR-9999");

        assert_eq!(report.duplicate_sirs.len(), 2);
        assert_eq!(report.duplicate_sirs[0].0, "AC-SIR-0003");
        assert_eq!(report.duplicate_sirs[1].0, "AC-SIR-0007");
        assert_eq!(report.duplicate_sirs[1].1.len(), 2);

        assert_eq!(report.exact_duplicates.len(), 1);
        assert_eq!(report.exact_duplicates[0].1.len(), 2);

        assert_eq!(report.revised_versions.len(), 1);
        assert_eq!(report.revised_versions[0].0, "AC-SIR-0007");
        assert_eq!(report.revised_versions[0].1, "pdf");
        assert_eq!(report.revised_versions[0].2.len(), 2);

        assert!(report.missing_check_active);
        assert_eq!(
            report.missing_files,
            vec![(
                "AC-SIR-0200".to_string(),
                gone.to_string_lossy().into_owned()
            )]
        );
    }

    #[test]
    fn missing_check_skipped_when_dir_is_unrelated() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let db = Db::open_memory().unwrap();
        // Register rows point at a different folder entirely.
        seed(
            &db,
            "AC-SIR-0001",
            1,
            Some(
                other
                    .path()
                    .join("E.AC-SIR-0001 01.01.2026 Elsewhere.pdf")
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
        std::fs::write(dir.path().join("loose.pdf"), b"x").unwrap();

        let report = scan(&db, dir.path()).unwrap();
        assert!(!report.missing_check_active);
        assert!(report.missing_files.is_empty());
    }
}
