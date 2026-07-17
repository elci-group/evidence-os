# SAS EvidenceOS

Local-first evidence and litigation preparation toolkit for document-heavy
employment disputes.

> Enter the evidence once. Retrieve, analyse and deploy it everywhere.

Deterministic Rust core owns SIR numbers, dates, relationships, filenames,
deadlines and audit trails. No cloud, no silent mutation of the record.

## Layout

- `crates/evidence-core` — SQLite schema, SIR allocator, standard filenames, audit log
- `crates/evidence-cli` — `evidenceos` binary, single entry point
- `crates/evidence-*` — the ten modules (harvester, chronology, disclosure,
  sentinel, graph, auditor, meetings, deadlines, drafting, cockpit)
- `fixtures/` — sample intake batch for testing

## Module contract

Each module crate exposes:

```rust
pub fn command() -> clap::Command;
pub fn try_dispatch(
    sub: Option<(&str, &clap::ArgMatches)>,
    db: &evidence_core::Db,
) -> anyhow::Result<bool>;
```

The CLI wires one `.subcommand(...)` + one `try_dispatch` line per module.

## Conventions

- Dates stored ISO (`yyyy-mm-dd`), displayed British (`dd/mm/yyyy`).
- SIR numbers: `AC-SIR-0147` (per-prefix counters in the `meta` table).
- Standard filenames: `E.AC-SIR-0092 PB-10-03.07.2026 Covering email.pdf`.
- Every mutating action is written to `audit_log`.
- Modules never edit `evidence-core` or each other's crates.

## Build

```bash
cargo build --workspace
cargo test --workspace
```
