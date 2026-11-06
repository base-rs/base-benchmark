# Releases

## Unreleased

- Establish the repository and shared Base development tooling.
- Record the organization-wide benchmarking audit and the two-repository Rust architecture.
- Incorporate scheduler consumer feedback on comparative preparation, layout controls, screening, and declared failure outcomes into the plan.
- Capture the lower-priority historical data, normalized comparison, and graph/analysis follow-up.
- Define a shared performance investigation record and template that retain rejected, invalid, and inconclusive attempts as well as adopted changes.
- Add versioned process records with explicit metric phases, sources, directions, and resolution, and `Interval` for measured-phase wall, CPU, and context-switch totals.
- Discover scenarios and variants from `[package.metadata.base-benchmark]`, including unpublished packages and package-level defaults.
- Prepare labelled source trees into frozen, hashed executables with source and build identity, plus a same-source layout replica built beside the baseline.
- Run seeded random rounds under a host-wide lock with CPU placement, deadlines, bounded output, signal cancellation, and process-group cleanup; journal every sample and record completion state.
- Summarize per-round paired differences and resolution-aware ratios, and warn about missing placement, inconsistent directions, and changes within or reversed by the layout replica.
- Add the `base-benchmark` command with `list`, `prepare`, `smoke`, `run`, and `summarize`.
- Control launch layout: stage executables under equal-length paths, clear the workload environment to `PATH` and `LC_ALL=C`, start in the workspace root, and add seeded random stack padding to every sample.
- Build any number of layout replicas with `--replicas N`, and warn when variant comparisons have no replica or an unresolved one.
- Report exact two-sided sign tests, count differences below a metric's resolution as ties, show percentage changes only when every round resolves, never show counts as percentages, and compare metrics only under identical definitions.
- Add a microsecond-resolution `cpu_time` total, and declare the tick-sampled user and system split at 10 ms.
- Reject campaigns whose records differ in workload or signature, or whose artifacts run a variant with different parameters.
- Record `cargo -Vv`, build-affecting environment variables, Cargo configuration files, and the Git state of out-of-tree path dependencies; recorded mode requires all of them to match and be committed.
- Add `measure-arguments` for screening and recorded runs, show resolved baseline variants, and add `Workspace::smoke_executable`.
- Split the runner into a default `runner` feature so workloads link only records and `Interval`.
- Use the fixed, shared host lock `/tmp/base-benchmark-host.lock`, write metadata atomically, fail samples whose escaped descendants hold the output open, retry busy executables, and handle signals process-wide: the first cancels guarded work, including preparation between builds, and a second exits.
- Take the scenario positionally in `base-benchmark run`, and reject a repeated `--manifest-path`.
- Never follow a symbolic link at the shared lock path, open an existing lock without `O_CREAT` so other users can share it under `fs.protected_regular`, and widen permissions only on a lock file created exclusively.
- Compute sign tests in logarithms, so long campaigns do not underflow to zero.
- Record path dependencies as unknown when Cargo metadata cannot be read, and reject them in recorded mode; filter metadata to the host platform, and include `~/.cargo` configuration when `$CARGO_HOME` is unset.
- Leave signals that were ignored at startup alone on Linux, and treat unavailable values as compatible with any statistic of the same metric.
