//! evidence-sentinel: Document Naming and Filing Sentinel.
//!
//! Enforces the SAS standard evidence filename rule on real folders —
//! naming integrity is evidence integrity:
//!
//! ```text
//! ^[A-Z]\.[A-Z]{1,6}-SIR-\d{4,} .+\.[a-z0-9]+$
//! E.AC-SIR-0092 PB-10-03.07.2026 Covering email.pdf
//! ```
//!
//! Subcommands:
//! - `sentinel scan <DIR>` — report violations (no changes).
//! - `sentinel propose <DIR> [--prefix AC]` — dry-run compliant names.
//! - `sentinel apply <DIR> [--move-to <DIR>]` — rename/move, audited and
//!   registered; refuses to overwrite, aborts with zero changes on collision.

mod apply;
mod files;
mod propose;
mod register;
mod scan;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Arg, ArgMatches, Command};
use evidence_core::Db;

/// Module name recorded in `audit_log` for every mutation.
pub(crate) const ACTOR: &str = "sentinel";

pub fn command() -> Command {
    Command::new("sentinel")
        .about("Document Naming and Filing Sentinel — enforce SAS naming rules")
        .override_usage("evidenceos sentinel <COMMAND>")
        .subcommand(
            Command::new("scan")
                .about("Report naming/register violations in DIR (no changes)")
                .override_usage("evidenceos sentinel scan <DIR>")
                .arg(Arg::new("dir").help("Folder to scan").value_name("DIR")),
        )
        .subcommand(
            Command::new("propose")
                .about("Propose compliant names for violating files (dry run)")
                .override_usage("evidenceos sentinel propose <DIR> [--prefix <PREFIX>]")
                .arg(Arg::new("dir").help("Folder to scan").value_name("DIR"))
                .arg(
                    Arg::new("prefix")
                        .long("prefix")
                        .help("Preferred SIR prefix when a filename carries several SIRs")
                        .value_name("PREFIX")
                        .default_value("AC"),
                ),
        )
        .subcommand(
            Command::new("apply")
                .about("Rename violating files to compliant names (refuses to overwrite)")
                .override_usage("evidenceos sentinel apply <DIR> [--move-to <DIR2>]")
                .arg(Arg::new("dir").help("Folder to scan").value_name("DIR"))
                .arg(
                    Arg::new("move-to")
                        .long("move-to")
                        .help("Move renamed files into DIR2 instead of renaming in place")
                        .value_name("DIR2"),
                ),
        )
}

pub fn try_dispatch(sub: Option<(&str, &ArgMatches)>, db: &Db) -> Result<bool> {
    let m = match sub {
        Some(("sentinel", m)) => m,
        _ => return Ok(false),
    };
    match m.subcommand() {
        Some(("scan", sm)) => {
            let Some(dir) = sm.get_one::<String>("dir") else {
                print_sub_help("scan");
                return Ok(true);
            };
            scan::run(db, Path::new(dir))?;
        }
        Some(("propose", sm)) => {
            let Some(dir) = sm.get_one::<String>("dir") else {
                print_sub_help("propose");
                return Ok(true);
            };
            let prefix = sm
                .get_one::<String>("prefix")
                .map(String::as_str)
                .unwrap_or("AC");
            propose::run(db, Path::new(dir), prefix)?;
        }
        Some(("apply", sm)) => {
            let Some(dir) = sm.get_one::<String>("dir") else {
                print_sub_help("apply");
                return Ok(true);
            };
            let move_to = sm.get_one::<String>("move-to").map(PathBuf::from);
            apply::run(db, Path::new(dir), move_to.as_deref())?;
        }
        _ => {
            // Bare `sentinel` (or an unknown subcommand): show module help.
            let mut cmd = command();
            cmd.print_help()?;
            println!();
        }
    }
    Ok(true)
}

/// Print the usage of one subcommand (used when a required arg is missing —
/// we report and return Ok(true) rather than letting clap kill the process).
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
    use evidence_core::SirEntry;

    fn root_cmd() -> Command {
        Command::new("evidenceos").subcommand(command())
    }

    #[test]
    fn dispatch_ignores_other_modules_and_none() {
        let db = Db::open_memory().unwrap();
        assert!(!try_dispatch(None, &db).unwrap());
        let other = Command::new("harvest").get_matches_from(["harvest"]);
        assert!(!try_dispatch(Some(("harvest", &other)), &db).unwrap());
    }

    #[test]
    fn dispatch_missing_dir_prints_usage_without_panicking() {
        let db = Db::open_memory().unwrap();
        let m = root_cmd().get_matches_from(["evidenceos", "sentinel", "scan"]);
        assert!(try_dispatch(m.subcommand(), &db).unwrap());
        let m = root_cmd().get_matches_from(["evidenceos", "sentinel"]);
        assert!(try_dispatch(m.subcommand(), &db).unwrap());
    }

    #[test]
    fn dispatch_scan_and_propose_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().unwrap();
        SirEntry {
            sir_no: "AC-SIR-0042".into(),
            seq: 42,
            prefix: "AC".into(),
            doc_date: Some("2026-07-03".into()),
            doc_type: Some("Email".into()),
            description: Some("Covering email".into()),
            source_batch: Some("PB-10".into()),
            status: "draft".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
        std::fs::write(
            dir.path().join("AC-SIR-0042 covering email.PDF"),
            b"mail",
        )
        .unwrap();

        let d = dir.path().to_str().unwrap().to_string();
        let m = root_cmd().get_matches_from(["evidenceos", "sentinel", "scan", &d]);
        assert!(try_dispatch(m.subcommand(), &db).unwrap());

        let m = root_cmd().get_matches_from(["evidenceos", "sentinel", "propose", &d]);
        assert!(try_dispatch(m.subcommand(), &db).unwrap());

        // propose is a dry run: the file must still be there under its old name.
        assert!(dir.path().join("AC-SIR-0042 covering email.PDF").exists());
    }

    #[test]
    fn dispatch_apply_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().unwrap();
        SirEntry {
            sir_no: "AC-SIR-0042".into(),
            seq: 42,
            prefix: "AC".into(),
            doc_date: Some("2026-07-03".into()),
            doc_type: Some("Email".into()),
            description: Some("Covering email".into()),
            source_batch: Some("PB-10".into()),
            status: "draft".into(),
            ..Default::default()
        }
        .insert(&db.conn)
        .unwrap();
        std::fs::write(
            dir.path().join("AC-SIR-0042 covering email.PDF"),
            b"mail",
        )
        .unwrap();

        let d = dir.path().to_str().unwrap().to_string();
        let m = root_cmd().get_matches_from(["evidenceos", "sentinel", "apply", &d]);
        assert!(try_dispatch(m.subcommand(), &db).unwrap());

        assert!(
            dir.path()
                .join("E.AC-SIR-0042 PB-10-03.07.2026 Covering email.pdf")
                .exists()
        );
        let entry = SirEntry::get(&db.conn, "AC-SIR-0042").unwrap().unwrap();
        assert!(entry.file_path.is_some());
    }
}
