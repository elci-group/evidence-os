//! `chronology check`: deterministic integrity scan over `sir_entries`.
//! Five rules, run in fixed order, findings sorted within each rule — same
//! register in, same findings out. Read-only: nothing is persisted.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use evidence_core::sir::extract_sir_refs;
use evidence_core::util::{days_between, uk_from_iso};
use evidence_core::{Db, SirEntry};
use regex::Regex;
use rusqlite::Connection;

/// Gaps longer than this between consecutive dated entries are reported.
const GAP_DAYS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    /// Machine rule name, e.g. "conflicting-date".
    pub rule: &'static str,
    /// What it concerns — SIR(s).
    pub subject: String,
    /// Human-readable explanation.
    pub detail: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {} — {}", self.rule, self.subject, self.detail)
    }
}

/// Run all five rules over the register. Deterministic: rules run in a fixed
/// order and findings are sorted within each rule.
pub(crate) fn run_checks(conn: &Connection) -> Result<Vec<Finding>> {
    let entries = SirEntry::list(conn)?; // ordered by prefix, seq
    let mut findings = Vec::new();
    findings.extend(check_conflicting_dates(&entries));
    findings.extend(check_missing_refs(&entries));
    findings.extend(check_gaps(&entries));
    findings.extend(check_orphan_attachments(&entries));
    findings.extend(check_impossible_sequences(&entries));
    Ok(findings)
}

/// Rule 1: identical normalized descriptions with different doc_dates.
/// Normalization: lowercase, trim, collapse whitespace, strip embedded dates.
fn check_conflicting_dates(entries: &[SirEntry]) -> Vec<Finding> {
    let mut groups: BTreeMap<String, Vec<&SirEntry>> = BTreeMap::new();
    for e in entries {
        let Some(desc) = e.description.as_deref() else { continue };
        let norm = normalize_description(desc);
        if norm.is_empty() {
            continue;
        }
        groups.entry(norm).or_default().push(e);
    }
    let mut out = Vec::new();
    for (norm, group) in groups {
        if group.len() < 2 {
            continue;
        }
        let mut dates: Vec<&str> = group
            .iter()
            .filter_map(|e| e.doc_date.as_deref())
            .collect();
        dates.sort_unstable();
        dates.dedup();
        if dates.len() < 2 {
            continue;
        }
        let sirs: Vec<&str> = group.iter().map(|e| e.sir_no.as_str()).collect();
        let brit: Vec<String> = dates.iter().map(|d| brit(d)).collect();
        out.push(Finding {
            rule: "conflicting-date",
            subject: sirs.join(", "),
            detail: format!(
                "same description \"{norm}\" recorded with different dates: {}",
                brit.join(" vs ")
            ),
        });
    }
    out.sort_by(|a, b| a.subject.cmp(&b.subject));
    out
}

/// Lowercase, strip embedded British/ISO dates, collapse whitespace, trim.
fn normalize_description(desc: &str) -> String {
    let lower = desc.to_lowercase();
    let dates =
        Regex::new(r"\b\d{1,2}[/.\-]\d{1,2}[/.\-]\d{2,4}\b|\b\d{4}-\d{2}-\d{2}\b").unwrap();
    let stripped = dates.replace_all(&lower, " ");
    let ws = Regex::new(r"\s+").unwrap();
    ws.replace_all(stripped.trim(), " ").to_string()
}

/// Rule 2: description/sender mentions an AC-SIR-#### style reference that
/// does not exist in the register.
fn check_missing_refs(entries: &[SirEntry]) -> Vec<Finding> {
    let known: HashSet<&str> = entries.iter().map(|e| e.sir_no.as_str()).collect();
    let mut out = Vec::new();
    for e in entries {
        let text = format!(
            "{} {}",
            e.description.as_deref().unwrap_or(""),
            e.sender.as_deref().unwrap_or("")
        );
        let mut refs = extract_sir_refs(&text);
        refs.sort_unstable();
        refs.dedup();
        for r in refs {
            if !known.contains(r.as_str()) {
                out.push(Finding {
                    rule: "missing-ref",
                    subject: e.sir_no.clone(),
                    detail: format!("references {r}, which is not in the register"),
                });
            }
        }
    }
    out
}

