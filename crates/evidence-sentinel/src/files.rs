//! Filesystem walk shared by all sentinel commands: regular, non-hidden files
//! under a directory, each classified against the SAS naming rule.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use evidence_core::{sir, util};
use regex::Regex;
use walkdir::WalkDir;

/// The SAS standard evidence filename rule, enforced on basenames:
/// `E.AC-SIR-0092 PB-10-03.07.2026 Covering email.pdf`
pub(crate) const NAMING_RULE: &str = r"^[A-Z]\.[A-Z]{1,6}-SIR-\d{4,} .+\.[a-z0-9]+$";

/// One regular file found under the scanned directory.
#[derive(Debug)]
pub(crate) struct FileInfo {
    /// Full path as walked (rooted at the scanned DIR argument).
    pub path: PathBuf,
    /// Validated SIR tokens carried by the basename, in order of appearance.
    pub sirs: Vec<String>,
    /// SHA-256 hex digest of the file's bytes.
    pub sha256: String,
    /// Lowercased extension without the dot; empty when the file has none.
    pub ext: String,
    /// Whether the basename satisfies [`NAMING_RULE`].
    pub compliant: bool,
}

/// Walk `dir` (recursively, skipping hidden files and directories) and classify
/// every regular file. Output is sorted by path for deterministic reports.
pub(crate) fn collect_files(dir: &Path) -> Result<Vec<FileInfo>> {
    if !dir.is_dir() {
        bail!("not a directory: {}", dir.display());
    }
    let rule = Regex::new(NAMING_RULE).expect("naming rule compiles");
    let mut out = Vec::new();
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let name = entry.file_name().to_string_lossy().into_owned();
        let sirs = sir::extract_sir_refs(&name)
            .into_iter()
            .filter(|s| sir::parse_sir(s).is_some())
            .collect();
        let sha256 = util::sha256_file(&path)?;
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let compliant = rule.is_match(&name);
        out.push(FileInfo {
            path,
            sirs,
            sha256,
            ext,
            compliant,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Hidden = dot-entry, but never the scanned root itself.
fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry.depth() > 0 && entry.file_name().to_string_lossy().starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_regular_files_and_skips_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("E.AC-SIR-0001 01.01.2026 Fine.pdf"), b"a").unwrap();
        std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("config"), b"junk").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("loose.txt"), b"b").unwrap();

        let files = collect_files(root).unwrap();
        let names: Vec<&str> = files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["E.AC-SIR-0001 01.01.2026 Fine.pdf", "loose.txt"]);
        assert!(files[0].compliant);
        assert!(!files[1].compliant);
        assert_eq!(files[0].sirs, vec!["AC-SIR-0001"]);
        assert_eq!(files[0].ext, "pdf");
        assert_eq!(files[0].sha256.len(), 64);
    }

    #[test]
    fn rejects_a_missing_directory() {
        let err = collect_files(Path::new("/nonexistent/sentinel-dir")).unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }
}
