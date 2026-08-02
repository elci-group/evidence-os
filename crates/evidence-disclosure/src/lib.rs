//! evidence-disclosure: Disclosure Control Tower.
//!
//! Manages disclosure as a live evidential process over the shared
//! `disclosure_items` table: recording requests, logging responses, listing,
//! deficiency flags, chase letters and deficiency schedules.
//!
//! Contract with the CLI shell:
//!   - `command()` returns the `disclosure` subcommand definition;
//!   - `try_dispatch()` executes it against the case database, returning
//!     `Ok(true)` when handled, `Ok(false)` when the subcommand is not ours.
//!
//! House rules: dates are stored ISO and displayed British (dd/mm/yyyy) via
//! `evidence_core::util`; every mutation writes an `audit_log` row.

mod flags;
mod item;
mod report;

use anyhow::{anyhow, bail, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};

use evidence_core::audit::audit;
use evidence_core::util;
use evidence_core::{Db, SirEntry};

use crate::item::DisclosureItem;

const ACTOR: &str = "disclosure";

const USAGE_ADD: &str = "usage: disclosure add --wording \"...\" [--issue \"...\"] [--recipient \"...\"] \
     [--requested DD/MM/YYYY] [--deadline-days N] [--bundle \"...\"] [--assists|--harms]";
const USAGE_LOG_RESPONSE: &str =
    "usage: disclosure log-response <ID> [--date DD/MM/YYYY] [--sir AC-SIR-0147] \
     [--objection \"...\"] [--partial]";
const USAGE_CHASE: &str = "usage: disclosure chase <ID>... [--out FILE]";

/// The `disclosure` subcommand definition (clap builder API).
pub fn command() -> Command {
    Command::new("disclosure")
        .about("Disclosure Control Tower — manage disclosure as a live evidential process")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("add")
                .about("Record a new disclosure request")
                .arg(
                    Arg::new("wording")
                        .long("wording")
                        .value_name("TEXT")
                        .help("Exact wording of the request"),
                )
                .arg(Arg::new("issue").long("issue").value_name("TEXT"))
                .arg(Arg::new("recipient").long("recipient").value_name("NAME"))
                .arg(
                    Arg::new("requested")
                        .long("requested")
                        .value_name("DD/MM/YYYY")
                        .help("Date the request was sent (British)"),
                )
                .arg(
                    Arg::new("deadline-days")
                        .long("deadline-days")
                        .value_name("N")
                        .value_parser(clap::value_parser!(i64))
                        .help("Days allowed for a response (default 14)"),
                )
                .arg(
                    Arg::new("bundle")
                        .long("bundle")
                        .value_name("REF")
                        .help("Bundle destination, e.g. PB-12"),
                )
                .arg(
                    Arg::new("assists")
                        .long("assists")
                        .action(ArgAction::SetTrue)
                        .conflicts_with("harms")
                        .help("This request is expected to assist the client"),
                )
                .arg(
                    Arg::new("harms")
                        .long("harms")
                        .action(ArgAction::SetTrue)
                        .help("This request is expected to harm the client"),
                ),
        )
        .subcommand(
            Command::new("log-response")
                .about("Record a response to a disclosure request")
                .arg(Arg::new("id").value_name("ID").help("Disclosure item id"))
                .arg(
                    Arg::new("date")
                        .long("date")
                        .value_name("DD/MM/YYYY")
                        .help("Date the response arrived (default: today)"),
                )
                .arg(
                    Arg::new("sir")
                        .long("sir")
                        .value_name("SIR")
                        .help("SIR of the supplied document, e.g. AC-SIR-0147"),
                )
                .arg(
                    Arg::new("objection")
                        .long("objection")
                        .value_name("TEXT")
                        .help("Objection raised by the respondent"),
                )
                .arg(
                    Arg::new("partial")
                        .long("partial")
                        .action(ArgAction::SetTrue)
                        .help("Response only partially answers the request"),
                ),
        )
        .subcommand(
            Command::new("list")
                .about("List disclosure requests")
                .arg(
                    Arg::new("status")
                        .long("status")
                        .value_name("STATUS")
                        .value_parser(["open", "answered", "partial", "objected", "closed"]),
                )
                .arg(
                    Arg::new("issue")
                        .long("issue")
                        .value_name("TEXT")
                        .help("Case-insensitive substring match on the issue field"),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .value_name("FMT")
                        .value_parser(["table", "csv"])
                        .default_value("table"),
                ),
        )
        .subcommand(
            Command::new("flags")
                .about("Auto-detect disclosure deficiencies and print them with item ids"),
        )
        .subcommand(
            Command::new("chase")
                .about("Generate a chase letter for outstanding requests")
                .arg(
                    Arg::new("ids")
                        .value_name("ID")
                        .num_args(1..)
                        .help("One or more disclosure item ids"),
                )
                .arg(
                    Arg::new("out")
                        .long("out")
                        .value_name("FILE")
                        .help("Write the letter to a file instead of stdout"),
                ),
        )
        .subcommand(
            Command::new("schedule")
                .about("Disclosure deficiency schedule: status, flags and bundle destination")
                .arg(
                    Arg::new("out")
                        .long("out")
                        .value_name("FILE")
                        .help("Write the schedule to a file instead of stdout"),
                ),
        )
}

