//! `chronology build` and `chronology export`: filtered or full-register
//! renderings of the Master Chronology. Rendering never copies data into new
//! tables — these are read-only views over `sir_entries` + `entry_tags`.

use std::collections::HashMap;

use anyhow::{Context, Result};
use clap::ArgMatches;
use evidence_core::audit::audit;
use evidence_core::util::{iso_from_uk_date, uk_from_iso};
use evidence_core::{Db, SirEntry};
use rusqlite::Connection;
use serde::Serialize;

use crate::{print_sub_help, ACTOR};

/// Filters for `build`. `from`/`to` are ISO dates (parsed from British input).
/// Multiple `tags` are OR'd. An entry with no date never matches a date range.
#[derive(Debug, Default)]
pub(crate) struct Filter {
    pub tags: Vec<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub prefix: Option<String>,
}

/// One chronology row: the register entry plus its sorted tag list.
#[derive(Debug)]
pub(crate) struct ChronRow {
    pub entry: SirEntry,
    pub tags: Vec<String>,
}

/// Load entries + tags, apply the filter, sort chronologically
/// (dated first by date, undated last; ties broken by prefix + seq so the
/// output is fully deterministic).
pub(crate) fn collect_rows(conn: &Connection, f: &Filter) -> Result<Vec<ChronRow>> {
    let mut tag_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT sir_no, tag FROM entry_tags ORDER BY sir_no, tag")?;
    let pairs = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for p in pairs {
        let (sir, tag) = p?;
        tag_map.entry(sir).or_default().push(tag);
    }

    let wanted: Vec<String> = f.tags.iter().map(|t| t.to_lowercase()).collect();
    let mut out = Vec::new();
    for e in SirEntry::list(conn)? {
        if let Some(p) = &f.prefix {
            if !e.prefix.eq_ignore_ascii_case(p) {
                continue;
            }
        }
        if f.from.is_some() || f.to.is_some() {
            // Undated entries cannot fall inside a date range.
            let Some(d) = e.doc_date.as_deref() else { continue };
            if f.from.as_deref().is_some_and(|from| d < from) {
                continue;
            }
            if f.to.as_deref().is_some_and(|to| d > to) {
                continue;
            }
        }
        let tags = tag_map.remove(&e.sir_no).unwrap_or_default();
        if !wanted.is_empty() && !wanted.iter().any(|t| tags.contains(t)) {
            continue;
        }
        out.push(ChronRow { entry: e, tags });
    }
    out.sort_by(chron_cmp);
    Ok(out)
}

fn chron_cmp(a: &ChronRow, b: &ChronRow) -> std::cmp::Ordering {
    chron_key(a).cmp(&chron_key(b))
}

fn chron_key(r: &ChronRow) -> (bool, &str, &str, i64) {
    (
        r.entry.doc_date.is_none(),
        r.entry.doc_date.as_deref().unwrap_or(""),
        &r.entry.prefix,
        r.entry.seq,
    )
}

/// British display date, or "undated".
fn brit_date(e: &SirEntry) -> String {
    e.doc_date
        .as_deref()
        .and_then(uk_from_iso)
        .unwrap_or_else(|| "undated".into())
}

fn or_dash(v: &Option<String>) -> &str {
    v.as_deref().filter(|s| !s.is_empty()).unwrap_or("-")
}

const HEADERS: [&str; 7] = ["SIR", "DATE", "TYPE", "DESCRIPTION", "SENDER", "BATCH", "TAGS"];

fn row_cells(r: &ChronRow) -> [String; 7] {
    [
        r.entry.sir_no.clone(),
        brit_date(&r.entry),
        or_dash(&r.entry.doc_type).to_string(),
        or_dash(&r.entry.description).to_string(),
        or_dash(&r.entry.sender).to_string(),
        or_dash(&r.entry.source_batch).to_string(),
        r.tags.join(", "),
    ]
}

