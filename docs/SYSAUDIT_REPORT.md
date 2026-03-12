# Sysaudit Report

Audit date: 2026-03-12

Scope:
- runtime topology: monolith vs hybrid gateway/core
- CRUD and recall path
- proxy injection, extraction, contradiction detection, and lifecycle learning
- background workers and hot-reload behavior
- CLI lifecycle tooling
- MCP HTTP bridge
- release vs release-smoke validation flow

## Component Ledger

| Component | Status | Evidence | Notes |
| --- | --- | --- | --- |
| hybrid gateway and core routing | healthy | integration tests + [STATE_MACHINES.md](/root/engraim/docs/STATE_MACHINES.md) | fail-open limited to `/proxy/*` |
| CRUD and recall APIs | healthy | `cargo test` integration suite | includes `query-explain` and task-context ranking |
| proxy injection/extraction path | healthy | integration tests + prior live extraction artifact | extraction still depends on post-filter for perfect specificity |
| lifecycle and contradiction handling | healthy | unit + integration tests | candidate/active/stale/contested/archived transitions covered |
| config hot-reload | fixed | code audit + new unit coverage | removed namespace allowlists no longer linger |
| CLI `memoryoss decay` | fixed | code audit + new unit coverage | stored-only namespaces are no longer skipped |
| MCP HTTP bridge | healthy | integration tests | still depends on healthy `/v1/*` core path |
| release workflow | healthy | prior live smoke run + workflow audit | real release and smoke are now separated |
| release-smoke workflow | healthy | live run `23011040534` + shared artifact builder | smoke and release now share one reusable build workflow |

## Findings

### Fixed

1. Hot-reload stale allowlist bug
   - Impact: removing a namespace from `trust.ip_allowlists` in config and sending `SIGHUP` left the old in-memory allowlist active.
   - Root cause: reload only overwrote present namespaces and never cleared removed ones.
   - Fix: startup and reload now rebuild the allowlist map and replace it atomically via `replace_ip_allowlists()` in [src/security/trust.rs](/root/engraim/src/security/trust.rs), [src/server/mod.rs](/root/engraim/src/server/mod.rs), and [src/server/routes.rs](/root/engraim/src/server/routes.rs).

2. CLI decay namespace coverage gap
   - Impact: `memoryoss decay` without `--namespace` could silently skip namespaces that still exist in storage but no longer appear in API-key config.
   - Root cause: namespace enumeration used config-derived namespaces plus `default`, not the database namespace set.
   - Fix: decay now scans the union of configured namespaces, stored namespaces, and `default` in [src/main.rs](/root/engraim/src/main.rs).

### Residual Risks

1. Extraction precision depends on filtering
   - Severity: low
   - Current live extraction metrics are strong after filtering, but raw extraction can still emit generic product phrasing before the filter removes it.

## Validation Status

| Check | Status | Notes |
| --- | --- | --- |
| targeted regression tests for audit fixes | pass | `replace_ip_allowlists_clears_removed_namespaces`, `decay_namespace_set_includes_stored_namespaces_not_in_config` |
| `cargo clippy -- -D warnings` | pass | rerun after fixes |
| `cargo test` | pass | `55` unit tests + `41` integration tests |
| `bash tests/run_all.sh` | pass | fresh post-fix run completed; `178` checks passed, `13` wizard scenarios, `20,000` benchmark memories, `20,000` calibration queries, `extraction_eval=pass` |
| `python3 tests/run_e2e_proxy_test.py` | pass | `33/33` checks passed against a fresh local HTTP server |
| live release-smoke workflow | pass | GitHub Actions run `23011040534` |
| latest live extraction eval artifact | pass | fresh rerun on `2026-03-12`: `case_recall_after_filter=1.0`, `case_specificity_after_filter=1.0`, `project_specific_fact_rate=0.9167`, `latency_ms_mean=2368.92` |

## Audit Verdict

The audit found two real silent-failure risks and fixed both. The system-level path model is now more explicit in [STATE_MACHINES.md](/root/engraim/docs/STATE_MACHINES.md). The current local build is broadly revalidated. The main remaining quality caveat is extraction raw precision before filtering, not a release-process drift issue.