/// Rule 3: more than GAP_DAYS between consecutive dated entries.
fn check_gaps(entries: &[SirEntry]) -> Vec<Finding> {
    let mut dated: Vec<&SirEntry> = entries.iter().filter(|e| e.doc_date.is_some()).collect();
    dated.sort_by(|a, b| {
        (
            a.doc_date.as_deref(),
            a.prefix.as_str(),
            a.seq,
        )
            .cmp(&(b.doc_date.as_deref(), b.prefix.as_str(), b.seq))
    });
    let mut out = Vec::new();
    for pair in dated.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (da, db) = (a.doc_date.as_deref().unwrap(), b.doc_date.as_deref().unwrap());
        let Some(gap) = days_between(da, db) else { continue };
        if gap > GAP_DAYS {
            out.push(Finding {
                rule: "gap",
                subject: format!("{} -> {}", a.sir_no, b.sir_no),
                detail: format!("{gap} days between {} and {}", brit(da), brit(db)),
            });
        }
    }
    out
}

/// Rule 4: parent_sir points at a SIR that does not exist.
fn check_orphan_attachments(entries: &[SirEntry]) -> Vec<Finding> {
    let known: HashSet<&str> = entries.iter().map(|e| e.sir_no.as_str()).collect();
    entries
        .iter()
        .filter(|e| {
            e.parent_sir
                .as_deref()
                .is_some_and(|p| !known.contains(p))
        })
        .map(|e| Finding {
            rule: "orphan-attachment",
            subject: e.sir_no.clone(),
            detail: format!(
                "parent {} is not in the register",
                e.parent_sir.as_deref().unwrap()
            ),
        })
        .collect()
}

/// Rule 5: an entry dated earlier than its own parent entry.
fn check_impossible_sequences(entries: &[SirEntry]) -> Vec<Finding> {
    let by_sir: HashMap<&str, &SirEntry> =
        entries.iter().map(|e| (e.sir_no.as_str(), e)).collect();
    let mut out = Vec::new();
    for e in entries {
        let Some(parent) = e.parent_sir.as_deref().and_then(|p| by_sir.get(p)) else {
            continue;
        };
        let (Some(d), Some(pd)) = (&e.doc_date, &parent.doc_date) else {
            continue;
        };
        if d < pd {
            out.push(Finding {
                rule: "impossible-sequence",
                subject: e.sir_no.clone(),
                detail: format!(
                    "dated {} but its parent {} is dated {}",
                    brit(d),
                    parent.sir_no,
                    brit(pd)
                ),
            });
        }
    }
    out
}

/// British display for an ISO date; falls back to the raw string if malformed.
fn brit(iso: &str) -> String {
    uk_from_iso(iso).unwrap_or_else(|| iso.to_string())
}

