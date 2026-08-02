//! Deficiency flags: deterministic rules over `disclosure_items` joined with
//! the Master Chronology. Each rule returns zero or more findings; nothing
//! here mutates the database.

use std::collections::HashSet;

use anyhow::Result;
use regex::Regex;
use rusqlite::Connection;

use evidence_core::sir::extract_sir_refs;
use evidence_core::util;
use evidence_core::SirEntry;

use crate::item;

pub(crate) const REQUESTED_BUT_NOT_ANSWERED: &str = "REQUESTED-BUT-NOT-ANSWERED";
pub(crate) const ANSWERED_WITHOUT_DOCUMENT: &str = "ANSWERED-WITHOUT-DOCUMENT";
pub(crate) const DOC_REFERENCED_NOT_DISCLOSED: &str = "DOC-REFERENCED-NOT-DISCLOSED";
pub(crate) const PARTIAL_DATE_RANGE: &str = "PARTIAL-DATE-RANGE";
pub(crate) const POLICY_WITHOUT_VERSION: &str = "POLICY-WITHOUT-VERSION";
pub(crate) const MISSING_ATTACHMENT: &str = "MISSING-ATTACHMENT";

/// Canonical display order for reports.
pub(crate) const RULE_ORDER: [&str; 6] = [
    REQUESTED_BUT_NOT_ANSWERED,
    ANSWERED_WITHOUT_DOCUMENT,
    DOC_REFERENCED_NOT_DISCLOSED,
    PARTIAL_DATE_RANGE,
    POLICY_WITHOUT_VERSION,
    MISSING_ATTACHMENT,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Flag {
    pub rule: &'static str,
    /// Item the flag attaches to; `None` for findings about chronology
    /// entries not linked to any item (DOC-REFERENCED-NOT-DISCLOSED).
    pub item_id: Option<i64>,
    pub detail: String,
}

/// Display helper: ISO -> dd/mm/yyyy, passing through anything unparseable.
fn uk(iso: &str) -> String {
    util::uk_from_iso(iso).unwrap_or_else(|| iso.to_string())
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}

/// Find a "from X to Y" British date range in free text. Returns the ISO pair
/// ordered earliest-first. Connectors: to / and / until / through / dashes.
pub(crate) fn extract_date_range(text: &str) -> Option<(String, String)> {
    let re = Regex::new(
        r"(\d{1,2}[/.\-]\d{1,2}[/.\-]\d{2,4})\s*(?i:to|and|until|through|[-–—])\s*(\d{1,2}[/.\-]\d{1,2}[/.\-]\d{2,4})",
    )
    .ok()?;
    let c = re.captures(text)?;
    let a = util::iso_from_uk_date(&c[1])?;
    let b = util::iso_from_uk_date(&c[2])?;
    Some(if a <= b { (a, b) } else { (b, a) })
}

/// Evaluate every rule against the current database state.
pub(crate) fn compute_flags(conn: &Connection) -> Result<Vec<Flag>> {
    let items = item::list(conn, None, None)?;
    let today = util::today_iso();
    let mut out = Vec::new();

    for it in &items {
        // REQUESTED-BUT-NOT-ANSWERED: deadline passed, still open.
        if it.status == "open" {
            if let Some(dl) = &it.response_deadline {
                if dl < &today {
                    out.push(Flag {
                        rule: REQUESTED_BUT_NOT_ANSWERED,
                        item_id: Some(it.id),
                        detail: format!("deadline {} passed, still open", uk(dl)),
                    });
                }
            }
        }

        // ANSWERED-WITHOUT-DOCUMENT: a response arrived but nothing was linked.
        if it.response_received.is_some() && it.supplied_sir.is_none() {
            out.push(Flag {
                rule: ANSWERED_WITHOUT_DOCUMENT,
                item_id: Some(it.id),
                detail: format!(
                    "response received {} but no document linked",
                    uk(it.response_received.as_deref().unwrap_or(""))
                ),
            });
        }

        // Rules needing the supplied chronology entry.
        if let Some(sir) = &it.supplied_sir {
            let Some(entry) = SirEntry::get(conn, sir)? else {
                continue;
            };

            // PARTIAL-DATE-RANGE: request named a range; supplied doc outside it.
            if let Some((lo, hi)) = extract_date_range(&it.request_wording) {
                if let Some(dd) = &entry.doc_date {
                    if dd < &lo || dd > &hi {
                        out.push(Flag {
                            rule: PARTIAL_DATE_RANGE,
                            item_id: Some(it.id),
                            detail: format!(
                                "supplied {} dated {} outside requested range {} to {}",
                                sir,
                                uk(dd),
                                uk(&lo),
                                uk(&hi)
                            ),
                        });
                    }
                }
            }

            // POLICY-WITHOUT-VERSION: policy request answered with an undated doc.
            let mentions_policy = contains_ci(&it.request_wording, "policy")
                || it
                    .issue
                    .as_deref()
                    .map_or(false, |i| contains_ci(i, "policy"));
            if mentions_policy && entry.doc_date.is_none() {
                out.push(Flag {
                    rule: POLICY_WITHOUT_VERSION,
                    item_id: Some(it.id),
                    detail: format!(
                        "supplied {sir} has no document date — policy version unverifiable"
                    ),
                });
            }

            // MISSING-ATTACHMENT: supplied email has no attachment children.
            let is_email = entry
                .doc_type
                .as_deref()
                .map_or(false, |t| contains_ci(t, "email"));
            if is_email {
                let has_child: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sir_entries WHERE parent_sir = ?1)",
                    [sir],
                    |r| r.get(0),
                )?;
                if !has_child {
                    out.push(Flag {
                        rule: MISSING_ATTACHMENT,
                        item_id: Some(it.id),
                        detail: format!(
                            "supplied {sir} is an email with no attachments on record"
                        ),
                    });
                }
            }
        }
    }

    // DOC-REFERENCED-NOT-DISCLOSED: chronology entries mentioning attachments/
    // enclosures that no disclosure item links to (via supplied_sir or a SIR
    // reference inside the request wording).
    let mut linked: HashSet<String> = HashSet::new();
    for it in &items {
        if let Some(s) = &it.supplied_sir {
            linked.insert(s.clone());
        }
        linked.extend(extract_sir_refs(&it.request_wording));
    }
    let mut stmt = conn.prepare(
        "SELECT sir_no, COALESCE(description, ''), COALESCE(sender, '') FROM sir_entries",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (sir_no, desc, sender) = row?;
        if linked.contains(&sir_no) {
            continue;
        }
        if ["attach", "enclos"]
            .iter()
            .any(|k| contains_ci(&desc, k) || contains_ci(&sender, k))
        {
            out.push(Flag {
                rule: DOC_REFERENCED_NOT_DISCLOSED,
                item_id: None,
                detail: format!(
                    "{sir_no} mentions an attachment/enclosure but is not linked to any disclosure item"
                ),
            });
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{insert, DisclosureItem};
    use evidence_core::Db;

    /// Seed a chronology entry without touching SIR allocation.
    fn seed_entry(
        db: &Db,
        sir: &str,
        doc_type: Option<&str>,
        doc_date: Option<&str>,
        desc: Option<&str>,
    ) {
        SirEntry {
            sir_no: sir.into(),
            seq: 1,
            prefix: "AC".into(),
            doc_date: doc_date.map(Into::into),
            doc_type: doc_type.map(Into::into),
            description: desc.map(Into::into),
            status: "confirmed".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
    }

    fn open_item(wording: &str) -> DisclosureItem {
        DisclosureItem::new(wording)
    }

    fn rules(flags: &[Flag]) -> Vec<&'static str> {
        flags.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn requested_but_not_answered_fires_and_clears() {
        let db = Db::open_memory().unwrap();
        let mut it = open_item("All grievance notes");
        it.date_requested = Some("2020-01-01".into());
        it.response_deadline = Some("2020-01-15".into()); // long past
        let id = insert(&db.conn, &it).unwrap();

        let flags = compute_flags(&db.conn).unwrap();
        assert!(rules(&flags).contains(&REQUESTED_BUT_NOT_ANSWERED));
        assert_eq!(flags[0].item_id, Some(id));

        // Answering the item clears the flag.
        crate::item::record_response(&db.conn, id, Some("2020-01-10"), None, "answered", None)
            .unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(!rules(&flags).contains(&REQUESTED_BUT_NOT_ANSWERED));

        // A future deadline on an open item is clean too.
        let mut it2 = open_item("Recent request");
        it2.date_requested = Some("2099-01-01".into());
        it2.response_deadline = Some("2099-01-15".into());
        insert(&db.conn, &it2).unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(!rules(&flags).contains(&REQUESTED_BUT_NOT_ANSWERED));
    }

    #[test]
    fn answered_without_document_fires_and_clears() {
        let db = Db::open_memory().unwrap();
        let id = insert(&db.conn, &open_item("Sickness records")).unwrap();
        crate::item::record_response(&db.conn, id, Some("2026-07-01"), None, "answered", None)
            .unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(rules(&flags).contains(&ANSWERED_WITHOUT_DOCUMENT));

        seed_entry(&db, "AC-SIR-0001", Some("Medical"), Some("2026-06-01"), None);
        crate::item::record_response(&db.conn, id, None, Some("AC-SIR-0001"), "answered", None)
            .unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(!rules(&flags).contains(&ANSWERED_WITHOUT_DOCUMENT));
    }

    #[test]
    fn doc_referenced_not_disclosed_fires_and_clears() {
        let db = Db::open_memory().unwrap();
        seed_entry(
            &db,
            "AC-SIR-0007",
            Some("Email"),
            Some("2026-03-02"),
            Some("Rota email with spreadsheet attached"),
        );
        let flags = compute_flags(&db.conn).unwrap();
        assert!(rules(&flags).contains(&DOC_REFERENCED_NOT_DISCLOSED));

        // Linking via supplied_sir clears it...
        let mut it = open_item("Rota spreadsheet");
        it.supplied_sir = Some("AC-SIR-0007".into());
        it.status = "answered".into();
        insert(&db.conn, &it).unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(!rules(&flags).contains(&DOC_REFERENCED_NOT_DISCLOSED));

        // ...and so does a SIR reference inside the request wording.
        seed_entry(
            &db,
            "AC-SIR-0008",
            Some("Letter"),
            Some("2026-03-05"),
            Some("Letter enclosing policy"),
        );
        insert(&db.conn, &open_item("The enclosed policy at AC-SIR-0008")).unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(!rules(&flags).contains(&DOC_REFERENCED_NOT_DISCLOSED));

        // An entry with no attach/enclos mention is clean from the start.
        seed_entry(&db, "AC-SIR-0009", Some("Letter"), None, Some("Plain letter"));
        let clean: Vec<_> = compute_flags(&db.conn)
            .unwrap()
            .into_iter()
            .filter(|f| f.rule == DOC_REFERENCED_NOT_DISCLOSED && f.detail.contains("AC-SIR-0009"))
            .collect();
        assert!(clean.is_empty());
    }

    #[test]
    fn partial_date_range_fires_and_clears() {
        let db = Db::open_memory().unwrap();
        seed_entry(&db, "AC-SIR-0002", Some("Email"), Some("2026-05-01"), None);
        let mut it = open_item("All rota emails from 01/01/2026 to 31/03/2026");
        it.supplied_sir = Some("AC-SIR-0002".into());
        it.status = "answered".into();
        insert(&db.conn, &it).unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(rules(&flags).contains(&PARTIAL_DATE_RANGE));

        // Supplied document inside the range: clean.
        seed_entry(&db, "AC-SIR-0003", Some("Email"), Some("2026-02-15"), None);
        let mut it2 = open_item("All rota emails from 01/01/2026 to 31/03/2026");
        it2.supplied_sir = Some("AC-SIR-0003".into());
        it2.status = "answered".into();
        insert(&db.conn, &it2).unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        let hits: Vec<_> = flags
            .iter()
            .filter(|f| f.rule == PARTIAL_DATE_RANGE && f.detail.contains("AC-SIR-0003"))
            .collect();
        assert!(hits.is_empty());
    }

    #[test]
    fn policy_without_version_fires_and_clears() {
        let db = Db::open_memory().unwrap();
        seed_entry(&db, "AC-SIR-0004", Some("Policy"), None, Some("Dignity at work policy"));
        let mut it = open_item("The dignity at work policy in force at the time");
        it.supplied_sir = Some("AC-SIR-0004".into());
        it.status = "answered".into();
        insert(&db.conn, &it).unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(rules(&flags).contains(&POLICY_WITHOUT_VERSION));

        // Dated policy document: clean. Issue field mention counts too.
        seed_entry(&db, "AC-SIR-0005", Some("Policy"), Some("2025-11-01"), None);
        let mut it2 = open_item("Version in force");
        it2.issue = Some("policy".into());
        it2.supplied_sir = Some("AC-SIR-0005".into());
        it2.status = "answered".into();
        insert(&db.conn, &it2).unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        let hits: Vec<_> = flags
            .iter()
            .filter(|f| f.rule == POLICY_WITHOUT_VERSION && f.detail.contains("AC-SIR-0005"))
            .collect();
        assert!(hits.is_empty());
    }

    #[test]
    fn missing_attachment_fires_and_clears() {
        let db = Db::open_memory().unwrap();
        seed_entry(&db, "AC-SIR-0006", Some("Email"), Some("2026-04-01"), Some("Covering email"));
        let mut it = open_item("The covering email and its attachments");
        it.supplied_sir = Some("AC-SIR-0006".into());
        it.status = "answered".into();
        insert(&db.conn, &it).unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(rules(&flags).contains(&MISSING_ATTACHMENT));

        // Give the email a child attachment: clean.
        SirEntry {
            sir_no: "AC-SIR-0010".into(),
            seq: 10,
            prefix: "AC".into(),
            doc_type: Some("Spreadsheet".into()),
            parent_sir: Some("AC-SIR-0006".into()),
            status: "confirmed".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
        let flags = compute_flags(&db.conn).unwrap();
        assert!(!rules(&flags).contains(&MISSING_ATTACHMENT));
    }

    #[test]
    fn date_range_parsing() {
        assert_eq!(
            extract_date_range("emails from 01/01/2026 to 31/03/2026"),
            Some(("2026-01-01".into(), "2026-03-31".into()))
        );
        assert_eq!(
            extract_date_range("between 31/03/2026 and 01/01/2026"),
            Some(("2026-01-01".into(), "2026-03-31".into()))
        );
        assert_eq!(
            extract_date_range("period 01-01-2026 - 31-03-2026"),
            Some(("2026-01-01".into(), "2026-03-31".into()))
        );
        // A single date is not a range.
        assert_eq!(extract_date_range("letter dated 03/07/2026"), None);
        assert_eq!(extract_date_range("no dates at all"), None);
    }
}
