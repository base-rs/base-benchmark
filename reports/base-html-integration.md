# `base-html` Integration Review

## Scope

On 7 November 2026, the local, unpublished `base-html` rendering benchmark was migrated onto the first `base-benchmark` and `bake-benchmark` implementation. This report records what the integration exposed and what changed in the shared tooling as a result. The runs below were smoke and screening runs on a development workstation. They are tooling evidence, **not performance evidence about `base-html`**: the host was not idle, the tree was uncommitted, and no decision rule was declared.

## What moved

`base-html` previously maintained its own Python collector of about 285 lines, with about 125 lines of Python tests, plus Rust `getrusage` and record modules. These were removed. The workload now keeps only project-owned concerns:

- workload parameters;
- exact-output and frame-boundary preflights;
- the operation boundary;
- record construction, about 15 lines with `Interval` and `ProcessRecord`.

Preparation, rounds, locking, cleanup, validation, and summaries come from the shared core, through `cargo bake benchmark:*`. Scenarios are declared in the benchmark package's Cargo metadata. The workload's tests now include one that runs the real binary through the shared runner for every scenario. The published `base-html` library has no dependency on either package.

## Findings and changes

| Finding | Change |
| --- | --- |
| Preparation guessed `target/PROFILE/BINARY`, which is wrong with a configured build target or a custom profile directory. | `prepare` reads the executable from Cargo's JSON artifact messages and fails if none is reported. |
| `base-html` has relative path dependencies outside its tree. A replica copied into the output directory could not resolve them. | Replicas are copied to a sibling of the baseline tree, built, and removed. A real `base-html` replica built and its copy was cleaned up. |
| Keeping 100% region coverage in the workload required injecting resource-sampling failures. | `Interval::start_with` accepts a `ResourceSource`. |
| A test that runs an existing binary through the runner needed about 30 lines of manifest construction. | `ArtifactManifest::unprepared` describes such a binary as a non-frozen baseline that can never satisfy recorded prerequisites. |
| All four scenarios repeated identical variants and smoke arguments. | Package-level `arguments`, `smoke-arguments`, `variants`, and `baseline` defaults. |
| Bake printed the entire campaign summary as JSON on stdout. | Tasks return the campaign directory, summary path, evidence class, and warnings. The human report goes to stderr, and `summary.json` holds the full data. |
| Smoke reports printed paired changes from three iterations. | Smoke reports omit comparisons. |
| Context switches of about 0.001 per operation produced changes of up to +1300%. Microsecond CPU time over a few operations produced ±100% changes. | Count metrics are reported as absolute per-operation differences. Ratios require a baseline of at least 100 resolution quanta, and other rounds are reported as unresolved. |
| See the screening observations below. | Warnings for missing CPU placement, timed metrics that moved both ways, and variant comparisons whose sign reverses in the layout replica. These complement the existing build-versus-replica warning. |
| A process may report the same metric name twice, for example from a wrapper, with different scopes. | Such conflicts are excluded and reported instead of being merged. |

### Screening observations that shaped the warnings

Host `aiko`, AMD Ryzen 9 9950X3D, Linux 7.2.14, workstation in use. `list` scenario, five rounds, with a same-source layout replica.

- Without CPU placement, `engine` appeared 19% faster than `reference` in wall time, improving in 5 of 5 rounds.
- With every process pinned to CPU 2, the same comparison showed +0.8% and then +2.1% in two campaigns.
- In the layout replica's variant comparison, the sign was opposite: -0.2%.
- The same binary's `reference` processes alternated between about 16 and 20 µs per operation, even when pinned.

"Better in 5 of 5 rounds" was therefore produced by uncontrolled placement and layout, not by the engine. The revised report now warns about exactly these cases. The per-process bimodality is an open question for `base-html`, to be pursued as an investigation; this report draws no conclusion about it or about engine performance.

## Remaining limits

- `base-html`, and `bake-benchmark` itself, depend on `base-benchmark` through local path dependencies. Before `bake-benchmark` can pass CI or be published, `base-benchmark` must be available from its Git repository or crates.io.
- Only single-sample, single-process workloads have been exercised. Multi-sample batches, multi-process roles, downstream patch preparation, and phase-matched counters remain unimplemented; see [architecture](../context/architecture.md).
- The `base-html` tree has no commits, so recorded mode cannot be used there yet. The tooling rejects it, as intended.
