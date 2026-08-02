//! evidence-harvester: SIR Harvester.
//!
//! Walks a folder of incoming evidence (emails, PDFs, Word docs, images,
//! text files) and turns each item into a proposed Master Chronology row.
//! Dry-run by default: without `--write` no SIRs are allocated, nothing is
//! inserted and `audit_log` stays untouched — the proposed rows are only
//! printed. With `--write` each non-duplicate row is allocated a SIR,
//! inserted as a draft `SirEntry` and audited.
//!
//! Duplicate detection is by SHA-256, both against `sir_entries.sha256`
//! (already-harvested evidence) and within the batch itself. Duplicates are
//! listed as `DUPLICATE` rows but are never allocated a SIR nor inserted;
//! attachments of a duplicate email are treated as duplicates of it.
//!
//! `.eml` attachments become child rows (`parent_sir` = the email's SIR).
//! `--extract-attachments` and `--csv` write files whenever requested, in
//! both dry-run and write mode; the database is only ever written with
//! `--write`.
//!
//! Date fallback order: content date -> date in filename -> file mtime
//! (for attachments: the parent email's date) -> None.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Arg, ArgAction, ArgMatches, Command};
use rusqlite::OptionalExtension;
use walkdir::WalkDir;

use evidence_core::audit::audit;
use evidence_core::sir;
use evidence_core::util;
use evidence_core::{Db, SirEntry};

mod email;
mod scan;

/// Module name recorded in `audit_log`.
const ACTOR: &str = "harvester";

/// Everything `harvest <DIR>` was invoked with.
pub(crate) struct HarvestConfig {
    dir: PathBuf,
    prefix: String,
    batch: Option<String>,
    root_source: Option<String>,
    write: bool,
    extract_attachments: Option<PathBuf>,
    csv: Option<PathBuf>,
}

/// Why a row is a duplicate.
#[derive(Debug)]
pub(crate) enum DupOf {
    /// Hash already in `sir_entries` — holds the existing SIR.
    Db(String),
    /// Same hash seen earlier in this batch — holds the earlier row's index.
    Batch(usize),
}

/// One proposed (or written) chronology row: a walked file or an email
/// attachment.
pub(crate) struct HarvestRow {
    original_filename: String,
    ext: String,
    doc_type: String,
    doc_date: Option<String>,
    description: String,
    sender: Option<String>,
    recipients: Option<String>,
    sha256: String,
    /// Index of the parent email row for attachments.
    parent: Option<usize>,
    /// Attachment payload awaiting optional extraction.
    attachment: Option<email::EmailAttachment>,
    file_path: String,
    dup_of: Option<DupOf>,
    // Assigned by `finalize`:
    sir: Option<String>,
    parent_sir: Option<String>,
    standard_filename: Option<String>,
}

impl HarvestRow {
    fn is_duplicate(&self) -> bool {
        self.dup_of.is_some()
    }
}

/// Module contract: the `harvest` subcommand definition.
pub fn command() -> Command {
    Command::new("harvest")
        .about("Harvest a folder of incoming evidence into draft Master Chronology entries")
        .arg(
            Arg::new("dir")
                .help("Folder to walk recursively")
                .value_name("DIR"),
        )
        .arg(
            Arg::new("prefix")
                .long("prefix")
                .help("SIR prefix")
                .value_name("PREFIX")
                .default_value("AC"),
        )
        .arg(
            Arg::new("batch")
                .long("batch")
                .help("Source batch label, e.g. PB-10")
                .value_name("BATCH"),
        )
        .arg(
            Arg::new("root-source")
                .long("root-source")
                .help("Where the evidence ultimately came from")
                .value_name("TEXT"),
        )
        .arg(
            Arg::new("write")
                .long("write")
                .help("Allocate SIRs and insert draft rows (default: dry-run)")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("extract-attachments")
                .long("extract-attachments")
                .help("Write email attachment bytes to DIR (never overwrites)")
                .value_name("DIR"),
        )
        .arg(
            Arg::new("csv")
                .long("csv")
                .help("Also write the proposed rows to FILE as CSV")
                .value_name("FILE"),
        )
}

