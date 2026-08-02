//! Rendering: list tables/CSV, the flags report, chase letters and the
//! disclosure deficiency schedule. Pure text generation — no database writes.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use rusqlite::Connection;

use evidence_core::util;

use crate::flags::{self, Flag};
use crate::item::{self, DisclosureItem};

/// ISO date (or None) for display: British dd/mm/yyyy, "-" when absent.
fn british(iso: Option<&String>) -> String {
    iso.and_then(|d| util::uk_from_iso(d))
        .unwrap_or_else(|| "-".to_string())
}

fn today_british() -> String {
    util::uk_from_iso(&util::today_iso()).unwrap_or_else(|| "-".to_string())
}

/// Truncate to `max` chars, adding an ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// Simple fixed-width table with a header row.
fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let render = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = widths[i] - c.chars().count();
                format!("{c}{:pad$}", "", pad = pad)
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = render(
        &headers
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>(),
    );
    for row in rows {
        out.push('\n');
        out.push_str(&render(row));
    }
    out
}

fn list_row(it: &DisclosureItem) -> Vec<String> {
    vec![
        it.id.to_string(),
        british(it.date_requested.as_ref()),
        british(it.response_deadline.as_ref()),
        british(it.response_received.as_ref()),
        it.status.clone(),
        it.supplied_sir.clone().unwrap_or_else(|| "-".into()),
        it.bundle_dest.clone().unwrap_or_else(|| "-".into()),
        truncate(&it.request_wording, 50),
    ]
}

const LIST_HEADERS: [&str; 8] = [
    "ID", "REQUESTED", "DEADLINE", "RECEIVED", "STATUS", "SIR", "BUNDLE", "WORDING",
];

/// Human-readable list of items (pre-filtered by the caller).
pub(crate) fn list_table(items: &[DisclosureItem]) -> String {
    if items.is_empty() {
        return "no disclosure items found".to_string();
    }
    let rows: Vec<Vec<String>> = items.iter().map(list_row).collect();
    table(&LIST_HEADERS, &rows)
}

/// Machine-readable list. Dates are displayed British, per house convention.
pub(crate) fn list_csv(items: &[DisclosureItem]) -> Result<String> {
    let mut wtr = csv::Writer::from_writer(vec![]);
    wtr.write_record([
        "id",
        "request_wording",
        "issue",
        "date_requested",
        "recipient",
        "response_deadline",
        "response_received",
        "supplied_sir",
        "status",
        "objection",
        "bundle_dest",
        "assists_client",
    ])?;
    for it in items {
        wtr.write_record([
            it.id.to_string(),
            it.request_wording.clone(),
            it.issue.clone().unwrap_or_default(),
            british(it.date_requested.as_ref()),
            it.recipient.clone().unwrap_or_default(),
            british(it.response_deadline.as_ref()),
            british(it.response_received.as_ref()),
            it.supplied_sir.clone().unwrap_or_default(),
            it.status.clone(),
            it.objection.clone().unwrap_or_default(),
            it.bundle_dest.clone().unwrap_or_default(),
            it.assists_client.map_or(String::new(), |v| v.to_string()),
        ])?;
    }
    Ok(String::from_utf8(wtr.into_inner()?)?)
}

/// Grouped flags report for `disclosure flags`.
pub(crate) fn flags_report(conn: &Connection) -> Result<String> {
    let flags = flags::compute_flags(conn)?;
    if flags.is_empty() {
        return Ok("no disclosure flags — all clear".to_string());
    }
    let mut out = format!("DISCLOSURE FLAGS — {}\n", today_british());
    for rule in flags::RULE_ORDER {
        let hits: Vec<&Flag> = flags.iter().filter(|f| f.rule == rule).collect();
        if hits.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{rule}\n"));
        for f in hits {
            match f.item_id {
                Some(id) => out.push_str(&format!("  item {id}: {}\n", f.detail)),
                None => out.push_str(&format!("  {}\n", f.detail)),
            }
        }
    }
    Ok(out.trim_end().to_string())
}