/// Aligned fixed-width table with a header and separator.
pub(crate) fn render_table(rows: &[ChronRow]) -> String {
    let cells: Vec<[String; 7]> = rows.iter().map(row_cells).collect();
    let mut widths: Vec<usize> = HEADERS.iter().map(|h| h.len()).collect();
    for c in &cells {
        for (i, v) in c.iter().enumerate() {
            widths[i] = widths[i].max(v.chars().count());
        }
    }
    let line = |vals: &[String]| -> String {
        vals.iter()
            .enumerate()
            .map(|(i, v)| format!("{:<width$}", v, width = widths[i]))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let mut out = String::new();
    out.push_str(&line(&HEADERS.map(String::from)));
    out.push('\n');
    out.push_str(
        &widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("-+-"),
    );
    out.push('\n');
    for c in &cells {
        out.push_str(&line(c));
        out.push('\n');
    }
    out
}

/// CSV view: `sir,date,type,description,sender,batch,tags` — British dates,
/// empty for missing values, tags joined with `;`.
pub(crate) fn render_csv(rows: &[ChronRow]) -> Result<String> {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record(["sir", "date", "type", "description", "sender", "batch", "tags"])?;
    for r in rows {
        w.write_record([
            r.entry.sir_no.as_str(),
            r.entry
                .doc_date
                .as_deref()
                .and_then(uk_from_iso)
                .unwrap_or_default()
                .as_str(),
            r.entry.doc_type.as_deref().unwrap_or(""),
            r.entry.description.as_deref().unwrap_or(""),
            r.entry.sender.as_deref().unwrap_or(""),
            r.entry.source_batch.as_deref().unwrap_or(""),
            r.tags.join(";").as_str(),
        ])?;
    }
    Ok(String::from_utf8(w.into_inner()?)?)
}

#[derive(Serialize)]
struct JsonRow {
    sir: String,
    /// British dd/mm/yyyy, null when undated.
    date: Option<String>,
    #[serde(rename = "type")]
    doc_type: Option<String>,
    description: Option<String>,
    sender: Option<String>,
    batch: Option<String>,
    tags: Vec<String>,
}

pub(crate) fn render_json(rows: &[ChronRow]) -> Result<String> {
    let jrows: Vec<JsonRow> = rows
        .iter()
        .map(|r| JsonRow {
            sir: r.entry.sir_no.clone(),
            date: r.entry.doc_date.as_deref().and_then(uk_from_iso),
            doc_type: r.entry.doc_type.clone(),
            description: r.entry.description.clone(),
            sender: r.entry.sender.clone(),
            batch: r.entry.source_batch.clone(),
            tags: r.tags.clone(),
        })
        .collect();
    Ok(format!("{}\n", serde_json::to_string_pretty(&jrows)?))
}

/// Full-register CSV for counsel/ET bundles: every register column of
/// interest, tags included, one row per entry.
fn render_register_csv(rows: &[ChronRow]) -> Result<String> {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record([
        "sir",
        "date",
        "type",
        "description",
        "sender",
        "recipients",
        "batch",
        "parent_sir",
        "status",
        "tags",
        "sha256",
        "standard_filename",
    ])?;
    for r in rows {
        let e = &r.entry;
        w.write_record([
            e.sir_no.as_str(),
            e.doc_date
                .as_deref()
                .and_then(uk_from_iso)
                .unwrap_or_default()
                .as_str(),
            e.doc_type.as_deref().unwrap_or(""),
            e.description.as_deref().unwrap_or(""),
            e.sender.as_deref().unwrap_or(""),
            e.recipients.as_deref().unwrap_or(""),
            e.source_batch.as_deref().unwrap_or(""),
            e.parent_sir.as_deref().unwrap_or(""),
            e.status.as_str(),
            r.tags.join(";").as_str(),
            e.sha256.as_deref().unwrap_or(""),
            e.standard_filename.as_deref().unwrap_or(""),
        ])?;
    }
    Ok(String::from_utf8(w.into_inner()?)?)
}

/// Build the filter from clap matches; British dates become ISO.
/// Err carries a user-facing message (never a panic).
pub(crate) fn filter_from_matches(m: &ArgMatches) -> std::result::Result<Filter, String> {
    let tags = m
        .get_many::<String>("tag")
        .map(|v| v.map(|s| s.trim().to_lowercase()).collect())
        .unwrap_or_default();
    let parse = |key: &str| -> std::result::Result<Option<String>, String> {
        match m.get_one::<String>(key) {
            None => Ok(None),
            Some(raw) => iso_from_uk_date(raw)
                .map(Some)
                .ok_or_else(|| format!("invalid --{key} date '{raw}' (expected DD/MM/YYYY)")),
        }
    };
    Ok(Filter {
        tags,
        from: parse("from")?,
        to: parse("to")?,
        prefix: m.get_one::<String>("prefix").map(|s| s.trim().to_uppercase()),
    })
}

/// Write rendered output to `--out` (audited) or stdout.
fn emit(db: &Db, out: Option<&String>, text: &str, n_rows: usize, action: &str) -> Result<()> {
    match out {
        Some(path) => {
            std::fs::write(path, text).with_context(|| format!("writing {path}"))?;
            audit(&db.conn, ACTOR, action, &format!("{n_rows} row(s) -> {path}"))?;
            println!("wrote {n_rows} row(s) to {path}");
        }
        None => print!("{text}"),
    }
    Ok(())
}

pub(crate) fn cmd_build(db: &Db, m: &ArgMatches) -> Result<bool> {
    let filter = match filter_from_matches(m) {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("error: {msg}");
            return Ok(true);
        }
    };
    let format = m
        .get_one::<String>("format")
        .map(|s| s.as_str())
        .unwrap_or("table");
    let rows = collect_rows(&db.conn, &filter)?;
    let text = match format {
        "table" => render_table(&rows),
        "csv" => render_csv(&rows)?,
        "json" => render_json(&rows)?,
        other => {
            eprintln!("error: unknown format '{other}' (table | csv | json)");
            return Ok(true);
        }
    };
    emit(db, m.get_one::<String>("out"), &text, rows.len(), "build")?;
    Ok(true)
}

