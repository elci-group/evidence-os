//! evidence-chronology: Chronology Composer.
//!
//! Many views over ONE Master Chronology (`sir_entries` + `entry_tags`).
//! One event record, many views — rows are never duplicated:
//! `tag`/`untag` slice entries into ad-hoc views, `build` renders filtered
//! chronologies (table/csv/json), `views` lists the preset slices, `check`
//! runs a deterministic integrity scan, `export` writes the full register
//! for counsel/ET bundle use.
//!
//! Dates are stored ISO (`yyyy-mm-dd`) and displayed British (`dd/mm/yyyy`),
//! always via `evidence_core::util` helpers. Every mutating operation is
//! recorded in `audit_log` with actor `chronology`.

mod build;
mod check;
mod tags;
mod views;

use anyhow::Result;
use clap::{Arg, ArgAction, ArgMatches, Command};
use evidence_core::Db;

/// audit_log actor for every mutation this module performs.
const ACTOR: &str = "chronology";

pub fn command() -> Command {
    let sir_arg = Arg::new("sir")
        .help("SIR number, e.g. AC-SIR-0147")
        .value_name("SIR");
    let tags_arg = Arg::new("tags")
        .help("one or more tags (lowercase; e.g. health, grievance, witness:sally)")
        .value_name("TAG")
        .num_args(1..);

    Command::new("chronology")
        .about("Chronology Composer — many views over the one Master Chronology")
        .subcommand(
            Command::new("tag")
                .about("Add tag(s) to an entry")
                .arg(sir_arg.clone())
                .arg(tags_arg.clone()),
        )
        .subcommand(
            Command::new("untag")
                .about("Remove tag(s) from an entry")
                .arg(sir_arg)
                .arg(tags_arg),
        )
        .subcommand(
            Command::new("build")
                .about("Print filtered chronology rows, sorted by date (undated last)")
                .arg(
                    Arg::new("tag")
                        .long("tag")
                        .help("filter by tag; repeatable, multiple tags are OR'd")
                        .value_name("T")
                        .action(ArgAction::Append),
                )
                .arg(
                    Arg::new("from")
                        .long("from")
                        .help("earliest date, British DD/MM/YYYY (undated entries excluded)")
                        .value_name("DD/MM/YYYY"),
                )
                .arg(
                    Arg::new("to")
                        .long("to")
                        .help("latest date, British DD/MM/YYYY (undated entries excluded)")
                        .value_name("DD/MM/YYYY"),
                )
                .arg(
                    Arg::new("prefix")
                        .long("prefix")
                        .help("filter by SIR prefix, e.g. AC")
                        .value_name("AC"),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("table | csv | json")
                        .value_name("FMT")
                        .default_value("table"),
                )
                .arg(
                    Arg::new("out")
                        .long("out")
                        .help("write to FILE instead of stdout")
                        .value_name("FILE"),
                ),
        )
        .subcommand(
            Command::new("views").about("List the preset views with current entry counts"),
        )
        .subcommand(
            Command::new("check")
                .about("Deterministic integrity scan over the Master Chronology"),
        )
        .subcommand(
            Command::new("export")
                .about("Full register export for counsel/ET bundle use")
                .arg(
                    Arg::new("format")
                        .long("format")
                        .help("export format: csv")
                        .value_name("FMT"),
                )
                .arg(
                    Arg::new("out")
                        .long("out")
                        .help("output file")
                        .value_name("FILE"),
                ),
        )
}

pub fn try_dispatch(sub: Option<(&str, &ArgMatches)>, db: &Db) -> Result<bool> {
    let Some(("chronology", m)) = sub else {
        return Ok(false);
    };
    match m.subcommand() {
        Some(("tag", sm)) => tags::cmd_tags(db, sm, false),
        Some(("untag", sm)) => tags::cmd_tags(db, sm, true),
        Some(("build", sm)) => build::cmd_build(db, sm),
        Some(("views", _)) => views::cmd_views(db),
        Some(("check", _)) => check::cmd_check(db),
        Some(("export", sm)) => build::cmd_export(db, sm),
        // Bare `chronology`: print top-level help rather than failing.
        _ => {
            command().print_help()?;
            println!();
            Ok(true)
        }
    }
}

/// Print the clap help of one of our subcommands (used when required args
/// are missing, instead of panicking).
fn print_sub_help(name: &str) {
    let mut cmd = command();
    if let Some(sub) = cmd.find_subcommand_mut(name) {
        let _ = sub.print_help();
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_foreign_subcommands() {
        let db = Db::open_memory().unwrap();
        assert!(!try_dispatch(None, &db).unwrap());
        let m = Command::new("x")
            .subcommand(Command::new("other"))
            .get_matches_from(["x", "other"]);
        assert!(!try_dispatch(m.subcommand(), &db).unwrap());
    }

    #[test]
    fn bare_chronology_prints_help() {
        let db = Db::open_memory().unwrap();
        let m = Command::new("x")
            .subcommand(command())
            .get_matches_from(["x", "chronology"]);
        assert!(try_dispatch(m.subcommand(), &db).unwrap());
    }
}
