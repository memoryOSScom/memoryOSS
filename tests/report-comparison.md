# Test Comparison

Comparison date: 2026-03-08

Sources:
- Earlier public test claims: `/var/www/memoryoss.com/tests.html`
- Current automated runner output: [`tests/report.json`](/root/engraim/tests/report.json)

## Important caveat

This is not a pure apples-to-apples comparison.

- The earlier page was a manually maintained result page with broad claim coverage.
- The baseline below refers to the earlier `tests/run_all.sh` snapshot before the new matrix/benchmark/calibration runners were rerun into the generated report.
- Some areas moved from static claims to real automated verification.
- The repository now includes dedicated runners for wizard matrix, benchmarks, and calibration, but the comparison table below still reflects the last pre-refresh report snapshot.

## Summary

| Area | Earlier page | Current runner |
|---|---:|---:|
| Reported passing checks | 63 | 43 |
| Sections | 14 | 8 |
| Rust unit tests | 11 | 17 |
| Rust integration tests | not reported as real count, page said 63 tests overall | 14 |
| TypeScript tests | not surfaced | 5 |
| Wizard checks | 107 assertions across 9 scenarios | 3 smoke assertions in the older generated snapshot |
| Duration | 126s | 48s |
| 20k benchmark | yes | not yet reflected in the older generated snapshot |
| 9,585-query calibration block | yes | not yet reflected in the older generated snapshot |

## What the current runner covers better

- Real build gates: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build`
- Real `cargo audit` execution
- TypeScript SDK build/test
- Lifecycle feedback and lifecycle admin view
- Sharing owner/grantee/grant/revoke/access paths
- GDPR export/access/certified forget
- Key rotation/list/revoke/readability
- Decay and migrate CLI paths

## What the earlier page claimed but the older generated snapshot did not yet reproduce

- Fine-grained auth token-exchange matrix
- Fine-grained RBAC matrix
- Explicit namespace-isolation matrix
- Proxy memory-header matrix
- Proxy passthrough auth matrix
- 20k-memory scaling benchmark
- large-query calibration/eval block

## What is likely a reporting change rather than a regression

- `63` earlier vs `43` today does **not** mean the system lost 20 tests.
- The current report counts real automated checks from the runner, with different grouping.
- Several older sections were high-level PASS matrices on the page, not direct runner-derived counts.
- The repo now contains runnable replacements for the wizard matrix, benchmark, and calibration blocks; the remaining gap is refreshing the generated report from those runs.

## Net effect

The current test system is:

- more honest
- more reproducible
- better aligned with the real runner
- better on system-path coverage

The earlier page was:

- broader in claimed surface area
- stronger on benchmark/calibration storytelling
- much stronger on wizard/auth matrix presentation
- weaker as a direct representation of the exact current runner

## Recommended next step

To get the best of both:

1. Keep the current generated report as the source of truth.
2. Re-add historical-style sections only when backed by runnable automation.
3. Re-run and publish the dedicated commands:
   - `tests/run_benchmarks.sh`
   - `tests/run_calibration.sh`
   - `tests/run_wizard_matrix.sh`

That would let the test page show both:

- current automated runner truth
- extended benchmark/calibration truth

without drifting back into hand-maintained claims.
