//! evidence-meetings: Meeting-to-Action Engine — stub, implementation pending.
use clap::{ArgMatches, Command};

pub fn command() -> Command {
    Command::new("meetings").about("Meeting-to-Action Engine (not yet implemented)")
}

pub fn try_dispatch(
    _sub: Option<(&str, &ArgMatches)>,
    _db: &evidence_core::Db,
) -> anyhow::Result<bool> {
    Ok(false)
}
