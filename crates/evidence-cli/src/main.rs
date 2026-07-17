//! evidenceos — SAS EvidenceOS command-line shell.
//!
//! Module contract (implemented by each `evidence-<module>` crate):
//!   - `pub fn command() -> clap::Command`
//!       the module's top-level subcommand (fixed name per module)
//!   - `pub fn try_dispatch(
//!         sub: Option<(&str, &clap::ArgMatches)>,
//!         db: &evidence_core::Db,
//!     ) -> anyhow::Result<bool>`
//!       returns Ok(true) if it handled the subcommand.
//!
//! Wiring a new module = one `.subcommand(...)` line + one `try_dispatch` line
//! in the two marked sections below. Nothing else in this file changes.

use anyhow::bail;
use clap::{Arg, ArgMatches, Command};
use evidence_core::sir;
use evidence_core::util;
use evidence_core::{Db, SirEntry};

fn main() -> anyhow::Result<()> {
    let mut cmd = Command::new("evidenceos")
        .version(env!("CARGO_PKG_VERSION"))
        .about("SAS EvidenceOS — local-first evidence & litigation preparation toolkit")
        .arg(
            Arg::new("db")
                .long("db")
                .help("Path to case database")
                .default_value("evidence.db")
                .global(true),
        )
        .subcommand(Command::new("init").about("Create (if needed) and migrate the case database"))
        .subcommand(
            Command::new("sir")
                .about("Master Chronology register operations")
                .subcommand_required(true)
                .subcommand(
                    Command::new("next")
                        .about("Show the next SIR number without consuming it")
                        .arg(Arg::new("prefix").long("prefix").default_value("AC")),
                )
                .subcommand(
                    Command::new("list")
                        .about("List Master Chronology entries")
                        .arg(Arg::new("prefix").long("prefix")),
                ),
        );

    // === MODULE REGISTRATION (one line per module crate) ===
    cmd = cmd
        .subcommand(evidence_harvester::command())
        .subcommand(evidence_chronology::command())
        .subcommand(evidence_disclosure::command())
        .subcommand(evidence_sentinel::command())
        .subcommand(evidence_graph::command())
        .subcommand(evidence_auditor::command())
        .subcommand(evidence_meetings::command())
        .subcommand(evidence_deadlines::command())
        .subcommand(evidence_drafting::command())
        .subcommand(evidence_cockpit::command());

    let matches = cmd.get_matches();
    let db_path = matches.get_one::<String>("db").unwrap();
    let db = Db::open(db_path)?;

    match matches.subcommand() {
        Some(("init", _)) => println!("database ready: {db_path}"),
        Some(("sir", sub)) => sir_cmd(sub, &db)?,
        other => {
            if !dispatch_modules(other, &db)? {
                bail!("unknown command — see `evidenceos --help`");
            }
        }
    }
    Ok(())
}

fn sir_cmd(sub: &ArgMatches, db: &Db) -> anyhow::Result<()> {
    match sub.subcommand() {
        Some(("next", m)) => {
            let prefix = m.get_one::<String>("prefix").unwrap();
            println!("{}", sir::peek_next_sir(&db.conn, prefix)?);
        }
        Some(("list", m)) => {
            let prefix = m.get_one::<String>("prefix");
            for e in SirEntry::list(&db.conn)? {
                if let Some(p) = prefix {
                    if &e.prefix != p {
                        continue;
                    }
                }
                println!(
                    "{} | {} | {} | {} | {} | {}",
                    e.sir_no,
                    e.doc_date
                        .as_deref()
                        .and_then(util::uk_from_iso)
                        .unwrap_or_else(|| "undated".into()),
                    e.doc_type.as_deref().unwrap_or("-"),
                    e.description.as_deref().unwrap_or("-"),
                    e.sender.as_deref().unwrap_or("-"),
                    e.source_batch.as_deref().unwrap_or("-"),
                );
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

// === MODULE DISPATCH (one if-block per module crate) ===
fn dispatch_modules(sub: Option<(&str, &ArgMatches)>, db: &Db) -> anyhow::Result<bool> {
    if evidence_harvester::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_chronology::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_disclosure::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_sentinel::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_graph::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_auditor::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_meetings::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_deadlines::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_drafting::try_dispatch(sub, db)? {
        return Ok(true);
    }
    if evidence_cockpit::try_dispatch(sub, db)? {
        return Ok(true);
    }
    Ok(false)
}