/// Module contract: handle `harvest` if that is the subcommand given.
pub fn try_dispatch(sub: Option<(&str, &ArgMatches)>, db: &Db) -> anyhow::Result<bool> {
    let Some(("harvest", m)) = sub else {
        return Ok(false);
    };
    let Some(dir) = m.get_one::<String>("dir") else {
        // Missing required arg: show usage, do not panic or error-exit.
        command().print_help()?;
        println!();
        return Ok(true);
    };
    let cfg = HarvestConfig {
        dir: PathBuf::from(dir),
        prefix: m
            .get_one::<String>("prefix")
            .cloned()
            .unwrap_or_else(|| "AC".into()),
        batch: m.get_one::<String>("batch").cloned(),
        root_source: m.get_one::<String>("root-source").cloned(),
        write: m.get_flag("write"),
        extract_attachments: m.get_one::<String>("extract-attachments").map(PathBuf::from),
        csv: m.get_one::<String>("csv").map(PathBuf::from),
    };
    run(&cfg, db)?;
    Ok(true)
}

/// Harvest end to end: gather, assign, print. Returns the rows for tests.
pub(crate) fn run(cfg: &HarvestConfig, db: &Db) -> anyhow::Result<Vec<HarvestRow>> {
    anyhow::ensure!(
        cfg.dir.is_dir(),
        "harvest source {} is not a directory",
        cfg.dir.display()
    );
    let mut rows = gather(cfg, db)?;
    finalize(cfg, db, &mut rows)?;
    print_rows(cfg, &rows)?;
    if let Some(csv_path) = &cfg.csv {
        write_csv(csv_path, cfg, &rows)?;
    }
    Ok(rows)
}

