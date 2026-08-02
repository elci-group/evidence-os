//! In-memory snapshot of the Master Chronology for the duration of one run.

use anyhow::Result;
use evidence_core::{Db, SirEntry};

/// The register, loaded once per command so scans and proposals work against a
/// stable view of `sir_entries`.
pub(crate) struct Register {
    entries: Vec<SirEntry>,
}

impl Register {
    pub fn load(db: &Db) -> Result<Self> {
        Ok(Self {
            entries: SirEntry::list(&db.conn)?,
        })
    }

    /// Look an entry up by SIR number.
    pub fn get(&self, sir_no: &str) -> Option<&SirEntry> {
        self.entries.iter().find(|e| e.sir_no == sir_no)
    }

    /// First entry (register order: prefix, then sequence) whose stored
    /// SHA-256 matches. Used to identify files whose name carries no SIR.
    pub fn find_by_sha(&self, sha256: &str) -> Option<&SirEntry> {
        self.entries
            .iter()
            .find(|e| e.sha256.as_deref() == Some(sha256))
    }

    pub fn entries(&self) -> &[SirEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sir_no: &str, seq: i64, sha: Option<&str>) -> SirEntry {
        SirEntry {
            sir_no: sir_no.into(),
            seq,
            prefix: "AC".into(),
            sha256: sha.map(str::to_string),
            status: "draft".into(),
            ..Default::default()
        }
    }

    #[test]
    fn lookups_by_sir_and_sha() {
        let db = Db::open_memory().unwrap();
        entry("AC-SIR-0001", 1, Some("aa")).insert(&db.conn).unwrap();
        entry("AC-SIR-0002", 2, None).insert(&db.conn).unwrap();
        let register = Register::load(&db).unwrap();
        assert!(register.get("AC-SIR-0001").is_some());
        assert!(register.get("AC-SIR-9999").is_none());
        assert_eq!(
            register.find_by_sha("aa").map(|e| e.sir_no.as_str()),
            Some("AC-SIR-0001")
        );
        assert!(register.find_by_sha("bb").is_none());
        assert_eq!(register.entries().len(), 2);
    }
}
