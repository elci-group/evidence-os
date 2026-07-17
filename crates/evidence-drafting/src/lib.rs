//! evidence-drafting: Drafting Assembly Bench — stub, implementation pending.
use clap::{ArgMatches, Command};

pub fn command() -> Command {
    Command::new("draft").about("Drafting Assembly Bench (not yet implemented)")
}

pub fn try_dispatch(
    _sub: Option<(&str, &ArgMatches)>,
    _db: &evidence_core::Db,
) -> anyhow::Result<bool> {
    Ok(false)
}