/// Execute the `disclosure` subcommand. Returns `Ok(false)` when `sub` names a
/// different module's command.
pub fn try_dispatch(sub: Option<(&str, &ArgMatches)>, db: &Db) -> Result<bool> {
    let Some(("disclosure", m)) = sub else {
        return Ok(false);
    };
    match m.subcommand() {
        Some(("add", sm)) => cmd_add(sm, db)?,
        Some(("log-response", sm)) => cmd_log_response(sm, db)?,
        Some(("list", sm)) => cmd_list(sm, db)?,
        Some(("flags", _)) => println!("{}", report::flags_report(&db.conn)?),
        Some(("chase", sm)) => cmd_chase(sm, db)?,
        Some(("schedule", sm)) => cmd_schedule(sm, db)?,
        _ => eprintln!("{USAGE_ADD}\n{USAGE_LOG_RESPONSE}\n{USAGE_CHASE}"),
    }
    Ok(true)
}

/// Parse a British CLI date into ISO, naming the flag in any error.
fn parse_uk_date(flag: &str, value: &str) -> Result<String> {
    util::iso_from_uk_date(value)
        .ok_or_else(|| anyhow!("invalid {flag} date '{value}' — expected DD/MM/YYYY"))
}

fn cmd_add(m: &ArgMatches, db: &Db) -> Result<()> {
    let Some(wording) = m.get_one::<String>("wording") else {
        eprintln!("{USAGE_ADD}");
        return Ok(());
    };
    if wording.trim().is_empty() {
        eprintln!("{USAGE_ADD}");
        return Ok(());
    }

    let requested = m
        .get_one::<String>("requested")
        .map(|d| parse_uk_date("--requested", d))
        .transpose()?;
    let days = m.get_one::<i64>("deadline-days").copied().unwrap_or(14);
    let deadline = requested.as_deref().and_then(|d| util::add_days(d, days));
    if m.contains_id("deadline-days") && requested.is_none() {
        eprintln!("note: --deadline-days ignored because no --requested date was given");
    }

    let mut it = DisclosureItem::new(wording.trim());
    it.issue = m.get_one::<String>("issue").cloned();
    it.recipient = m.get_one::<String>("recipient").cloned();
    it.date_requested = requested;
    it.response_deadline = deadline;
    it.bundle_dest = m.get_one::<String>("bundle").cloned();
    it.assists_client = if m.get_flag("assists") {
        Some(1)
    } else if m.get_flag("harms") {
        Some(0)
    } else {
        None
    };

    let id = item::insert(&db.conn, &it)?;
    audit(&db.conn, ACTOR, "add", &format!("item {id}: {}", report_summary(wording)))?;
    println!("recorded disclosure request #{id}");
    if let Some(dl) = &it.response_deadline {
        println!(
            "response deadline: {}",
            util::uk_from_iso(dl).unwrap_or_else(|| dl.clone())
        );
    }
    Ok(())
}

