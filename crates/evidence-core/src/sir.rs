//! SIR numbers and standard evidence filenames.
//!
//! SIR format: `{PREFIX}-SIR-{seq:04}` (e.g. `AC-SIR-0147`). Sequences are
//! per-prefix and stored in the `meta` table, so allocation survives restarts.
//!
//! Standard filename format (per SAS convention):
//!   `E.AC-SIR-0092 PB-10-03.07.2026 Covering email.pdf`
//!   `M.AC-SIR-0148 18.06.2026 ICU discharge summary.pdf`
//! i.e. `{LETTER}.{SIR} [{BATCH}-]{DD.MM.YYYY} {Description}.{ext}`

use anyhow::{anyhow, Result};
use regex::Regex;
use rusqlite::Connection;

use crate::util;

/// Allocate the next SIR number for `prefix`, persisting the counter.
pub fn allocate_sir(conn: &Connection, prefix: &str) -> Result<String> {
    let key = format!("sir_seq_{prefix}");
    let next: i64 = conn
        .query_row(
            "INSERT INTO meta(key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1
             RETURNING CAST(value AS INTEGER)",
            [key],
            |r| r.get(0),
        )
        .map_err(|e| anyhow!("allocating SIR for {prefix}: {e}"))?;
    Ok(format!("{prefix}-SIR-{next:04}"))
}

/// Peek at the next SIR without consuming it.
pub fn peek_next_sir(conn: &Connection, prefix: &str) -> Result<String> {
    let key = format!("sir_seq_{prefix}");
    let cur: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .ok();
    let next: i64 = cur.and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
    Ok(format!("{prefix}-SIR-{next:04}"))
}

/// Split `AC-SIR-0147` into (`AC`, 147). Returns None for malformed input.
pub fn parse_sir(s: &str) -> Option<(String, u32)> {
    let re = Regex::new(r"^([A-Z]{1,6})-SIR-(\d{4,})$").unwrap();
    let c = re.captures(s.trim())?;
    Some((c[1].to_string(), c[2].parse().ok()?))
}

/// Find SIR references (e.g. AC-SIR-0147) inside free text.
pub fn extract_sir_refs(text: &str) -> Vec<String> {
    let re = Regex::new(r"[A-Z]{1,6}-SIR-\d{4,}").unwrap();
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

/// Single-letter code used at the start of a standard filename.
pub fn doc_type_letter(doc_type: &str) -> char {
    let t = doc_type.to_lowercase();
    let rules: &[(&[&str], char)] = &[
        (&["email", "e-mail"], 'E'),
        (&["medical", "gp", "hospital", "icu", "fit note", "sick", "physio"], 'M'),
        (&["letter"], 'L'),
        (&["image", "photo", "scan"], 'I'),
        (&["witness", "statement"], 'W'),
        (&["policy", "procedure", "handbook"], 'P'),
        (&["text", "note", "minutes"], 'T'),
        (&["grievance"], 'G'),
        (&["order", "tribunal", "et1", "et3"], 'C'),
    ];
    for (keys, letter) in rules {
        if keys.iter().any(|k| t.contains(k)) {
            return *letter;
        }
    }
    'D'
}

/// Build the standard evidence filename.
/// `date_iso` is yyyy-mm-dd; `ext` without a leading dot.
pub fn standard_filename(
    doc_type: &str,
    sir_no: &str,
    batch: Option<&str>,
    date_iso: Option<&str>,
    description: &str,
    ext: &str,
) -> String {
    let letter = doc_type_letter(doc_type);
    let date = date_iso
        .and_then(util::dmy_dots_from_iso)
        .unwrap_or_else(|| "undated".to_string());
    let desc = sanitize_description(description);
    let batch_part = batch
        .filter(|b| !b.trim().is_empty())
        .map(|b| format!("{}-", b.trim()))
        .unwrap_or_default();
    let stem = format!("{letter}.{sir_no} {batch_part}{date} {desc}");
    let ext = ext.trim_start_matches('.');
    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{}", ext.to_lowercase())
    }
}

/// Remove characters that are illegal or troublesome in filenames.
pub fn sanitize_description(desc: &str) -> String {
    let re = Regex::new(r#"[/\\:*?"<>|\r\n]+"#).unwrap();
    let cleaned = re.replace_all(desc, " ");
    let ws = Regex::new(r"\s+").unwrap();
    ws.replace_all(cleaned.trim(), " ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn allocates_sequential_per_prefix() {
        let db = Db::open_memory().unwrap();
        assert_eq!(allocate_sir(&db.conn, "AC").unwrap(), "AC-SIR-0001");
        assert_eq!(allocate_sir(&db.conn, "AC").unwrap(), "AC-SIR-0002");
        assert_eq!(allocate_sir(&db.conn, "IG").unwrap(), "IG-SIR-0001");
        assert_eq!(peek_next_sir(&db.conn, "AC").unwrap(), "AC-SIR-0003");
        // peek must not consume
        assert_eq!(peek_next_sir(&db.conn, "AC").unwrap(), "AC-SIR-0003");
    }

    #[test]
    fn parses_sir_refs() {
        assert_eq!(parse_sir("AC-SIR-0147"), Some(("AC".into(), 147)));
        assert_eq!(parse_sir("KGS-SIR-0009"), Some(("KGS".into(), 9)));
        assert!(parse_sir("not a sir").is_none());
        assert_eq!(
            extract_sir_refs("see AC-SIR-0043 and IG-SIR-0002."),
            vec!["AC-SIR-0043", "IG-SIR-0002"]
        );
    }

    #[test]
    fn builds_standard_filenames() {
        assert_eq!(
            standard_filename(
                "Email",
                "AC-SIR-0092",
                Some("PB-10"),
                Some("2026-07-03"),
                "Covering email",
                "pdf"
            ),
            "E.AC-SIR-0092 PB-10-03.07.2026 Covering email.pdf"
        );
        assert_eq!(
            standard_filename(
                "Medical record",
                "AC-SIR-0148",
                None,
                Some("2026-06-18"),
                "ICU discharge summary",
                "PDF"
            ),
            "M.AC-SIR-0148 18.06.2026 ICU discharge summary.pdf"
        );
        assert_eq!(
            standard_filename("Letter", "AC-SIR-0001", None, None, "odd: name?", "docx"),
            "L.AC-SIR-0001 undated odd name.docx"
        );
    }
}
