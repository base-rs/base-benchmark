# Using the Shared Benchmark Tooling

Use this guide to add a workload to a Base project and collect it with `base-benchmark`. The `base-project-benchmarks` and `base-project-performance` skills remain the methodology authority: where packages live, how to control the host, what to measure, and how to report. This guide covers how the tooling implements that.

## Write a workload binary

Put the workload in an unpublished `benchmarks/<subject>/` workspace package, named `<project>-benchmark-<subject>`, that depends on the measured library and on `base-benchmark`. The binary parses its own options, prepares inputs and checks correctness outside the measured interval, then writes one record to stdout:

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

- `Interval` reports measured-phase wall time, user and system CPU time, and voluntary and involuntary context switches. Report project metrics as totals over the measured operations; summaries divide them by `operations`.
- Mark metrics that cover more than the measured operations with `Phase::Process` or `Phase::Attached`; those are never normalized.
- Declare a direction, such as `lower_is_better()`, so paired changes can be counted as better or worse. Neutral metrics, such as output size, are summarized but not compared.
- Set a signature only when equal values mean equivalent work. The runner rejects a campaign whose subjects report different signatures. Omit it when variants legitimately produce different output.
- Exit unsuccessfully, without a record, on any correctness failure. The runner treats a nonzero exit, a timeout, an invalid record, or extra output as a failed sample and stops the campaign.
- For tests of error paths, start the interval with `Interval::start_with` and an injected `ResourceSource`.

## Declare scenarios

```toml
[package.metadata.base-benchmark]
smoke-arguments = ["--iterations", "3", "--warmup", "1"]
variants = [{ name = "reference", arguments = ["--variant", "reference"] }, { name = "engine", arguments = ["--variant", "engine"] }]

[[package.metadata.base-benchmark.scenario]]
name = "list"
arguments = ["--workload", "list", "--items", "32"]
```

Keys are `binary` (needed only with several binary targets), `profile` (default `release`), `features`, `arguments` (prepended to every scenario's), `smoke-arguments`, `variants`, and `baseline` (default: the first variant). A scenario can replace `smoke-arguments`, `variants`, and `baseline`. Unknown keys are errors. Each subject runs `arguments`, then its variant's arguments, then the smoke arguments in smoke mode.

## Run it

Add the Bake tasks to the private `bake/` package with `cargo add --manifest-path bake/Cargo.toml bake-benchmark` and `cargo bake --regenerate`. The `base-benchmark` command offers the same operations with the same options.

| Task | Purpose |
| --- | --- |
| `benchmark:list` | Show discovered suites, scenarios, and variants. Nothing is built. |
| `benchmark:smoke` | Build the current tree and run every scenario once with smoke arguments. Not performance evidence; no comparisons are reported. |
| `benchmark:prepare --tree LABEL=PATH ...` | Build trees, baseline first, into frozen artifacts plus a layout replica (default for two or more trees; `--replica true` forces one). Writes `artifacts.json`. |
| `benchmark:run SCENARIO --artifacts FILE` | Collect `--rounds` seeded random rounds under the host lock. `--mode` is `screening` (default) or `recorded`. `--cpus` pins workloads; `--perf-event` adds whole-process counters. |
| `benchmark:summarize DIR` | Regenerate `summary.json` from a complete campaign's raw samples. |

Recorded runs require committed, unmodified trees, at least 10 rounds, CPU placement, a `--decision-rule`, `--idle-host true`, one toolchain, and a layout replica when comparing separately built trees. All unmet prerequisites are reported together.

Progress and the human report go to stderr; Bake's result is a compact JSON object with the campaign directory, `summary.json` path, evidence class, and warnings.

## Interpret the results

Read warnings first. Interpret a candidate change against:

- the layout replica's change in the same rounds;
- the spread across rounds;
- the consistency of its direction.

A change within the replica's range, a variant result that reverses in the replica, or a metric that moves both ways is not evidence of improvement. Counts are shown as absolute per-operation differences. CPU-time ratios below 100 resolution quanta are reported as unresolved. Screening results are triage. Recorded results still need the predeclared rule and an investigation record, kept alongside the campaign directory.
