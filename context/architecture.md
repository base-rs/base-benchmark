# Architecture

## Package boundary

`base-benchmark` owns Bake-independent records, measurement, preparation, the experiment runner, validation, provenance, persistence, and statistics. The `base-benchmark` command is a thin argument parser over the `Workspace` operations. [`bake-benchmark`](https://github.com/base-rs/bake-benchmark) registers `benchmark:*` tasks that call the same `Workspace` operations; it contains no collector, validator, or summarizer. The dependency direction is adapter → core. Workloads depend on the core with `default-features = false`, which provides only records and interval measurement; the default `runner` feature adds preparation, collection, summaries, and the command. Public runtime libraries depend on neither package. No normal path uses Python.

The user selected this two-repository split on 6 November 2026, superseding the audit's single feature-gated package recommendation.

## Implemented modules

| Module | Responsibility |
| --- | --- |
| `record` | `ProcessRecord`: one versioned envelope per workload process, with workload identity, complete parameters, an optional output signature, ordered samples, and project extensions. Each `Metric` declares unit, phase (`measured`, `attached`, `process`), source, improvement direction, resolution, and a total, mean, quantile, or unavailable measurement. Only measured-phase totals are normalized by operations. |
| `usage` | `Interval` measures one batch: wall time plus `getrusage(RUSAGE_SELF)` total, user, and system CPU time and context switches, with checked deltas. Total CPU time has microsecond resolution; the tick-sampled user and system split is declared at 10 ms. `ResourceSource` is injectable for tests and alternative scopes. |
| `config` | Discovers `[package.metadata.base-benchmark]` in locked Cargo metadata, including unpublished packages, and resolves package-level defaults into scenarios and variants, including each scenario's baseline variant. |
| `prepare` and `artifact` | Builds labelled trees with one profile into separate target directories, locked unless the current workload package is overlaid onto copies of them, once for each feature set the prepared scenarios need; a tree's feature sets share its target directory, and each artifact records its features. The executable path comes from Cargo's JSON artifact messages. Executables are copied into `artifacts/LABEL/` and hashed, and each target directory is removed once its executable is frozen. Each artifact records source revision, dirty state, lockfile hash, the Git state of out-of-tree path dependencies, the build command, `rustc -Vv`, `cargo -Vv`, build-affecting environment variables, and the hashes of applicable Cargo configuration files, including `$CARGO_HOME` or `~/.cargo`. Path dependencies are recorded as unknown when Cargo metadata cannot be read, which recorded mode rejects. Compiler variables for C code, such as `CC` and `CFLAGS`, are not recorded. Layout replicas copy the baseline to exclusively created sibling directories, so relative path dependencies still resolve, rebuild them with every crate's `-C metadata` salted through a `RUSTC_WRAPPER` script, and remove the copies; without the salt a replica of a workspace build was byte-identical to its baseline, and summaries warn about any replica that still is. Cancellation is checked between builds. |
| `plan` | Expands a scenario into subjects (artifact × variant) and pairs (variant, build, replica), records the launch environment and working directory, and enforces recorded-mode prerequisites. Round order is a seeded Fisher–Yates shuffle; stack padding lengths come from a separate stream of the same seed. |
| `runner` | Stages every executable under equal-length names in `bin/`, takes the host-wide lock with visible, cancellation-aware waiting, then runs each subject as its own process group with a cleared environment, the workspace root as working directory, and optional stack padding. CPU placement is inherited at spawn, spawns retry `ETXTBSY`, and stdout and stderr are bounded. A deadline or cancellation kills the whole group, and group members left after the leader exits are killed before it is reaped. A descendant that leaves the group cannot be killed; if it keeps the output open beyond a 2 s grace period, the sample fails rather than hangs. The runner verifies executable hashes before every sample, reading each file once per process while its device, inode, size, and modification time are unchanged, and a staged copy inherits its source's verified hash, strictly parses one record per process, requires one workload name and signature across the campaign and equal parameters for each variant across artifacts, and optionally wraps `perf stat` for whole-process counters. It journals every sample, including failures, and records `in_progress`, `failed`, `cancelled`, or `complete` metadata, written atomically. |
| `remote` | Runs `prepare`, `run`, and `smoke` on another host over SSH: mirrors the workspace, trees, and their out-of-tree path dependencies there with `rsync`, maps every path argument to its mirror, installs this version of the command when it is missing, and copies the output directory back, also after a failure. |
| `summary` | Per-subject distributions and per-round paired differences, ratios, better and worse counts, and exact sign tests, with explicit aggregation and interpretation warnings. Summaries record the generating tool version and are regenerated from raw samples of complete campaigns only. |

Collection never builds code; preparation finishes first. Campaign directories and preparation outputs must be new directories. A failed or cancelled campaign keeps its samples and metadata but never produces a summary.

## Interpretation safeguards

These were added after the `base-html` integration, its screening runs, and two independent reviews; see the [integration report](https://github.com/base-rs/base-benchmark/blob/main/reports/base-html-integration.md).

- Every metric has a measurement quantum: its declared resolution, or one unit, divided by operations for per-operation values. Differences below the quantum count as ties in better and worse counts and in the sign test.
- A percentage change is reported only when every round's baseline is at least 100 quanta (`RESOLUTION_FACTOR`). Reporting ratios from only the resolvable rounds would select rounds with large baselines and bias the result. Count metrics are never shown as ratios.
- Pairs compare a metric only when unit, phase, source, and statistic match. Metrics defined differently, or missing from the candidate, are reported in warnings.
- Smoke summaries contain no pairs.
- Warnings name the following hazards:
  - missing CPU placement and disabled stack padding;
  - timed metrics that improved in some rounds and regressed in others;
  - build changes within the layout replicas' range;
  - build or variant comparisons without a replica;
  - variant comparisons whose sign reverses in the layout replica, or whose replica comparison is unresolved;
  - metrics redefined within or between processes, which are excluded rather than merged.

Warnings describe the evidence; none is an acceptance verdict, and the absence of warnings proves nothing.

## Layout controls

Emery Berger's "Performance Matters" (Strange Loop 2019) and Mytkowicz et al., "Producing Wrong Data Without Doing Anything Obviously Wrong!" (ASPLOS 2009), show that incidental layout, such as the size of the environment, link order, or a binary's path, can move results as much as the optimization being measured. The runner adopts the parts that need no special toolchain:

- a cleared, recorded environment and a fixed working directory, so the campaign does not depend on the shell or agent that started it;
- equal-length staged executable paths;
- random stack padding per sample, from the campaign seed;
- an exact sign test, so consistency across rounds has a stated probability instead of a bare count;
- several layout replicas, each one sample of build layout, with salted crate metadata so that each is actually built differently.

Deferred:

- Stabilizer-style runtime re-randomization of code, stack, and heap layout during execution. It needs compiler and runtime support that Base projects do not have; replicas and stack padding sample layout between processes instead.
- Coz causal profiling. It answers where to optimize rather than whether a change helped, so it belongs in investigations rather than in the runner.

## Not yet implemented

- Multi-process roles: server/client placement, readiness, endpoint transfer, and final statistics validation. Validate these against scheduler echo and HTTP client/hello before generalizing.
- Per-role resource sources such as `wait4` lifetime usage and `/proc` interval snapshots.
- Downstream workloads, in another repository, built against each tree through `[patch]` overrides, and revision archives as tree inputs. Callers create trees themselves, for example with `git worktree add`. A workload in the same workspace can be overlaid onto older trees with `--overlay`.
- Unchanged external comparators as plan subjects alongside variants, a same-binary repeat subject, and predeclared expected-error outcome contracts beyond reporting them as ordinary metrics.
- Phase-matched hardware counters. `perf stat` totals are whole-process only and are never divided by measured operations. Multiplexing percentages are not checked, and on hybrid CPUs, which report one line per PMU, only the first line is used.
- Several samples per process exist in the schema, but only single-sample workloads have been exercised. Markdown's batches are the intended test.
- The longitudinal history and analysis follow-up in [history](history.md), and investigation record generation. The [investigation convention](investigations.md) is documentation only.

## Known limits

- Scenario arguments for every tree come from the current workspace's metadata, not from each tree's own.
- Replica copies keep symbolic links as they are, so absolute links still point into the original tree, and they omit `.git`, so build scripts that call Git see no repository.
- A process that embeds the runner must not ignore `SIGCHLD`. The kernel would then reap exited workloads automatically, so the runner could neither clean up their process groups safely nor collect their exit status. A `Cancellation` tracks one workload process at a time.
- The signal handler stays installed for the life of the process; once no campaign is guarded, a termination signal exits with status 128 plus the signal number. On Linux, signals ignored when it is installed, as under `nohup`, stay ignored; other systems have no `/proc/self/status` to read, so there they are handled. A second signal exits immediately, so it can leave a replica source copy beside the baseline tree and a campaign recorded as `in_progress`; so can `SIGKILL`.
- A descendant that leaves the process group and closes its output is not detected, and may keep running during later samples.
- Once replicas exist, variant comparisons inside candidate artifacts have no replica control and no warning; only the baseline's variant comparisons are checked against the replica.
- If writing metadata fails after `summary.json` is written, the summary exists while the campaign is still recorded as `in_progress`.
- The runner builds on every platform, so a project's task package, such as one that depends on `bake-benchmark`, builds in CI on macOS and Windows. Collecting samples needs Linux: process groups, `waitid` without reaping, and CPU affinity. The host lock and replica copies of symbolic links need Unix. Elsewhere these operations return `Unsupported` errors; listing scenarios, preparing artifacts, and summarizing work. Tests and coverage run only on Linux; CI also checks that the library and command build on macOS and Windows.

Do not invent numeric performance thresholds, portable PMU support, peak-memory guarantees, pooled latency percentiles, automatic revision checkout, or one universal workload trait. Hosted CI validates correctness and failure handling, not performance.
