# PADAGONIA Integration Roadmap

Use `/home/sal/padagonia/docs/enterprise-integration-directives.md` as the
release contract.

## Modules

- `evidence_ingest`: normalize evidence records, source hashes, observations,
  and collection metadata.
- `claim_linker`: connect evidence to claims, controls, decisions, owners, and
  deployments using explicit `supports`, `contradicts`, and `derived_from` edges.
- `identity_hash`: hash content and bind authenticated producer/version data;
  do not claim Padagonia itself is tamper-proof.
- `control_query`: provide namespace-filtered evidence and conflict-aware
  compliance queries.
- `decision_export`: generate deterministic audit exports with correlation IDs.
- `retention`: implement expiry, redaction, and snapshot handling across all
  copies of evidence.

## Acceptance gates

Every audit conclusion has traceable evidence, conflicts remain visible, and a
restore drill reproduces the same decision inputs.
