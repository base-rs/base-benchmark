# Longitudinal Benchmark Data and Analysis

## Priority and status

The user requested this on 6 November 2026 as a **nice-to-have follow-up, not a first implementation priority**: accumulate benchmark data over time, preferably on the same host or meaningfully normalized, compare revisions on graphs, and eventually offer a small comparison web server or analysis tool. No history store, query tool, graph UI, or server exists in the scaffold. Do not delay trustworthy measurement/validation or the first consumers to build a dashboard.

## Prepare the record contract now

When records are implemented, preserve enough information to select comparable series without reinterpreting historical numbers:

- Stable project/workload/scenario identities and complete parameters; operation/corpus/protocol identity and explicit semantic version or digest when those change.
- Source revision and optional patch/source digest, artifact/build/harness/schema identities, actual toolchain/features/flags/dependencies, and normal versus diagnostic status.
- Run identity, timestamp, mode, process/batch/round/pair/role identity, raw samples, failed/incomplete states, and availability. A timestamp is not a substitute for source identity.
- Metric units, scope/window, denominator, direction of improvement, aggregation, cohort/latency semantics, and correctness/completion contract.
- Stable measurement-host identity plus a hardware/OS/kernel/topology/configuration fingerprint or environment epoch, role-specific CPU placement, and relevant recorded power/frequency controls. The same hostname alone does not establish an unchanged environment.
- Baseline/control/replica identities, randomized order, pairing, and provenance of any normalization. Keep raw values alongside derived ratios and link every displayed aggregate back to its evidence.

Schema evolution and import adapters must preserve original records; do not rewrite history to make an old harness look compatible with a new workload definition. Storage/index details remain open: immutable result bundles plus a rebuildable index may suffice initially; no database dependency is implied.

## Comparable series, not a universal normalization factor

Prefer series collected on the same dedicated host under a stable environment and workload/harness contract. Split or clearly flag series when hardware, kernel, toolchain, placement, source semantics, or instrumentation changes. Same-host results still need comparable builds, controls, and noise interpretation.

Paired candidate/baseline ratios from the same rounds can provide a useful relative view when workload, scopes, and baseline identity match. A changing baseline is a different reference, not automatically the same trend line. Show numerator, denominator, control/layout-replica behavior, and missing prerequisites. Per-operation normalization is not cross-machine normalization, and one calibration constant cannot make arbitrary hosts or unlike protocols comparable. Any future cross-host comparison needs an explicit validated method and caveats; unsupported comparisons should be rejected or visibly separated, not silently graphed as one series.

## Later analysis and presentation

Index [investigation records](investigations.md) as well as run bundles so rejected/deferred ideas and their revisit conditions remain discoverable. Keep decision status separate from metric validity; do not omit a run merely because its candidate was rejected, and do not plot an invalid measurement as a valid regression.

A small Rust analysis command could ingest validated result bundles, filter compatible series, compare selected revisions, and export tables/JSON/graph data. A later read-only web server/UI could use those same comparison APIs and evidence, not invent another validator/statistics pipeline. Keep web framework/database dependencies out of minimal measurement consumers; exact packaging is undecided. Bake may invoke shared import/report operations without becoming the data store or running collection on a hosted web server.

Views should distinguish absolute values from ratios, process spread from request percentiles, screening from recorded evidence, and missing/failed samples from zero. Gaps must remain gaps. Graphs and “improved in N rounds” counts do not establish acceptance or causality.

## Follow-up validation

Test historical schema imports, stable IDs, duplicate run ingestion, incomplete bundles, changed host/environment epochs, workload/corpus/harness changes, metric-scope incompatibility, baseline drift, and reproducible aggregate regeneration. Check every derived point against raw records and its declared pair/normalization. Choose retention, indexing, access control, and UI packaging when this follow-up is prioritized.