pub(crate) fn cmd_export(db: &Db, m: &ArgMatches) -> Result<bool> {
    let format = m.get_one::<String>("format");
    let out = m.get_one::<String>("out");
    let (Some(format), Some(out)) = (format, out) else {
        print_sub_help("export");
        return Ok(true);
    };
    if format != "csv" {
        eprintln!("error: export supports only --format csv");
        return Ok(true);
    }
    let rows = collect_rows(&db.conn, &Filter::default())?;
    let text = render_register_csv(&rows)?;
    std::fs::write(out, &text).with_context(|| format!("writing {out}"))?;
    audit(
        &db.conn,
        ACTOR,
        "export",
        &format!("csv register {} row(s) -> {out}", rows.len()),
    )?;
    println!("exported {} row(s) to {out}", rows.len());
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
            sender: Some("sender@example.com".into()),
            source_batch: Some("PB-10".into()),
            status: "draft".into(),
            ..Default::default()
        }
    }

    fn tag(db: &Db, sir: &str, tag: &str) {
        db.conn
            .execute(
                "INSERT INTO entry_tags(sir_no, tag) VALUES (?1, ?2)",
                rusqlite::params![sir, tag],
            )
            .unwrap();
    }

    fn seed(db: &Db) {
        entry("AC-SIR-0001", 1, Some("2026-02-10"), "Grievance letter sent").insert(&db.conn).unwrap();
        entry("AC-SIR-0002", 2, Some("2026-03-15"), "OH referral appointment").insert(&db.conn).unwrap();
        entry("AC-SIR-0003", 3, Some("2026-04-02"), "Sick note issued").insert(&db.conn).unwrap();
        entry("AC-SIR-0004", 4, None, "Undated manuscript note").insert(&db.conn).unwrap();
        tag(db, "AC-SIR-0001", "grievance");
        tag(db, "AC-SIR-0001", "health");
        tag(db, "AC-SIR-0002", "health");
    }

    #[test]
    fn tag_filters_are_or_d() {
        let db = Db::open_memory().unwrap();
        seed(&db);
        let rows = collect_rows(
            &db.conn,
            &Filter { tags: vec!["grievance".into(), "health".into()], ..Default::default() },
        )
        .unwrap();
        let sirs: Vec<&str> = rows.iter().map(|r| r.entry.sir_no.as_str()).collect();
        assert_eq!(sirs, ["AC-SIR-0001", "AC-SIR-0002"]);

        let rows = collect_rows(
            &db.conn,
            &Filter { tags: vec!["Grievance".into()], ..Default::default() },
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tags, vec!["grievance", "health"]);
    }

    #[test]
    fn date_range_excludes_undated_and_out_of_range() {
        let db = Db::open_memory().unwrap();
        seed(&db);
        let rows = collect_rows(
            &db.conn,
            &Filter { from: Some("2026-03-01".into()), to: Some("2026-03-31".into()), ..Default::default() },
        )
        .unwrap();
        let sirs: Vec<&str> = rows.iter().map(|r| r.entry.sir_no.as_str()).collect();
        assert_eq!(sirs, ["AC-SIR-0002"]);
    }

    #[test]
    fn undated_sorts_last_and_dates_ascend() {
        let db = Db::open_memory().unwrap();
        seed(&db);
        let rows = collect_rows(&db.conn, &Filter::default()).unwrap();
        let sirs: Vec<&str> = rows.iter().map(|r| r.entry.sir_no.as_str()).collect();
        assert_eq!(sirs, ["AC-SIR-0001", "AC-SIR-0002", "AC-SIR-0003", "AC-SIR-0004"]);
    }

    #[test]
    fn prefix_filter_matches_case_insensitively() {
        let db = Db::open_memory().unwrap();
        seed(&db);
        entry("IG-SIR-0001", 1, Some("2026-03-20"), "Other prefix doc").insert(&db.conn).unwrap();
        let rows = collect_rows(&db.conn, &Filter { prefix: Some("ac".into()), ..Default::default() }).unwrap();
        assert!(rows.iter().all(|r| r.entry.prefix == "AC"));
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn renders_all_three_formats() {
        let db = Db::open_memory().unwrap();
        seed(&db);
        let rows = collect_rows(&db.conn, &Filter::default()).unwrap();

        let table = render_table(&rows);
        assert!(table.contains("SIR"));
        assert!(table.contains("AC-SIR-0001"));
        assert!(table.contains("10/02/2026")); // British display
        assert!(table.contains("undated"));
        assert!(table.contains("grievance, health"));

        let csv = render_csv(&rows).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "sir,date,type,description,sender,batch,tags");
        assert!(csv.contains("AC-SIR-0001,10/02/2026"));
        assert!(csv.contains("grievance;health"));

        let json = render_json(&rows).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 4);
        assert_eq!(parsed[0]["sir"], "AC-SIR-0001");
        assert_eq!(parsed[0]["date"], "10/02/2026");
        assert_eq!(parsed[0]["tags"].as_array().unwrap().len(), 2);
        assert!(parsed[3]["date"].is_null()); // undated
    }

    #[test]
    fn british_date_filters_parse() {
        let m = clap::Command::new("x")
            .subcommand(crate::command())
            .get_matches_from([
                "x", "chronology", "build",
                "--from", "01/03/2026",
                "--to", "31.3.26",
                "--tag", "Health",
                "--prefix", "ac",
            ]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("build", sm)) = cm.subcommand() else { panic!() };
        let f = filter_from_matches(sm).unwrap();
        assert_eq!(f.from.as_deref(), Some("2026-03-01"));
        assert_eq!(f.to.as_deref(), Some("2026-03-31"));
        assert_eq!(f.tags, vec!["health"]);
        assert_eq!(f.prefix.as_deref(), Some("AC"));
    }

    #[test]
    fn bad_british_date_is_user_error_not_panic() {
        let m = clap::Command::new("x")
            .subcommand(crate::command())
            .get_matches_from(["x", "chronology", "build", "--from", "2026-03-01"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("build", sm)) = cm.subcommand() else { panic!() };
        let err = filter_from_matches(sm).unwrap_err();
        assert!(err.contains("invalid --from date"));
        assert!(err.contains("2026-03-01"));

        let m = clap::Command::new("x")
            .subcommand(crate::command())
            .get_matches_from(["x", "chronology", "build", "--to", "31/02/2026"]);
        let Some(("chronology", cm)) = m.subcommand() else { panic!() };
        let Some(("build", sm)) = cm.subcommand() else { panic!() };
        assert!(filter_from_matches(sm).is_err());
    }

    #[test]
    fn register_export_has_full_columns() {
        let db = Db::open_memory().unwrap();
        seed(&db);
        let rows = collect_rows(&db.conn, &Filter::default()).unwrap();
        let csv = render_register_csv(&rows).unwrap();
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "sir,date,type,description,sender,recipients,batch,parent_sir,status,tags,sha256,standard_filename"
        );
        assert_eq!(lines.count(), 4);
    }
}
