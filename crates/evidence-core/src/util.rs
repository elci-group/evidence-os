//! Small deterministic helpers shared by all modules: hashing and UK dates.
//!
//! Convention: dates are *stored* as ISO `yyyy-mm-dd` and *displayed* as
//! British `dd/mm/yyyy`.

use std::fs;
use std::path::Path;

use anyhow::Context;
use chrono::{Local, NaiveDate};
use regex::Regex;
use sha2::{Digest, Sha256};

/// SHA-256 hex digest of a file — used for duplicate detection.
pub fn sha256_file(path: impl AsRef<Path>) -> anyhow::Result<String> {
    let bytes = fs::read(path.as_ref())
        .with_context(|| format!("reading {}", path.as_ref().display()))?;
    Ok(hex_lower(&Sha256::digest(&bytes)))
}

/// SHA-256 hex digest of in-memory bytes.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

fn hex_lower(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Current local time as ISO-ish `yyyy-mm-ddTHH:MM:SS`.
pub fn now_iso() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// Today, ISO `yyyy-mm-dd`.
pub fn today_iso() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

/// Parse a British date (`03/07/2026`, `3/7/26`, `03.07.2026`, `03-07-2026`)
/// into ISO `2026-07-03`. Two-digit years map to 20xx. Returns None if the
/// numbers are not a valid calendar date.
pub fn iso_from_uk_date(s: &str) -> Option<String> {
    let re = Regex::new(r"^(\d{1,2})[/.\-](\d{1,2})[/.\-](\d{2}|\d{4})$").unwrap();
    let c = re.captures(s.trim())?;
    let day: u32 = c[1].parse().ok()?;
    let month: u32 = c[2].parse().ok()?;
    let mut year: i32 = c[3].parse().ok()?;
    if year < 100 {
        year += 2000;
    }
    NaiveDate::from_ymd_opt(year, month, day).map(|d| d.format("%Y-%m-%d").to_string())
}

/// `2026-07-03` -> `03/07/2026`.
pub fn uk_from_iso(iso: &str) -> Option<String> {
    let d = NaiveDate::parse_from_str(iso.trim(), "%Y-%m-%d").ok()?;
    Some(d.format("%d/%m/%Y").to_string())
}

/// `2026-07-03` -> `03.07.2026` (standard filename style).
pub fn dmy_dots_from_iso(iso: &str) -> Option<String> {
    let d = NaiveDate::parse_from_str(iso.trim(), "%Y-%m-%d").ok()?;
    Some(d.format("%d.%m.%Y").to_string())
}

/// Find the first British-style date in free text, returned as ISO.
/// Handles `03/07/2026`, `3.7.26`, `03-07-26`.
pub fn extract_date_from_text(text: &str) -> Option<String> {
    let re = Regex::new(r"\b\d{1,2}[/.\-]\d{1,2}[/.\-]\d{2,4}\b").unwrap();
    let m = re.find(text)?;
    iso_from_uk_date(m.as_str())
}

/// Add `days` to an ISO date. Returns None on bad input.
pub fn add_days(iso: &str, days: i64) -> Option<String> {
    let d = NaiveDate::parse_from_str(iso.trim(), "%Y-%m-%d").ok()?;
    Some(
        d.checked_add_signed(chrono::Duration::days(days))?
            .format("%Y-%m-%d")
            .to_string(),
    )
}

/// Whole days from `from` to `to` (both ISO). Negative if `to` is earlier.
pub fn days_between(from: &str, to: &str) -> Option<i64> {
    let a = NaiveDate::parse_from_str(from.trim(), "%Y-%m-%d").ok()?;
    let b = NaiveDate::parse_from_str(to.trim(), "%Y-%m-%d").ok()?;
    Some((b - a).num_days())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uk_dates_round_trip() {
        assert_eq!(iso_from_uk_date("03/07/2026"), Some("2026-07-03".into()));
        assert_eq!(iso_from_uk_date("3.7.26"), Some("2026-07-03".into()));
        assert_eq!(iso_from_uk_date("03-07-26"), Some("2026-07-03".into()));
        assert_eq!(iso_from_uk_date("31/02/2026"), None);
        assert_eq!(uk_from_iso("2026-07-03"), Some("03/07/2026".into()));
        assert_eq!(dmy_dots_from_iso("2026-07-03"), Some("03.07.2026".into()));
    }

    #[test]
    fn extracts_dates_from_text() {
        assert_eq!(
            extract_date_from_text("ICU discharge summary 18.06.2026 final"),
            Some("2026-06-18".into())
        );
        assert_eq!(extract_date_from_text("no date here"), None);
    }

    #[test]
    fn date_arithmetic() {
        assert_eq!(add_days("2026-07-01", 14), Some("2026-07-15".into()));
        assert_eq!(days_between("2026-07-01", "2026-07-15"), Some(14));
        assert_eq!(days_between("2026-07-15", "2026-07-01"), Some(-14));
    }

    #[test]
    fn hashes_files_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, b"hello").unwrap();
        let h1 = sha256_file(&p).unwrap();
        let h2 = sha256_bytes(b"hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }
}
