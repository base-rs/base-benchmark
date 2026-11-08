# Using the Shared Benchmark Tooling

Use this guide to add a workload to a Base project and collect it with `base-benchmark`. The `base-project-benchmarks` and `base-project-performance` skills remain the methodology authority: where packages live, how to control the host, what to measure, and how to report. This guide covers how the tooling implements that.

## Write a workload binary

Put the workload in an unpublished `benchmarks/<subject>/` workspace package, named `<project>-benchmark-<subject>`, that depends on the measured library and on `base-benchmark` with `default-features = false`. That provides records and `Interval` without linking the runner into the measured binary. Tests that drive the binary through the runner can enable the default `runner` feature as a dev-dependency. This relies on Cargo's feature resolver 2 or later, the default since edition 2021, which keeps dev-dependency features out of ordinary builds.

```toml
[dependencies]
base-benchmark = { version = "0.1", default-features = false }

[dev-dependencies]
base-benchmark = "0.1"
```

The binary parses its own options, prepares inputs and checks correctness outside the measured interval, then writes one record to stdout:

```rust,ignore
use base_benchmark::{Interval, Metric, ProcessRecord, Unit};

let interval = Interval::start()?;
for _ in 0..iterations {
    operation(std::hint::black_box(&input));
}
let sample = interval
    .finish(iterations)?
    .with(Metric::total("output_bytes", Unit::Bytes, bytes_per_operation * iterations));
ProcessRecord::new("project/subject", serde_json::to_value(&configuration)?)
    .with_signature(format!("{length}:{checksum:016x}"))
    .with_sample(sample)
    .emit(&mut std::io::stdout())?;
```

- `Interval` reports measured-phase wall time, total CPU time, user and system CPU time, and voluntary and involuntary context switches. Total CPU time has microsecond resolution. Linux divides it between user and system time by scheduler ticks, so the split is declared with 10 ms resolution. Report project metrics as totals over the measured operations; summaries divide them by `operations`.
- Mark metrics that cover more than the measured operations with `Phase::Process` or `Phase::Attached`; those are never normalized. `Metric::mean` values are reported as they are, and so are `Metric::quantile` values, such as a 99th percentile latency with quantile 0.99. Values at different quantiles are different definitions, so a subject that reports another quantile under the same name is flagged rather than compared. Like every metric, a subject's quantile is the median, over its processes, of each process's median sample, not a quantile of every observation pooled together.
- Declare a direction, such as `lower_is_better()`, so paired changes can be counted as better or worse. Neutral metrics, such as output size, are summarized but not compared. Declare `with_resolution` for quantized metrics; the default is one unit.
- Put every parameter that changes the work in `parameters`. The runner rejects a campaign in which two artifacts run the same variant with different parameters, for example after a candidate changed a default.
- Set a signature only when equal values mean equivalent work. Every record in a campaign must report the same workload name and the same signature, or all omit it. Omit it when variants legitimately produce different output.
- Exit unsuccessfully, without a record, on any correctness failure. The runner treats a nonzero exit, a timeout, an invalid record, extra output, or a descendant that leaves the process group and keeps its output open as a failed sample, and stops the campaign.
- For tests of error paths, start the interval with `Interval::start_with` and an injected `ResourceSource`. `Workspace::smoke_executable` runs every scenario once against a binary built by Cargo, such as `CARGO_BIN_EXE_<name>` in the workload's own tests.

## Declare scenarios

```toml
[package.metadata.base-benchmark]
smoke-arguments = ["--iterations", "3", "--warmup", "1"]
measure-arguments = ["--iterations", "10000"]
variants = [{ name = "reference", arguments = ["--variant", "reference"] }, { name = "engine", arguments = ["--variant", "engine"] }]

[[package.metadata.base-benchmark.scenario]]
name = "list"
arguments = ["--workload", "list", "--items", "32"]
```