/// Walk the folder, extract metadata, hash and classify every file (and
/// every email attachment) into a proposed row, flagging duplicates.
fn gather(cfg: &HarvestConfig, db: &Db) -> anyhow::Result<Vec<HarvestRow>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(&cfg.dir) {
        let entry = entry.with_context(|| format!("walking {}", cfg.dir.display()))?;
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }
    files.sort();

    let mut rows: Vec<HarvestRow> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for path in files {
        let original_filename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = extension_of(&path);
        let sha256 = util::sha256_file(&path)?;
        let file_path = path.to_string_lossy().to_string();

        if ext == "eml" {
            let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            match email::parse_eml(&bytes) {
                Ok(parsed) => {
                    let parent_idx = rows.len();
                    let date = parsed
                        .date
                        .or_else(|| util::extract_date_from_text(&original_filename))
                        .or_else(|| scan::mtime_date(&path));
                    let description = parsed
                        .subject
                        .unwrap_or_else(|| file_stem(&original_filename));
                    rows.push(HarvestRow {
                        original_filename: original_filename.clone(),
                        ext: ext.clone(),
                        doc_type: "Email".into(),
                        doc_date: date.clone(),
                        description,
                        sender: parsed.sender.clone(),
                        recipients: parsed.recipients.clone(),
                        sha256: sha256.clone(),
                        parent: None,
                        attachment: None,
                        file_path: file_path.clone(),
                        dup_of: dup_of(db, &seen, &sha256)?,
                        sir: None,
                        parent_sir: None,
                        standard_filename: None,
                    });
                    seen.insert(sha256, parent_idx);
                    for att in parsed.attachments {
                        let att_ext = attachment_extension(&att.filename);
                        let att_hash = util::sha256_bytes(&att.data);
                        let att_date = scan::content_date_bytes(&att.data, &att_ext)
                            .or_else(|| util::extract_date_from_text(&att.filename))
                            .or(date.clone());
                        let idx = rows.len();
                        // Bytes live inside the email until optional extraction.
                        let pseudo_path = format!("{file_path}#{}", att.filename);
                        rows.push(HarvestRow {
                            original_filename: att.filename.clone(),
                            ext: att_ext,
                            doc_type: scan::classify(&attachment_extension(&att.filename)).into(),
                            doc_date: att_date,
                            description: file_stem(&att.filename),
                            sender: parsed.sender.clone(),
                            recipients: parsed.recipients.clone(),
                            sha256: att_hash.clone(),
                            parent: Some(parent_idx),
                            attachment: Some(att),
                            file_path: pseudo_path,
                            dup_of: dup_of(db, &seen, &att_hash)?,
                            sir: None,
                            parent_sir: None,
                            standard_filename: None,
                        });
                        seen.insert(att_hash, idx);
                    }
                }
                Err(e) => {
                    // Unparseable email: still harvested as a plain Email row.
                    eprintln!("warning: {}: {e:#}; treating as undated email", path.display());
                    rows.push(HarvestRow {
                        original_filename: original_filename.clone(),
                        ext: ext.clone(),
                        doc_type: "Email".into(),
                        doc_date: util::extract_date_from_text(&original_filename)
                            .or_else(|| scan::mtime_date(&path)),
                        description: file_stem(&original_filename),
                        sender: None,
                        recipients: None,
                        sha256: sha256.clone(),
                        parent: None,
                        attachment: None,
                        file_path,
                        dup_of: dup_of(db, &seen, &sha256)?,
                        sir: None,
                        parent_sir: None,
                        standard_filename: None,
                    });
                    seen.insert(sha256, rows.len() - 1);
                }
            }
        } else {
            let date = scan::content_date(&path, &ext)
                .or_else(|| util::extract_date_from_text(&original_filename))
                .or_else(|| scan::mtime_date(&path));
            rows.push(HarvestRow {
                original_filename: original_filename.clone(),
                ext,
                doc_type: scan::classify(&extension_of(&path)).into(),
                doc_date: date,
                description: file_stem(&original_filename),
                sender: None,
                recipients: None,
                sha256: sha256.clone(),
                parent: None,
                attachment: None,
                file_path,
                dup_of: dup_of(db, &seen, &sha256)?,
                sir: None,
                parent_sir: None,
                standard_filename: None,
            });
            seen.insert(sha256, rows.len() - 1);
        }
    }
    Ok(rows)
}

