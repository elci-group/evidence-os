//! evidence-sentinel: Document Naming and Filing Sentinel — stub, implementation pending.
use clap::{ArgMatches, Command};

pub fn command() -> Command {
    Command::new("sentinel").about("Document Naming and Filing Sentinel (not yet implemented)")
}

pub fn try_dispatch(
    _sub: Option<(&str, &ArgMatches)>,
    _db: &evidence_core::Db,
) -> anyhow::Result<bool> {
    Ok(false)
}