pub(crate) fn cmd_check(db: &Db) -> Result<bool> {
    let findings = run_checks(&db.conn)?;
    for f in &findings {
        println!("{f}");
    }
    println!("{} finding(s)", findings.len());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use evidence_core::Db;

    fn entry(sir: &str, seq: i64, date: Option<&str>, desc: &str) -> SirEntry {
        let (prefix, _) = evidence_core::sir::parse_sir(sir).unwrap();
        SirEntry {
            sir_no: sir.into(),
            seq,
            prefix,
            doc_date: date.map(String::from),
            doc_type: Some("Email".into()),
            description: Some(desc.into()),
            status: "draft".into(),
            ..Default::default()
        }
    }

    fn rules(findings: &[Finding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.rule).collect()
    }

    /// Insert an entry whose parent_sir dangles (simulating data imported
    /// before its parent, or a parent later removed). The schema's FK on
    /// parent_sir must be bypassed to plant this state in a test.
    fn insert_with_dangling_parent(db: &Db, e: &SirEntry) {
        db.conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        e.insert(&db.conn).unwrap();
        db.conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    }

    /// Clean register: distinct descriptions, valid refs and parents, tight
    /// date sequence. No rule may fire.
    fn seed_clean(db: &Db) {
        entry("AC-SIR-0001", 1, Some("2026-03-01"), "Grievance letter sent").insert(&db.conn).unwrap();
        entry("AC-SIR-0002", 2, Some("2026-03-20"), "OH report received").insert(&db.conn).unwrap();
        let mut child = entry("AC-SIR-0003", 3, Some("2026-03-25"), "Attachment to OH report");
        child.parent_sir = Some("AC-SIR-0002".into());
        child.insert(&db.conn).unwrap();
        entry("AC-SIR-0004", 4, None, "Undated manuscript note").insert(&db.conn).unwrap();
    }

    #[test]
    fn clean_register_produces_no_findings() {
        let db = Db::open_memory().unwrap();
        seed_clean(&db);
        assert!(run_checks(&db.conn).unwrap().is_empty());
    }

    #[test]
    fn rule_conflicting_date_fires() {
        let db = Db::open_memory().unwrap();
        // Same event described with different embedded dates and whitespace.
        entry("AC-SIR-0001", 1, Some("2026-03-01"), "Grievance letter sent 03/07/2026")
            .insert(&db.conn).unwrap();
        entry("AC-SIR-0002", 2, Some("2026-03-05"), "  grievance   letter SENT ")
            .insert(&db.conn).unwrap();
        let findings = run_checks(&db.conn).unwrap();
        assert_eq!(rules(&findings), ["conflicting-date"]);
        assert_eq!(findings[0].subject, "AC-SIR-0001, AC-SIR-0002");
        assert!(findings[0].detail.contains("01/03/2026 vs 05/03/2026"));
    }

    #[test]
    fn rule_conflicting_date_ignores_same_date_and_distinct_desc() {
        let db = Db::open_memory().unwrap();
        entry("AC-SIR-0001", 1, Some("2026-03-01"), "Grievance letter").insert(&db.conn).unwrap();
        // Identical description, identical date: a duplicate, but not a conflict.
        entry("AC-SIR-0002", 2, Some("2026-03-01"), "grievance letter").insert(&db.conn).unwrap();
        assert!(run_checks(&db.conn).unwrap().is_empty());
    }

    #[test]
    fn rule_missing_ref_fires_on_description_and_sender() {
        let db = Db::open_memory().unwrap();
        entry("AC-SIR-0001", 1, Some("2026-03-01"), "Follow-up to AC-SIR-0999 discussed")
            .insert(&db.conn).unwrap();
        let mut e2 = entry("AC-SIR-0002", 2, Some("2026-03-02"), "Routine note");
        e2.sender = Some("Re: IG-SIR-0007 <j@example.com>".into());
        e2.insert(&db.conn).unwrap();
        let findings = run_checks(&db.conn).unwrap();
        assert_eq!(rules(&findings), ["missing-ref", "missing-ref"]);
        assert_eq!(findings[0].subject, "AC-SIR-0001");
        assert!(findings[0].detail.contains("AC-SIR-0999"));
        assert_eq!(findings[1].subject, "AC-SIR-0002");
        assert!(findings[1].detail.contains("IG-SIR-0007"));
    }

    #[test]
    fn rule_missing_ref_quiet_when_ref_exists() {
        let db = Db::open_memory().unwrap();
        entry("AC-SIR-0001", 1, Some("2026-03-01"), "Original email").insert(&db.conn).unwrap();
        entry("AC-SIR-0002", 2, Some("2026-03-02"), "Reply to AC-SIR-0001").insert(&db.conn).unwrap();
        assert!(run_checks(&db.conn).unwrap().is_empty());
    }

    #[test]
    fn rule_gap_fires_over_60_days() {
        let db = Db::open_memory().unwrap();
        entry("AC-SIR-0001", 1, Some("2026-01-01"), "First event").insert(&db.conn).unwrap();
        entry("AC-SIR-0002", 2, Some("2026-03-05"), "Second event").insert(&db.conn).unwrap();
        let findings = run_checks(&db.conn).unwrap();
        assert_eq!(rules(&findings), ["gap"]);
        assert_eq!(findings[0].subject, "AC-SIR-0001 -> AC-SIR-0002");
        assert!(findings[0].detail.contains("63 days"));
    }

    #[test]
    fn rule_gap_quiet_at_exactly_60_days() {
        let db = Db::open_memory().unwrap();
        entry("AC-SIR-0001", 1, Some("2026-01-01"), "First event").insert(&db.conn).unwrap();
        entry("AC-SIR-0002", 2, Some("2026-03-02"), "Second event").insert(&db.conn).unwrap(); // 60 days
        assert!(run_checks(&db.conn).unwrap().is_empty());
    }

    #[test]
    fn rule_orphan_attachment_fires() {
        let db = Db::open_memory().unwrap();
        let mut e = entry("AC-SIR-0001", 1, Some("2026-03-01"), "Scanned attachment");
        e.parent_sir = Some("AC-SIR-0999".into());
        insert_with_dangling_parent(&db, &e);
        let findings = run_checks(&db.conn).unwrap();
        assert_eq!(rules(&findings), ["orphan-attachment"]);
        assert_eq!(findings[0].subject, "AC-SIR-0001");
        assert!(findings[0].detail.contains("AC-SIR-0999"));
    }

    #[test]
    fn rule_impossible_sequence_fires() {
        let db = Db::open_memory().unwrap();
        entry("AC-SIR-0001", 1, Some("2026-05-10"), "Parent email").insert(&db.conn).unwrap();
        let mut child = entry("AC-SIR-0002", 2, Some("2026-05-01"), "Attachment predating parent");
        child.parent_sir = Some("AC-SIR-0001".into());
        child.insert(&db.conn).unwrap();
        let findings = run_checks(&db.conn).unwrap();
        assert_eq!(rules(&findings), ["impossible-sequence"]);
        assert_eq!(findings[0].subject, "AC-SIR-0002");
        assert!(findings[0].detail.contains("01/05/2026"));
        assert!(findings[0].detail.contains("10/05/2026"));
    }

    #[test]
    fn rule_impossible_sequence_ignores_undated() {
        let db = Db::open_memory().unwrap();
        entry("AC-SIR-0001", 1, Some("2026-05-10"), "Parent email").insert(&db.conn).unwrap();
        let mut child = entry("AC-SIR-0002", 2, None, "Undated attachment");
        child.parent_sir = Some("AC-SIR-0001".into());
        child.insert(&db.conn).unwrap();
        assert!(run_checks(&db.conn).unwrap().is_empty());
    }

    #[test]
    fn several_rules_can_fire_together_in_stable_order() {
        let db = Db::open_memory().unwrap();
        // One fixture that trips conflicting-date, gap, and orphan together.
        entry("AC-SIR-0001", 1, Some("2026-01-01"), "Meeting notes 01.01.2026")
            .insert(&db.conn).unwrap();
        entry("AC-SIR-0002", 2, Some("2026-02-01"), "meeting notes").insert(&db.conn).unwrap();
        let mut orphan = entry("AC-SIR-0003", 3, Some("2026-06-01"), "Late scan");
        orphan.parent_sir = Some("AC-SIR-0999".into());
        insert_with_dangling_parent(&db, &orphan);
        let findings = run_checks(&db.conn).unwrap();
        assert_eq!(
            rules(&findings),
            ["conflicting-date", "gap", "orphan-attachment"]
        );
    }

    #[test]
    fn description_normalization() {
        assert_eq!(normalize_description("  Grievance   Letter SENT 03/07/2026 "), "grievance letter sent");
        assert_eq!(normalize_description("Report 2026-07-03 final"), "report final");
        assert_eq!(normalize_description("no dates"), "no dates");
        assert_eq!(normalize_description("03/07/2026"), "");
    }
}