fn cmd_log_response(m: &ArgMatches, db: &Db) -> Result<()> {
    let Some(id_str) = m.get_one::<String>("id") else {
        eprintln!("{USAGE_LOG_RESPONSE}");
        return Ok(());
    };
    let id: i64 = id_str
        .trim()
        .parse()
        .map_err(|_| anyhow!("invalid item id '{id_str}'"))?;
    if item::get(&db.conn, id)?.is_none() {
        bail!("no disclosure item with id {id}");
    }

    let date = match m.get_one::<String>("date") {
        Some(d) => parse_uk_date("--date", d)?,
        None => util::today_iso(),
    };
    let sir = m.get_one::<String>("sir").map(|s| s.trim().to_string());
    if let Some(s) = &sir {
        if SirEntry::get(&db.conn, s)?.is_none() {
            bail!("unknown SIR '{s}' — no such entry in the Master Chronology");
        }
    }
    let objection = m.get_one::<String>("objection").cloned();
    let status = if objection.is_some() {
        "objected"
    } else if m.get_flag("partial") {
        "partial"
    } else {
        "answered"
    };

    item::record_response(&db.conn, id, Some(&date), sir.as_deref(), status, objection.as_deref())?;
    audit(
        &db.conn,
        ACTOR,
        "log-response",
        &format!(
            "item {id}: status={status} date={date} sir={}",
            sir.as_deref().unwrap_or("-")
        ),
    )?;
    println!("item {id}: response logged as {status}");
    Ok(())
}

fn cmd_list(m: &ArgMatches, db: &Db) -> Result<()> {
    let items = item::list(
        &db.conn,
        m.get_one::<String>("status").map(String::as_str),
        m.get_one::<String>("issue").map(String::as_str),
    )?;
    match m.get_one::<String>("format").map(String::as_str) {
        Some("csv") => print!("{}", report::list_csv(&items)?),
        _ => println!("{}", report::list_table(&items)),
    }
    Ok(())
}

fn cmd_chase(m: &ArgMatches, db: &Db) -> Result<()> {
    let Some(values) = m.get_many::<String>("ids") else {
        eprintln!("{USAGE_CHASE}");
        return Ok(());
    };
    let mut ids = Vec::new();
    for v in values {
        ids.push(
            v.trim()
                .parse::<i64>()
                .map_err(|_| anyhow!("invalid item id '{v}'"))?,
        );
    }
    let (letter, used, skipped) = report::chase_letter(&db.conn, &ids)?;
    for id in &skipped {
        eprintln!("note: item {id} is no longer outstanding — skipped");
    }
    let dest = report::emit(m.get_one::<String>("out"), &letter)?;
    audit(
        &db.conn,
        ACTOR,
        "chase",
        &format!("chase letter for items {used:?} — {dest}"),
    )?;
    if m.get_one::<String>("out").is_some() {
        println!("chase letter {dest}");
    }
    Ok(())
}

fn cmd_schedule(m: &ArgMatches, db: &Db) -> Result<()> {
    let text = report::schedule(&db.conn)?;
    let dest = report::emit(m.get_one::<String>("out"), &text)?;
    audit(&db.conn, ACTOR, "schedule", &format!("deficiency schedule — {dest}"))?;
    if m.get_one::<String>("out").is_some() {
        println!("deficiency schedule {dest}");
    }
    Ok(())
}

