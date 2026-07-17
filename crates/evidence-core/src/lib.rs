//! evidence-core: shared foundation for SAS EvidenceOS.
//!
//! One SQLite case database, one authoritative Master Chronology (`sir_entries`),
//! deterministic SIR numbering, standard evidence filenames and an audit log.
//! Module crates build on this; they must not duplicate its responsibilities.

pub mod audit;
pub mod db;
pub mod models;
pub mod sir;
pub mod util;

pub use db::Db;
pub use models::SirEntry;
