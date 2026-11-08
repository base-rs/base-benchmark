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

Scenario arguments always come from the current workspace, so every tree must contain a workload that accepts them. To compare revisions from before the workload existed, or with an older version of it, prepare with `--overlay true`. Each tree is then copied to a sibling directory, so relative path dependencies still resolve, its workload package directory is replaced with this workspace's, and the copy is built. The trees themselves are never modified.

- **Lockfiles.** The copy is built without `--locked`, since the workload may need dependencies the tree's lockfile lacks. Its lockfile is first seeded with the current workspace's entries for packages the tree does not lock, so that the workload's new dependencies resolve to the current versions in every tree. Where the workload needs a newer version of a package the tree locks, Cargo upgrades it for the whole tree, including the library being measured. Each artifact lists such changes, and any added package that resolved differently from the current workspace, in `lockfile_changes`, and recorded campaigns refuse them: the tree is then not built as it was locked.
- **The tree's workspace.** It must list the package as a member, by name or with a glob, and define whatever the package inherits with `workspace = true`. Inherited dependencies take the tree's versions and features, and the tree's `[patch]` applies.
- **Provenance.** Each artifact records the overlaid package's path and Git state, read once per preparation, and recorded campaigns require it to be committed and unmodified. Reports name the overlaid package and the artifacts built with it, and list each artifact's lockfile changes, and `summary.json` records the same in `overlays`, so an overlaid comparison describes itself.

Every task except `summarize` accepts `--manifest-path` for a standalone workspace, such as a benchmark suite excluded from the main workspace. Relative paths are resolved against the project root. The first SIGINT, SIGTERM, or SIGHUP stops preparation between builds, writing no manifest, or kills the running workload's process group and records the campaign as cancelled; a second signal exits immediately.

Recorded runs require:

- at least 10 rounds;
- explicit CPU placement, a `--decision-rule`, and `--idle-host true`;
- committed, unmodified trees and out-of-tree path dependencies;
- the same toolchain, Cargo version, build environment variables, and Cargo configuration for every artifact;
- a layout replica when comparing separately built trees.

All unmet prerequisites are reported together.

Preparation finds out-of-tree path dependencies with `cargo metadata`, once per tree, run in the tree so that its Cargo configuration applies: offline, then, if that fails, with network access, which it says first, since a fresh Cargo cache lacks packages that only other workspace members need. If it still cannot read them, it warns, with Cargo's errors from both attempts, and records them as unknown, which recorded runs refuse. A cancelled preparation does not try again with network access.

Progress and the human report go to stderr; Bake's result is a compact JSON object with the campaign directory, `summary.json` path, evidence class, and warnings.

### Measuring on another host

A dedicated, otherwise idle machine gives steadier results than a development one. `prepare`, `run`, and `smoke` accept `--host HOST`, an SSH destination, to run there:

```sh
base-benchmark prepare --host bench.local --tree main=../main --tree candidate=.
base-benchmark run periodic --host bench.local --artifacts target/base-benchmark/prepared-TIME/artifacts.json --cpus 2-5
base-benchmark summarize target/base-benchmark/periodic-screening-TIME
```

- **Mirrored paths.** Every local path a command names is mirrored on the host at `DIRECTORY/mirror/ABSOLUTE-PATH`, where `--remote-directory DIRECTORY` defaults to `.cache/base-benchmark` in the remote user's home. Paths are made canonical locally: inputs must exist, and an output's existing ancestors are resolved, so a path named through a symlink, such as `/tmp` on macOS, maps to the same mirror each time, and no mirror leaves the directory. Before the command, the workspace, each `--tree`, and the trees' path dependencies outside them are copied there with `rsync`, without their top-level `target` directories but with their Git state, deleting files that no longer exist locally. A tree that is a linked worktree, as `git worktree add` makes, has its repository mirrored too, and its mirror names the mirrored repository. Relative path dependencies resolve within the mirrors, but an absolute `path =` in a manifest names that path on the host, which is not mirrored. Mirrors are keyed by local paths alone, so machines or users that share a host account need their own `--remote-directory`.
- **Outputs come back.** Commands get an explicit `--output`, the same default directory a local run would use. Afterwards that directory is copied back, also when the command failed or was cancelled, so such campaigns can be inspected locally. A command that failed before creating it has nothing to copy back, so its own error is all that is reported. A mirrored output whose local copy is gone is cleared first, so the command sees a new directory. Downloaded artifact manifests and campaigns name the host's paths: run prepared artifacts on the same host, and summarize campaigns anywhere.
- **Interruption.** SIGINT, SIGTERM, or SIGHUP locally, or a dropped connection, sends the host's command SIGTERM, and the cancelled campaign is still copied back when the connection allows. A remote `prepare` stops after the build in progress. SSH can still prompt on the terminal, for example for a password. If the connection is lost before the output comes back, it remains only in the host's mirror; running again with the same `--output` clears it, so copy it first if it matters.
- **The command on the host.** By default the host runs the crates.io release of this version, installed with `cargo install --locked` under `DIRECTORY/tool` when it is missing; an unreleased build needs `--remote-program PATH`, a path on the host, such as a build of the same revision there. Remote scripts run with `sh` from standard input, whatever the remote user's login shell.
- **Other arguments** pass unchanged and are checked by the host's command. `--lock` names a file on the host; a relative one is relative to the remote user's home.
- **Requirements.** `ssh`, and `rsync` 3.2.3 or later, on both machines, and a POSIX shell and Cargo on the host. `BASE_BENCHMARK_SSH` and `BASE_BENCHMARK_RSYNC` name other local programs. The host lock, CPU placement, and the idle-host declaration all apply to the host. Preparation does not take the host lock, so prepare before measuring rather than during another campaign.