/// Database hit wins; otherwise an earlier same-batch row, if any.
fn dup_of(db: &Db, seen: &HashMap<String, usize>, sha256: &str) -> anyhow::Result<Option<DupOf>> {
    let existing: Option<String> = db
        .conn
        .query_row(
            "SELECT sir_no FROM sir_entries WHERE sha256 = ?1 LIMIT 1",
            [sha256],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(sir_no) = existing {
        return Ok(Some(DupOf::Db(sir_no)));
    }
    Ok(seen.get(sha256).copied().map(DupOf::Batch))
}

/// Assign SIRs (real with `--write`, simulated from `peek_next_sir` in
/// dry-run), extract attachments when asked, insert and audit draft rows.
fn finalize(cfg: &HarvestConfig, db: &Db, rows: &mut [HarvestRow]) -> anyhow::Result<()> {
    let mut sim_next: Option<u32> = None; // dry-run SIR preview counter
    for i in 0..rows.len() {
        if rows[i].is_duplicate() {
            continue;
        }
        // Attachments of a duplicate email are already in the evidence store.
        if let Some(p) = rows[i].parent {
            if rows[p].is_duplicate() {
                rows[i].dup_of = Some(DupOf::Batch(p));
                continue;
            }
        }

        let sir_no = if cfg.write {
            sir::allocate_sir(&db.conn, &cfg.prefix)?
        } else {
            let next = match sim_next {
                Some(n) => n,
                None => sir::parse_sir(&sir::peek_next_sir(&db.conn, &cfg.prefix)?)
                    .map(|(_, n)| n)
                    .unwrap_or(1),
            };
            sim_next = Some(next + 1);
            format!("{}-SIR-{next:04}", cfg.prefix)
        };
        let seq = sir::parse_sir(&sir_no).map(|(_, n)| n as i64).unwrap_or(0);
        rows[i].parent_sir = rows[i]
            .parent
            .and_then(|p| rows[p].sir.clone());
        rows[i].standard_filename = Some(sir::standard_filename(
            &rows[i].doc_type,
            &sir_no,
            cfg.batch.as_deref(),
            rows[i].doc_date.as_deref(),
            &rows[i].description,
            &rows[i].ext,
        ));

        if let (Some(att), Some(dir)) = (&rows[i].attachment, &cfg.extract_attachments) {
            fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
            let target = collision_free_path(dir, &att.filename);
            fs::write(&target, &att.data)
                .with_context(|| format!("writing {}", target.display()))?;
            rows[i].file_path = target.to_string_lossy().to_string();
            if cfg.write {
                audit(
                    &db.conn,
                    ACTOR,
                    "extract-attachment",
                    &format!("{sir_no} -> {}", target.display()),
                )?;
            }
        }
        rows[i].sir = Some(sir_no.clone());

        if cfg.write {
            SirEntry {
                sir_no: sir_no.clone(),
                seq,
                prefix: cfg.prefix.clone(),
                doc_date: rows[i].doc_date.clone(),
                doc_type: Some(rows[i].doc_type.clone()),
                description: Some(rows[i].description.clone()),
                sender: rows[i].sender.clone(),
                recipients: rows[i].recipients.clone(),
                root_source: cfg.root_source.clone(),
                source_batch: cfg.batch.clone(),
                original_filename: Some(rows[i].original_filename.clone()),
                standard_filename: rows[i].standard_filename.clone(),
                file_path: Some(rows[i].file_path.clone()),
                sha256: Some(rows[i].sha256.clone()),
                parent_sir: rows[i].parent_sir.clone(),
                status: "draft".into(),
                ..Default::default()
            }
            .insert(&db.conn)?;
            audit(
                &db.conn,
                ACTOR,
                "insert",
                &format!(
                    "{sir_no} draft {} '{}' <- {}",
                    rows[i].doc_type, rows[i].description, rows[i].file_path
                ),
            )?;
        }
    }
    Ok(())
}

/// Pipe-separated copy-paste rows on stdout; a one-line summary on stderr.
fn print_rows(cfg: &HarvestConfig, rows: &[HarvestRow]) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut duplicates = 0usize;
    for row in rows.iter() {
        let date = row
            .doc_date
            .as_deref()
            .and_then(util::uk_from_iso)
            .unwrap_or_else(|| "undated".into());
        let sender = row.sender.as_deref().unwrap_or("-");
        let batch = cfg.batch.as_deref().unwrap_or("-");
        if let Some(dup) = &row.dup_of {
            duplicates += 1;
            let of = match dup {
                DupOf::Db(sir_no) => sir_no.clone(),
                DupOf::Batch(p) => rows[*p]
                    .sir
                    .clone()
                    .unwrap_or_else(|| rows[*p].original_filename.clone()),
            };
            writeln!(
                out,
                "DUPLICATE | {date} | {} | {} | {sender} | {batch} | duplicate of {of}",
                row.doc_type, row.description
            )?;
        } else {
            let sir_no = row.sir.as_deref().unwrap_or("?");
            writeln!(
                out,
                "{sir_no} | {date} | {} | {} | {sender} | {batch}",
                row.doc_type, row.description
            )?;
        }
    }
    let verb = if cfg.write { "written" } else { "dry-run" };
    eprintln!(
        "{verb}: {} proposed, {duplicates} duplicate(s) skipped",
        rows.len() - duplicates
    );
    Ok(())
}

