# Architecture

## Package boundary

`base-benchmark` owns Bake-independent records, measurement, preparation, the experiment runner, validation, provenance, persistence, and statistics. The `base-benchmark` command is a thin argument parser over the `Workspace` operations. [`bake-benchmark`](https://github.com/base-rs/bake-benchmark) registers `benchmark:*` tasks that call the same `Workspace` operations; it contains no collector, validator, or summarizer. The dependency direction is adapter → core. Workloads depend on the core only for records and interval measurement, and public runtime libraries depend on neither package. No normal path uses Python.

The user selected this two-repository split on 6 November 2026, superseding the audit's single feature-gated package recommendation.

## Implemented modules

| Module | Responsibility |
| --- | --- |
| `record` | `ProcessRecord`: one versioned envelope per workload process, with workload identity, complete parameters, an optional output signature, ordered samples, and project extensions. Each `Metric` declares unit, phase (`measured`, `attached`, `process`), source, improvement direction, resolution, and a total, mean, or unavailable measurement. Only measured-phase totals are normalized by operations. |
| `usage` | `Interval` measures one batch: wall time plus `getrusage(RUSAGE_SELF)` CPU time and context switches, with checked deltas and the source's resolution. `ResourceSource` is injectable for tests and alternative scopes. |
| `config` | Discovers `[package.metadata.base-benchmark]` in locked Cargo metadata, including unpublished packages, and resolves package-level defaults into scenarios and variants. |
| `prepare` and `artifact` | Builds labelled trees with one profile, feature set, and `--locked` policy into separate target directories. The executable path comes from Cargo's JSON artifact messages. Executables are copied into `artifacts/LABEL/`, hashed, and recorded with source revision, dirty state, lockfile hash, build command, toolchain, and rustflags. A layout replica copies the baseline to a sibling directory, so relative path dependencies still resolve, rebuilds it, and removes the copy. |
| `plan` | Expands a scenario into subjects (artifact × variant) and pairs (variant, build, replica), and enforces recorded-mode prerequisites. Round order is a seeded Fisher–Yates shuffle. |
| `runner` | Takes the host-wide lock with visible, cancellation-aware waiting, then runs each subject as its own process group. CPU placement is inherited at spawn, and stdout and stderr are bounded. A deadline and SIGINT, SIGTERM, or SIGHUP kill the whole group. Remaining descendants are cleaned up before the leader is reaped. The runner verifies executable hashes, strictly parses one record per process, checks output signatures, and optionally wraps `perf stat` for whole-process counters. It journals every sample, including failures, and records `in_progress`, `failed`, `cancelled`, or `complete` metadata. |
| `summary` | Per-subject distributions and per-round paired differences and ratios, with explicit aggregation and interpretation warnings. Summaries are regenerated from raw samples of complete campaigns only. |

Collection never builds code; preparation finishes first. Campaign directories and preparation outputs must be new directories. A failed or cancelled campaign keeps its samples and metadata but never produces a summary.

## Interpretation safeguards

These were added after the `base-html` integration and its screening runs; see the [integration report](https://github.com/base-rs/base-benchmark/blob/main/reports/base-html-integration.md).

- Ratios are computed only when the baseline is at least 100 measurement quanta (`RESOLUTION_FACTOR`); other rounds are counted as unresolved. Count metrics are reported as absolute per-operation differences, because ratios of a few events are noise.
- Smoke reports omit comparisons entirely.
- Warnings name the following hazards:
  - missing CPU placement;
  - timed metrics that improved in some rounds and regressed in others;
  - build changes within the layout replica's range;
  - build comparisons without a replica;
  - variant comparisons whose sign reverses in the layout replica;
  - metrics redefined within or between processes.

Warnings describe the evidence; none is an acceptance verdict, and the absence of warnings proves nothing.

## Not yet implemented

- Multi-process roles: server/client placement, readiness, endpoint transfer, and final statistics validation. Validate these against scheduler echo and HTTP client/hello before generalizing.
- Per-role resource sources such as `wait4` lifetime usage and `/proc` interval snapshots.
- Downstream workloads built against each tree through `[patch]` overrides, and revision archives as tree inputs. Callers create trees themselves, for example with `git worktree add`.
- Unchanged external comparators as plan subjects alongside variants, and predeclared expected-error outcome contracts beyond reporting them as ordinary metrics.
- Phase-matched hardware counters. `perf stat` totals are whole-process only and are never divided by measured operations.
- Several samples per process exist in the schema, but only single-sample workloads have been exercised. Markdown's batches are the intended test.
- Remote measuring hosts. The runner records the host it runs on; synchronization or SSH transport is left to the operator.
- The longitudinal history and analysis follow-up in [history](history.md), and investigation record generation. The [investigation convention](investigations.md) is documentation only.

Do not invent numeric performance thresholds, portable PMU support, peak-memory guarantees, pooled latency percentiles, automatic revision checkout, or one universal workload trait. Hosted CI validates correctness and failure handling, not performance.