/// Build a chase letter for the given item ids. Returns the letter, the ids
/// included, and the ids skipped because they are no longer outstanding.
pub(crate) fn chase_letter(
    conn: &Connection,
    ids: &[i64],
) -> Result<(String, Vec<i64>, Vec<i64>)> {
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    for id in ids {
        let it = item::get(conn, *id)?
            .ok_or_else(|| anyhow::anyhow!("no disclosure item with id {id}"))?;
        if it.is_outstanding() {
            items.push(it);
        } else {
            skipped.push(*id);
        }
    }
    if items.is_empty() {
        bail!("none of the given items are outstanding — nothing to chase");
    }

    let today = util::today_iso();
    let mut s = String::new();
    s.push_str("DISCLOSURE CHASE LETTER\n");
    s.push_str(&format!("Date: {}\n\n", today_british()));
    s.push_str("Dear Sirs,\n\n");
    s.push_str("Re: Outstanding disclosure requests\n\n");
    s.push_str(
        "The following requests for disclosure remain outstanding. The date each \
         request was made and the response deadline are shown. Please supply the \
         requested material immediately.\n\n",
    );
    for (n, it) in items.iter().enumerate() {
        s.push_str(&format!("{}. {}\n", n + 1, it.request_wording));
        s.push_str(&format!("   Date requested: {}\n", british(it.date_requested.as_ref())));
        match &it.response_deadline {
            Some(dl) => {
                let note = util::days_between(dl, &today).map_or(String::new(), |d| {
                    if d > 0 {
                        format!(" — overdue by {d} days")
                    } else {
                        String::new()
                    }
                });
                s.push_str(&format!("   Response deadline: {}{note}\n", british(Some(dl))));
            }
            None => s.push_str("   Response deadline: -\n"),
        }
        match it.status.as_str() {
            "partial" => s.push_str(
                "   Status: partially answered — the remainder is still outstanding.\n",
            ),
            "objected" => s.push_str(&format!(
                "   Status: objection raised ({}). The objection is not accepted; \
                 the material is still required.\n",
                it.objection.as_deref().unwrap_or("no reason given")
            )),
            _ => {}
        }
        s.push('\n');
    }
    s.push_str(
        "We look forward to receiving the outstanding material by return.\n\n\
         Yours faithfully,\n",
    );
    let used = items.iter().map(|it| it.id).collect();
    Ok((s, used, skipped))
}

/// The disclosure deficiency schedule: every item, its status, live flags and
/// bundle destination, plus unlinked document references.
pub(crate) fn schedule(conn: &Connection) -> Result<String> {
    let items = item::list(conn, None, None)?;
    let flags = flags::compute_flags(conn)?;

    let mut by_item: HashMap<i64, Vec<&str>> = HashMap::new();
    let mut unlinked: Vec<&Flag> = Vec::new();
    for f in &flags {
        match f.item_id {
            Some(id) => by_item.entry(id).or_default().push(f.rule),
            None => unlinked.push(f),
        }
    }

    let mut out = format!("DISCLOSURE DEFICIENCY SCHEDULE — {}\n\n", today_british());
    if items.is_empty() {
        out.push_str("no disclosure items on record\n");
        return Ok(out.trim_end().to_string());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|it| {
            let item_flags = by_item
                .get(&it.id)
                .map(|v| v.join(", "))
                .unwrap_or_else(|| "-".into());
            vec![
                it.id.to_string(),
                british(it.date_requested.as_ref()),
                british(it.response_deadline.as_ref()),
                british(it.response_received.as_ref()),
                it.status.clone(),
                item_flags,
                it.bundle_dest.clone().unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    out.push_str(&table(
        &["ID", "REQUESTED", "DEADLINE", "RECEIVED", "STATUS", "FLAGS", "BUNDLE"],
        &rows,
    ));

    if !unlinked.is_empty() {
        out.push_str(
            "\n\nDocuments referencing attachments but not linked to any request:",
        );
        for f in unlinked {
            out.push_str(&format!("\n  {}", f.detail));
        }
    }

    let open = items.iter().filter(|i| i.is_outstanding()).count();
    let flagged = by_item.len();
    out.push_str(&format!(
        "\n\nTotals: {} item{} | {open} outstanding | {flagged} flagged",
        items.len(),
        if items.len() == 1 { "" } else { "s" },
    ));
    Ok(out)
}

/// Write `text` to `path`, or print it when no path is given. Returns a short
/// human-readable destination description.
pub(crate) fn emit(out: Option<&String>, text: &str) -> Result<String> {
    match out {
        Some(path) => {
            std::fs::write(path, format!("{text}\n"))
                .with_context(|| format!("writing {path}"))?;
            Ok(format!("written to {path}"))
        }
        None => {
            println!("{text}");
            Ok("printed to stdout".to_string())
        }
    }
}