/// The same rows as CSV: everything needed to audit or re-key the harvest.
fn write_csv(path: &Path, cfg: &HarvestConfig, rows: &[HarvestRow]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut w = csv::Writer::from_path(path)
        .with_context(|| format!("writing CSV {}", path.display()))?;
    w.write_record([
        "sir_no",
        "status",
        "doc_date",
        "doc_type",
        "description",
        "sender",
        "recipients",
        "source_batch",
        "root_source",
        "original_filename",
        "standard_filename",
        "file_path",
        "sha256",
        "parent_sir",
    ])?;
    for row in rows {
        w.write_record([
            row.sir.as_deref().unwrap_or(""),
            if row.is_duplicate() { "duplicate" } else { "draft" },
            row.doc_date.as_deref().unwrap_or(""),
            &row.doc_type,
            &row.description,
            row.sender.as_deref().unwrap_or(""),
            row.recipients.as_deref().unwrap_or(""),
            cfg.batch.as_deref().unwrap_or(""),
            cfg.root_source.as_deref().unwrap_or(""),
            &row.original_filename,
            row.standard_filename.as_deref().unwrap_or(""),
            &row.file_path,
            &row.sha256,
            row.parent_sir.as_deref().unwrap_or(""),
        ])?;
    }
    w.flush()?;
    Ok(())
}