Keys are `binary` (needed only with several binary targets), `profile` (default `release`), `features`, `arguments` (prepended to every scenario's), `smoke-arguments`, `measure-arguments`, `variants`, and `baseline` (default: the first variant). A scenario can replace `smoke-arguments`, `measure-arguments`, `variants`, `baseline`, and `features`. Unknown keys are errors.

A scenario that needs other features, such as a diagnostic build that counts allocations or reports internal statistics, sets its own `features`. Preparation builds each tree once for every distinct feature set of the scenarios it prepares, and a run uses only the builds with exactly its scenario's features. Timed scenarios keep the package's features, so a diagnostic feature never reaches their builds. `--scenario NAME`, repeatable, prepares only the feature sets those scenarios need; a run of a scenario whose features were not prepared fails and says so. Each subject runs `arguments`, then its variant's arguments, then the smoke arguments in smoke mode or the measure arguments otherwise, so the two never collide.

## Run it

Add the Bake tasks to the private `bake/` package with `cargo add --manifest-path bake/Cargo.toml bake-benchmark` and `cargo bake --regenerate`. The `base-benchmark` command offers the same operations, options, and defaults; only the spelling differs where Bake has its own conventions. Run `cargo bake benchmark:run --help` or `base-benchmark help` for every option.

| Task | Purpose |
| --- | --- |
| `benchmark:list` | Show discovered suites, scenarios, variants, and each scenario's resolved baseline. Nothing is built. |
| `benchmark:smoke` | Build the current tree and run every scenario once with smoke arguments. Not performance evidence; no comparisons are computed. |
| `benchmark:prepare --tree LABEL=PATH ...` | Build trees, baseline first, into frozen artifacts plus `--replicas N` layout replicas of the baseline (one by default for two or more trees, otherwise none), once for each feature set of the scenarios, or of those named with `--scenario`. `--overlay true` builds every tree with this workspace's workload package. Writes `artifacts.json`. |
| `benchmark:run SCENARIO --artifacts FILE` | Collect `--rounds` seeded random rounds under the host lock. `--mode` is `screening` (default) or `recorded`. `--cpus` pins workloads; `--perf-event` adds whole-process counters; `--stack-padding false` disables stack padding. |
| `benchmark:summarize DIR` | Regenerate `summary.json` from a complete campaign's raw samples. |

### Comparing revisions with the current workload

Scenario arguments always come from the current workspace, so every tree must contain a workload that accepts them. To compare revisions from before the workload existed, or with an older version of it, prepare with `--overlay true`. Each tree is then copied to a sibling directory, so relative path dependencies still resolve, its workload package directory is replaced with this workspace's, and the copy is built. The trees themselves are never modified. The copy is built without `--locked`, since the workload may need dependencies the tree's lockfile lacks; Cargo keeps the tree's locked versions where they satisfy it, and each artifact records the lockfile it was built with. The tree's workspace must list the package as a member, and the package must be in a directory below the workspace root. Each artifact records the overlaid package's path and Git state, and recorded campaigns require it to be committed and unmodified.

Every task except `summarize` accepts `--manifest-path` for a standalone workspace, such as a benchmark suite excluded from the main workspace. Relative paths are resolved against the project root. The first SIGINT, SIGTERM, or SIGHUP stops preparation between builds or kills the running workload's process group and records the campaign as cancelled; a second signal exits immediately.

Recorded runs require:

- at least 10 rounds;
- explicit CPU placement, a `--decision-rule`, and `--idle-host true`;
- committed, unmodified trees and out-of-tree path dependencies;
- the same toolchain, Cargo version, build environment variables, and Cargo configuration for every artifact;
- a layout replica when comparing separately built trees.

All unmet prerequisites are reported together.

Progress and the human report go to stderr; Bake's result is a compact JSON object with the campaign directory, `summary.json` path, evidence class, and warnings.

## How samples are launched

Code and stack layout can change a program's speed as much as a real optimization. Each sample therefore starts under controlled conditions:

- The host lock is `/tmp/base-benchmark-host.lock`, independent of `$TMPDIR`, and every user can take it. Under a private `/tmp`, pass a shared `--lock` explicitly.
- Every executable is staged under the campaign's `bin/` directory with a name of the same length, so the path copied onto the initial stack does not depend on the artifact's label.
- The environment contains only `PATH` and `LC_ALL=C`, plus, outside recorded campaigns, `LLVM_PROFILE_FILE` when the collector has it, so that coverage runs, such as a workload test that smokes its scenarios under `cargo llvm-cov`, collect the workload's profiles. The working directory is the workspace root. The campaign metadata records both.
- Stack padding adds `BASE_BENCHMARK_STACK_PADDING` with a random length from 0 to 4095 bytes, drawn from the campaign seed, so stack alignment varies between processes rather than being fixed for a build. The length is journalled with each sample.
- Layout replicas rebuild the baseline's source at other paths. A different path alone usually gives an identical executable, since Cargo identifies workspace members by their relative paths, so the workspace's own crates are also built with a distinct `-C metadata` salt per replica, added by a `RUSTC_WORKSPACE_WRAPPER` script that runs any wrapper already in the environment first. The salt changes symbol hashes and therefore code layout, and each artifact records it. Each replica is one more sample of build layout; several give a better estimate of how much layout alone moves a result. Salting needs Unix; elsewhere replicas are unsalted.

## Interpret the results

Read warnings first. Interpret a candidate change against:

- the layout replicas' changes in the same rounds;
- the spread across rounds;
- the consistency of its direction, and the sign test.

A change within the replicas' range, a variant result that reverses in the replica, or a metric that moves both ways is not evidence of improvement. Differences smaller than a metric's resolution count as ties. A percentage change is shown only when every round's baseline is at least 100 resolution quanta; otherwise the metric is reported as unresolved, with differences only. Counts are never shown as percentages. The sign test is the exact two-sided probability of a split at least as uneven if better and worse rounds were equally likely; it ignores magnitudes and is not corrected for the many metrics in one report. With five rounds it cannot fall below 0.0625.

Screening results are triage. Recorded results still need the predeclared rule and an investigation record, kept alongside the campaign directory. Before committing a campaign directory, check `metadata.json`: it records the collector's `PATH`, host name, and invocation.