/// One-line summary of request wording for audit detail.
fn report_summary(wording: &str) -> String {
    let mut chars = wording.trim().chars();
    let head: String = chars.by_ref().take(60).collect();
    if wording.trim().chars().count() > 60 {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evidence_core::SirEntry;

    /// Parse `args` (argv including "disclosure") and dispatch as the CLI shell would.
    fn dispatch(db: &Db, args: &[&str]) -> Result<bool> {
        let m = command().try_get_matches_from(args)?;
        try_dispatch(Some(("disclosure", &m)), db)
    }

    fn seed_sir(db: &Db, sir: &str, doc_type: Option<&str>, doc_date: Option<&str>) {
        SirEntry {
            sir_no: sir.into(),
            seq: 1,
            prefix: "AC".into(),
            doc_type: doc_type.map(Into::into),
            doc_date: doc_date.map(Into::into),
            status: "confirmed".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
    }

    fn audit_actions(db: &Db) -> Vec<(String, String)> {
        let mut stmt = db
            .conn
            .prepare("SELECT actor, action FROM audit_log ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn ignores_other_modules_subcommands() {
        let db = Db::open_memory().unwrap();
        let m = Command::new("shell")
            .subcommand(Command::new("harvest"))
            .try_get_matches_from(["shell", "harvest"])
            .unwrap();
        assert!(!try_dispatch(m.subcommand(), &db).unwrap());
        assert!(!try_dispatch(None, &db).unwrap());
    }

    #[test]
    fn add_and_log_response_lifecycle() {
        let db = Db::open_memory().unwrap();
        seed_sir(&db, "AC-SIR-0147", Some("Letter"), Some("2026-06-20"));

        // add: deadline = requested + 14 days, stored ISO.
        assert!(dispatch(
            &db,
            &[
                "disclosure", "add",
                "--wording", "All emails between J. Smith and HR attaching the March 2026 rota",
                "--issue", "rota",
                "--recipient", "Respondent's solicitors",
                "--requested", "01/06/2026",
                "--deadline-days", "14",
                "--bundle", "PB-12",
                "--assists",
            ],
        )
        .unwrap());
        let it = item::get(&db.conn, 1).unwrap().unwrap();
        assert_eq!(it.date_requested.as_deref(), Some("2026-06-01"));
        assert_eq!(it.response_deadline.as_deref(), Some("2026-06-15"));
        assert_eq!(it.status, "open");
        assert_eq!(it.bundle_dest.as_deref(), Some("PB-12"));
        assert_eq!(it.assists_client, Some(1));

        // log-response: links the supplied document, status answered.
        dispatch(
            &db,
            &[
                "disclosure", "log-response", "1",
                "--date", "14/06/2026",
                "--sir", "AC-SIR-0147",
            ],
        )
        .unwrap();
        let it = item::get(&db.conn, 1).unwrap().unwrap();
        assert_eq!(it.status, "answered");
        assert_eq!(it.response_received.as_deref(), Some("2026-06-14"));
        assert_eq!(it.supplied_sir.as_deref(), Some("AC-SIR-0147"));

        // every mutation audited under this module's name
        assert_eq!(
            audit_actions(&db),
            vec![
                ("disclosure".to_string(), "add".to_string()),
                ("disclosure".to_string(), "log-response".to_string()),
            ]
        );
    }

    #[test]
    fn add_defaults_to_14_day_deadline() {
        let db = Db::open_memory().unwrap();
        dispatch(
            &db,
            &["disclosure", "add", "--wording", "Occupational health file", "--requested", "01/07/2026"],
        )
        .unwrap();
        let it = item::get(&db.conn, 1).unwrap().unwrap();
        assert_eq!(it.response_deadline.as_deref(), Some("2026-07-15"));
        assert_eq!(it.assists_client, None);
    }

    #[test]
    fn log_response_status_precedence() {
        let db = Db::open_memory().unwrap();
        dispatch(&db, &["disclosure", "add", "--wording", "Request A"]).unwrap();
        dispatch(&db, &["disclosure", "add", "--wording", "Request B"]).unwrap();
        dispatch(&db, &["disclosure", "add", "--wording", "Request C"]).unwrap();

        // plain response -> answered, date defaults to today
        dispatch(&db, &["disclosure", "log-response", "1"]).unwrap();
        let it = item::get(&db.conn, 1).unwrap().unwrap();
        assert_eq!(it.status, "answered");
        assert_eq!(it.response_received.as_deref(), Some(util::today_iso().as_str()));

        // --partial -> partial
        dispatch(&db, &["disclosure", "log-response", "2", "--partial"]).unwrap();
        assert_eq!(item::get(&db.conn, 2).unwrap().unwrap().status, "partial");

        // --objection wins over --partial -> objected, reason stored
        dispatch(
            &db,
            &["disclosure", "log-response", "3", "--partial", "--objection", "privilege claimed"],
        )
        .unwrap();
        let it = item::get(&db.conn, 3).unwrap().unwrap();
        assert_eq!(it.status, "objected");
        assert_eq!(it.objection.as_deref(), Some("privilege claimed"));
    }

    #[test]
    fn log_response_unknown_sir_errors() {
        let db = Db::open_memory().unwrap();
        dispatch(&db, &["disclosure", "add", "--wording", "Request A"]).unwrap();
        let err = dispatch(
            &db,
            &["disclosure", "log-response", "1", "--sir", "AC-SIR-9999"],
        )
        .unwrap_err();
        assert!(err.to_string().contains("AC-SIR-9999"));
        // item untouched
        assert_eq!(item::get(&db.conn, 1).unwrap().unwrap().status, "open");
        assert!(audit_actions(&db).iter().all(|(_, a)| a == "add"));
    }

    #[test]
    fn log_response_unknown_item_errors() {
        let db = Db::open_memory().unwrap();
        let err = dispatch(&db, &["disclosure", "log-response", "42"]).unwrap_err();
        assert!(err.to_string().contains("42"));
    }

    #[test]
    fn add_missing_wording_prints_usage_not_panic() {
        let db = Db::open_memory().unwrap();
        assert!(dispatch(&db, &["disclosure", "add"]).unwrap());
        assert!(dispatch(&db, &["disclosure", "add", "--wording", "   "]).unwrap());
        assert!(item::list(&db.conn, None, None).unwrap().is_empty());
    }

    #[test]
    fn bad_british_dates_error() {
        let db = Db::open_memory().unwrap();
        let err = dispatch(
            &db,
            &["disclosure", "add", "--wording", "X", "--requested", "31/02/2026"],
        )
        .unwrap_err();
        assert!(err.to_string().contains("--requested"));
        dispatch(&db, &["disclosure", "add", "--wording", "X"]).unwrap();
        let err = dispatch(&db, &["disclosure", "log-response", "1", "--date", "2026-07-01"])
            .unwrap_err();
        assert!(err.to_string().contains("--date"));
    }

    #[test]
    fn list_filters_and_csv() {
        let db = Db::open_memory().unwrap();
        dispatch(
            &db,
            &["disclosure", "add", "--wording", "Rota documents", "--issue", "rota", "--requested", "01/06/2026"],
        )
        .unwrap();
        dispatch(
            &db,
            &["disclosure", "add", "--wording", "Health records", "--issue", "health"],
        )
        .unwrap();
        dispatch(&db, &["disclosure", "list", "--status", "open"]).unwrap();
        dispatch(&db, &["disclosure", "list", "--issue", "ROT"]).unwrap();
        dispatch(&db, &["disclosure", "list", "--format", "csv"]).unwrap();

        // filter logic at the data layer
        assert_eq!(item::list(&db.conn, Some("open"), None).unwrap().len(), 2);
        assert_eq!(item::list(&db.conn, None, Some("rota")).unwrap().len(), 1);
        let csv = report::list_csv(&item::list(&db.conn, None, None).unwrap()).unwrap();
        assert!(csv.lines().next().unwrap().starts_with("id,request_wording"));
        assert!(csv.contains("01/06/2026")); // British date in output
        assert_eq!(csv.lines().count(), 3);
    }

    #[test]
    fn flags_command_runs_clean_and_dirty() {
        let db = Db::open_memory().unwrap();
        dispatch(&db, &["disclosure", "flags"]).unwrap(); // empty db: all clear
        dispatch(
            &db,
            &["disclosure", "add", "--wording", "Old request", "--requested", "01/01/2020"],
        )
        .unwrap();
        let report = report::flags_report(&db.conn).unwrap();
        assert!(report.contains("REQUESTED-BUT-NOT-ANSWERED"));
        assert!(report.contains("item 1"));
    }

    #[test]
    fn chase_letter_contains_wording_and_british_dates() {
        let db = Db::open_memory().unwrap();
        dispatch(
            &db,
            &[
                "disclosure", "add",
                "--wording", "All emails between J. Smith and HR attaching the March 2026 rota",
                "--requested", "01/06/2020",
                "--deadline-days", "14",
            ],
        )
        .unwrap();
        let (letter, used, skipped) = report::chase_letter(&db.conn, &[1]).unwrap();
        assert_eq!(used, vec![1]);
        assert!(skipped.is_empty());
        assert!(letter.contains("1. All emails between J. Smith and HR attaching the March 2026 rota"));
        assert!(letter.contains("Date requested: 01/06/2020"));
        assert!(letter.contains("Response deadline: 15/06/2020"));
        assert!(letter.contains("overdue by"));
        assert!(letter.contains("supply the requested material immediately"));

        // dispatch path, writing to a file
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("chase.txt");
        dispatch(
            &db,
            &["disclosure", "chase", "1", "--out", out.to_str().unwrap()],
        )
        .unwrap();
        let written = std::fs::read_to_string(&out).unwrap();
        assert!(written.contains("All emails between J. Smith and HR"));
        assert!(written.contains("01/06/2020"));
        assert!(audit_actions(&db).iter().any(|(_, a)| a == "chase"));
    }

    #[test]
    fn chase_skips_answered_and_errors_when_nothing_outstanding() {
        let db = Db::open_memory().unwrap();
        dispatch(&db, &["disclosure", "add", "--wording", "Request A"]).unwrap();
        dispatch(&db, &["disclosure", "add", "--wording", "Request B"]).unwrap();
        dispatch(&db, &["disclosure", "log-response", "1"]).unwrap();

        let (_, used, skipped) = report::chase_letter(&db.conn, &[1, 2]).unwrap();
        assert_eq!(used, vec![2]);
        assert_eq!(skipped, vec![1]);

        let err = report::chase_letter(&db.conn, &[1]).unwrap_err();
        assert!(err.to_string().contains("nothing to chase"));

        let err = report::chase_letter(&db.conn, &[99]).unwrap_err();
        assert!(err.to_string().contains("99"));
    }

    #[test]
    fn schedule_lists_status_flags_and_bundle() {
        let db = Db::open_memory().unwrap();
        dispatch(
            &db,
            &[
                "disclosure", "add",
                "--wording", "Policy documents request",
                "--requested", "01/01/2020",
                "--bundle", "PB-12",
            ],
        )
        .unwrap();
        let text = report::schedule(&db.conn).unwrap();
        assert!(text.contains("DISCLOSURE DEFICIENCY SCHEDULE"));
        assert!(text.contains("PB-12"));
        assert!(text.contains("open"));
        assert!(text.contains("REQUESTED-BUT-NOT-ANSWERED"));
        assert!(text.contains("1 outstanding"));

        // --out path writes a file and audits
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("schedule.txt");
        dispatch(&db, &["disclosure", "schedule", "--out", out.to_str().unwrap()]).unwrap();
        assert!(std::fs::read_to_string(&out).unwrap().contains("PB-12"));
        assert!(audit_actions(&db).iter().any(|(_, a)| a == "schedule"));
    }

    #[test]
    fn file_backed_db_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("case.db");
        {
            let db = Db::open(&path).unwrap();
            dispatch(
                &db,
                &["disclosure", "add", "--wording", "Persisted request", "--requested", "02/07/2026"],
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let it = item::get(&db.conn, 1).unwrap().unwrap();
        assert_eq!(it.request_wording, "Persisted request");
        assert_eq!(it.response_deadline.as_deref(), Some("2026-07-16"));
    }
}