/// `dir/name.ext`, or `dir/name-2.ext`, `-3`... on collision. Never
/// overwrites an existing file.
fn collision_free_path(dir: &Path, filename: &str) -> PathBuf {
    let safe = safe_filename(filename);
    let candidate = dir.join(&safe);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match safe.rfind('.') {
        Some(dot) if dot > 0 => (safe[..dot].to_string(), safe[dot..].to_string()),
        _ => (safe.clone(), String::new()),
    };
    let mut n = 2u32;
    loop {
        let candidate = dir.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Make an attachment filename safe to write: no path components, no
/// filename-hostile characters.
fn safe_filename(name: &str) -> String {
    let base = name.replace('\\', "/");
    let base = base.rsplit('/').next().unwrap_or("attachment");
    let cleaned = sir::sanitize_description(base).replace(' ', "_");
    // sanitize_description eats path-hostile chars but keeps dots; a name that
    // collapses to nothing (or only dots) must still be writable.
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "attachment".into()
    } else {
        cleaned
    }
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn attachment_extension(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .filter(|e| !e.is_empty() && e.len() <= 10)
        .unwrap_or_default()
}

fn file_stem(filename: &str) -> String {
    match filename.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => filename.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_EML: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/covering-email.eml");

    fn cfg_for(dir: &Path) -> HarvestConfig {
        HarvestConfig {
            dir: dir.to_path_buf(),
            prefix: "AC".into(),
            batch: Some("PB-10".into()),
            root_source: Some("client drop".into()),
            write: false,
            extract_attachments: None,
            csv: None,
        }
    }

    fn count(db: &Db, table: &str) -> i64 {
        db.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn dispatch_ignores_other_subcommands() {
        let db = Db::open_memory().unwrap();
        assert!(!try_dispatch(None, &db).unwrap());
        let m = command().get_matches_from(vec!["harvest"]);
        assert!(!try_dispatch(Some(("sentinel", &m)), &db).unwrap());
    }

    #[test]
    fn dispatch_without_dir_prints_usage_ok() {
        let db = Db::open_memory().unwrap();
        let m = command().get_matches_from(vec!["harvest"]);
        assert!(try_dispatch(Some(("harvest", &m)), &db).unwrap());
    }

    #[test]
    fn dry_run_allocates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(FIXTURE_EML, dir.path().join("covering.eml")).unwrap();
        fs::write(dir.path().join("note.txt"), "statement taken 18.06.2026").unwrap();
        let db = Db::open_memory().unwrap();

        let rows = run(&cfg_for(dir.path()), &db).unwrap();

        // email + attachment + note = 3 proposed rows with preview SIRs
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].sir.as_deref(), Some("AC-SIR-0001"));
        assert_eq!(rows[1].sir.as_deref(), Some("AC-SIR-0002"));
        assert_eq!(rows[1].parent_sir.as_deref(), Some("AC-SIR-0001"));
        assert_eq!(rows[2].sir.as_deref(), Some("AC-SIR-0003"));
        // but nothing reached the database
        assert_eq!(count(&db, "sir_entries"), 0);
        assert_eq!(count(&db, "audit_log"), 0);
        assert_eq!(count(&db, "meta"), 0);
        assert_eq!(
            sir::peek_next_sir(&db.conn, "AC").unwrap(),
            "AC-SIR-0001",
            "dry-run must not consume SIR numbers"
        );
    }

    #[test]
    fn write_inserts_parent_and_child_rows() {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(FIXTURE_EML, dir.path().join("covering.eml")).unwrap();
        let db = Db::open_memory().unwrap();
        let mut cfg = cfg_for(dir.path());
        cfg.write = true;

        let rows = run(&cfg, &db).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(count(&db, "sir_entries"), 2);

        let parent = SirEntry::get(&db.conn, "AC-SIR-0001").unwrap().unwrap();
        assert_eq!(parent.doc_type.as_deref(), Some("Email"));
        assert_eq!(parent.doc_date.as_deref(), Some("2026-07-03"));
        assert_eq!(parent.sender.as_deref(), Some("Sally-Ann Spring"));
        assert_eq!(parent.status, "draft");
        assert_eq!(parent.source_batch.as_deref(), Some("PB-10"));
        assert_eq!(parent.root_source.as_deref(), Some("client drop"));
        assert_eq!(parent.original_filename.as_deref(), Some("covering.eml"));
        assert!(parent.parent_sir.is_none());
        assert_eq!(
            parent.standard_filename.as_deref(),
            Some(sir::standard_filename(
                "Email",
                "AC-SIR-0001",
                Some("PB-10"),
                Some("2026-07-03"),
                "Covering email supplying CrowdJustice contact details",
                "eml"
            ))
            .as_deref()
        );
        assert_eq!(
            parent.sha256.as_deref(),
            Some(util::sha256_file(dir.path().join("covering.eml")).unwrap()).as_deref()
        );

        let child = SirEntry::get(&db.conn, "AC-SIR-0002").unwrap().unwrap();
        assert_eq!(child.parent_sir.as_deref(), Some("AC-SIR-0001"));
        assert_eq!(child.doc_type.as_deref(), Some("Text"));
        assert_eq!(
            child.original_filename.as_deref(),
            Some("crowdjustice-contacts.txt")
        );
        // no date anywhere: falls back to the parent email's date
        assert_eq!(child.doc_date.as_deref(), Some("2026-07-03"));
        assert_eq!(
            child.standard_filename.as_deref(),
            Some(sir::standard_filename(
                "Text",
                "AC-SIR-0002",
                Some("PB-10"),
                Some("2026-07-03"),
                "crowdjustice-contacts",
                "txt"
            ))
            .as_deref()
        );

        // one audit row per insert
        let audits: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE actor = 'harvester' AND action = 'insert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audits, 2);
    }

    #[test]
    fn duplicate_same_file_twice_second_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"identical bytes, no date";
        fs::write(dir.path().join("first.txt"), body).unwrap();
        fs::write(dir.path().join("second.txt"), body).unwrap();
        let db = Db::open_memory().unwrap();
        let mut cfg = cfg_for(dir.path());
        cfg.write = true;

        let rows = run(&cfg, &db).unwrap();

        assert_eq!(rows.len(), 2);
        assert!(rows[0].dup_of.is_none(), "first occurrence must be kept");
        assert!(
            matches!(rows[1].dup_of, Some(DupOf::Batch(0))),
            "second occurrence must duplicate the first, got {:?}",
            rows[1].dup_of
        );
        assert!(rows[1].sir.is_none(), "duplicates get no SIR");
        assert_eq!(count(&db, "sir_entries"), 1, "only one row inserted");
    }

    #[test]
    fn duplicate_against_database_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("doc.txt"), "already harvested").unwrap();
        let db = Db::open_memory().unwrap();
        let mut cfg = cfg_for(dir.path());
        cfg.write = true;

        run(&cfg, &db).unwrap();
        assert_eq!(count(&db, "sir_entries"), 1);

        // harvesting the same folder again must not insert or allocate
        let rows = run(&cfg, &db).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            matches!(&rows[0].dup_of, Some(DupOf::Db(s)) if s == "AC-SIR-0001"),
            "expected duplicate of AC-SIR-0001, got {:?}",
            rows[0].dup_of
        );
        assert_eq!(count(&db, "sir_entries"), 1);
        assert_eq!(count(&db, "audit_log"), 1, "no new audit rows");
        assert_eq!(sir::peek_next_sir(&db.conn, "AC").unwrap(), "AC-SIR-0002");
    }

    #[test]
    fn filename_date_used_when_content_has_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Letter to employer 03.07.2026.txt"),
            "no date in this body",
        )
        .unwrap();
        let db = Db::open_memory().unwrap();

        let rows = run(&cfg_for(dir.path()), &db).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].doc_date.as_deref(), Some("2026-07-03"));
        assert_eq!(rows[0].doc_type, "Text");
    }

    #[test]
    fn content_date_beats_filename_date() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Letter 03.07.2026.txt"),
            "actually written on 01/02/2026 despite the name",
        )
        .unwrap();
        let db = Db::open_memory().unwrap();

        let rows = run(&cfg_for(dir.path()), &db).unwrap();

        assert_eq!(rows[0].doc_date.as_deref(), Some("2026-02-01"));
    }

    #[test]
    fn extract_attachments_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(FIXTURE_EML, dir.path().join("covering.eml")).unwrap();
        let out = tempfile::tempdir().unwrap();
        // pre-existing file forces the -2 suffix
        fs::write(out.path().join("crowdjustice-contacts.txt"), b"occupied").unwrap();
        let db = Db::open_memory().unwrap();
        let mut cfg = cfg_for(dir.path());
        cfg.write = true;
        cfg.extract_attachments = Some(out.path().to_path_buf());

        let rows = run(&cfg, &db).unwrap();

        let extracted = out.path().join("crowdjustice-contacts-2.txt");
        assert!(extracted.exists(), "expected {}", extracted.display());
        assert_eq!(
            fs::read_to_string(&extracted).unwrap(),
            "CrowdJustice contacts: jane@example.org, 01234 567890\n"
        );
        // original file untouched
        assert_eq!(fs::read(out.path().join("crowdjustice-contacts.txt")).unwrap(), b"occupied");
        // child row's file_path points at the extracted copy and was audited
        assert_eq!(rows[1].file_path, extracted.to_string_lossy());
        let extract_audits: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'extract-attachment'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(extract_audits, 1);
        // ... and that is what landed in the database
        let child = SirEntry::get(&db.conn, "AC-SIR-0002").unwrap().unwrap();
        assert_eq!(child.file_path.as_deref(), Some(extracted.to_string_lossy().as_ref()));
    }

    #[test]
    fn csv_written_in_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello, undated").unwrap();
        let csv_path = dir.path().join("out").join("harvest.csv");
        let db = Db::open_memory().unwrap();
        let mut cfg = cfg_for(dir.path());
        cfg.csv = Some(csv_path.clone());

        run(&cfg, &db).unwrap();

        let text = fs::read_to_string(&csv_path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "header + one row");
        assert!(lines[0].starts_with("sir_no,status,doc_date,"));
        assert!(lines[1].starts_with("AC-SIR-0001,draft,"), "row: {}", lines[1]);
    }

    #[test]
    fn missing_dir_is_an_error_not_a_panic() {
        let db = Db::open_memory().unwrap();
        let m = command().get_matches_from(vec!["harvest", "/no/such/dir/anywhere"]);
        let result = try_dispatch(Some(("harvest", &m)), &db);
        assert!(result.is_err());
    }
}