## How samples are launched

Code and stack layout can change a program's speed as much as a real optimization. Each sample therefore starts under controlled conditions:

- The host lock is `/tmp/base-benchmark-host.lock`, independent of `$TMPDIR`, and every user can take it. Under a private `/tmp`, pass a shared `--lock` explicitly.
- Every executable is staged under the campaign's `bin/` directory with a name of the same length, so the path copied onto the initial stack does not depend on the artifact's label.
- The environment contains only `PATH` and `LC_ALL=C`, plus, outside recorded campaigns, `LLVM_PROFILE_FILE` when the collector has it, so that coverage runs, such as a workload test that smokes its scenarios under `cargo llvm-cov`, collect the workload's profiles. The working directory is the workspace root. The campaign metadata records both.
- Stack padding adds `BASE_BENCHMARK_STACK_PADDING` with a random length from 0 to 4095 bytes, drawn from the campaign seed, so stack alignment varies between processes rather than being fixed for a build. The length is journalled with each sample.
- Layout replicas rebuild the baseline's source at other paths. A different path alone usually gives an identical executable, since Cargo identifies workspace members by their relative paths, so every crate of a replica is also built with a distinct `-C metadata` salt, which changes symbol hashes and therefore code layout throughout. A `RUSTC_WRAPPER` script adds it, running in turn the wrapper the build would otherwise use, such as sccache, found as Cargo finds it: `RUSTC_WRAPPER`, then `CARGO_BUILD_RUSTC_WRAPPER`, then `build.rustc-wrapper` in the Cargo configuration nearest the tree, then `$CARGO_HOME`'s, including files they `include`. Relative paths resolve as Cargo resolves them, and `--config` arguments do not apply, since preparation never passes any. A compilation cache keys on the compiler's arguments, which include the salt, so it never returns an unsalted build for a replica. The script lives in the replica's own target directory, inside the preparation's new output directory; Cargo does not hash a `RUSTC_WRAPPER`'s path, so a replica's executable depends only on its salt and the chained wrapper, and each artifact records its salt. Each replica is one more sample of build layout; several give a better estimate of how much layout alone moves a result. A summary warns about any replica that is still byte-identical to its baseline. Salting needs Unix; elsewhere replicas are unsalted.

## Interpret the results

Read warnings first. Each names every metric with the same hazard in one comparison or subject, and they are ordered by hazard: build layout first, such as a variant comparison whose sign reverses in the layout replica, then metric definitions, campaign controls, and last, metrics that changed in both directions. Interpret a candidate change against:

- the layout replicas' changes in the same rounds;
- the spread across rounds;
- the consistency of its direction, and the sign test.

A change within the replicas' range, a variant result that reverses in the replica, or a metric that moves both ways is not evidence of improvement. Differences smaller than a metric's resolution count as ties. A percentage change is shown only when every round's baseline is at least 100 resolution quanta; otherwise the metric is reported as unresolved, with differences only. Counts are never shown as percentages. The sign test is the exact two-sided probability of a split at least as uneven if better and worse rounds were equally likely; it ignores magnitudes and is not corrected for the many metrics in one report. With five rounds it cannot fall below 0.0625.

Screening results are triage. Recorded results still need the predeclared rule and an investigation record, kept alongside the campaign directory. Before committing a campaign directory, check `metadata.json`: it records the collector's `PATH`, host name, and invocation.
