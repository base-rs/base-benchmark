# `base-html` Integration Review

## Scope

On 7 November 2026, the local, unpublished `base-html` rendering benchmark was migrated onto the first `base-benchmark` and `bake-benchmark` implementation. This report records what the integration exposed and what changed in the shared tooling as a result. The runs below were smoke and screening runs on a development workstation. They are tooling evidence, **not performance evidence about `base-html`**: the host was not idle, the tree was uncommitted, and no decision rule was declared.

## What moved

`base-html` previously maintained its own Python collector of about 285 lines, with about 125 lines of Python tests, plus Rust `getrusage` and record modules. These were removed. The workload now keeps only project-owned concerns:

- workload parameters;
- exact-output and frame-boundary preflights;
- the operation boundary;
- record construction, about 35 lines with `Interval` and `ProcessRecord`.

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

The unpinned "better in 5 of 5 rounds" was therefore not reproduced under pinning, and the pinned changes were within layout-replica variation: in the second pinned campaign, the replica alone moved `engine` by -3.1% (better in 4 of 5 rounds) and `reference` by +1.0% (worse in 5 of 5). The revised report warns about exactly these cases. The per-process bimodality is an open question for `base-html`, to be pursued as an investigation; this report draws no conclusion about it or about engine performance.

## Review of the integration

Two independent reviews followed: one of the core, and one of the `base-html` migration and the Bake adapter. Their findings, and Emery Berger's "Performance Matters" talk on layout effects, led to these further changes.

| Finding | Change |
| --- | --- |
| The resolution rule applied only to declared resolutions. Ratios still appeared in `summary.json` for counts, and ratios computed from only the resolvable rounds selected large baselines. | Every metric has a quantum, by default one unit. Percentages require every round to resolve, counts are never ratios, and differences below the quantum count as ties. |
| Pairs compared metrics with different units, phases, or sources without warning; metrics redefined between processes were still merged. | Pairs require identical definitions and warn about incompatible or missing metrics; redefining processes are excluded. |
| Smoke `summary.json` still contained pairs. | Smoke summaries have no pairs. |
| Variant pairs without a replica, and replica checks that could not be resolved, were silent. | Both are warnings. |
| A descendant that escaped the process group could hang the runner while it held the lock; a dropped signal guard left signals ignored, and a second Ctrl-C could not exit. | Samples fail after a 2 s output grace period. A process-wide handler cancels guarded work on the first signal and exits on the next. |
| A replica copy was created after the builds, could collide with a concurrent run, and leaked on interrupt; preparation ignored signals. | Copies are created exclusively, with a per-process name, before building, and removed after a failure or a first signal; preparation is signal-guarded and checks cancellation between builds. A second signal or `SIGKILL` still leaves the copy behind. |
| Provenance missed path dependencies outside the tree, Cargo configuration, and most build-affecting environment variables. | All are recorded, and recorded mode requires them to be committed and identical. |
| Nothing checked that subjects ran the same workload with the same parameters, so a changed default in a candidate tree went unnoticed. | Records must share workload and signature, and each variant's parameters must match across artifacts. |
| Scenarios could not pin iteration counts without colliding with smoke arguments. | `measure-arguments` apply only outside smoke runs. |
| The "host-wide" lock followed `$TMPDIR`, and other users could not open it. | `/tmp/base-benchmark-host.lock`, opened read-only when present and created world-writable. |
| Workload binaries linked the whole runner. | A default `runner` feature; workloads use `default-features = false`. |
| User and system CPU time are tick-sampled on Linux, so 1 µs resolution overstated them. | A precise `cpu_time` total, and a 10 ms split. |
| The adapter duplicated core defaults, and nothing tested standalone manifests or command and task equivalence. | Defaults are resolved only in the core; the adapter's tests cover both. |
| Layout, such as environment size and executable path, can bias every process of one subject the same way. | A cleared environment, equal-length staged paths, seeded stack padding, several replicas, and an exact sign test. Stabilizer-style runtime re-randomization and Coz causal profiling are deferred; see [architecture](../context/architecture.md). |

### Screening with layout controls

On the same host, with the workstation still in use, the `list` scenario was re-run with two layout replicas, seven rounds, seed 20261107, pinned to CPU 2, and stack padding enabled. As before, this is tooling evidence only.

- Every variant comparison, in the baseline and both replicas, moved both ways across rounds. Medians were +0.1% to +0.4% and sign tests 0.45 to 1.0, and each was warned about as inconclusive.
- User and system CPU time were reported as below resolution in every round, rather than as percentages.
- 6 of 21 `reference` processes ran at about 19.7 µs per operation against a median of about 16.0; no `engine` process did. The slow processes appeared in the baseline and in both replicas, and showed no simple relation to the padding length, which ASLR also offsets. With padding, the effect is spread across subjects instead of being fixed to one build.

## Remaining limits

- Only single-sample, single-process workloads have been exercised. Multi-sample batches, multi-process roles, downstream patch preparation, and phase-matched counters remain unimplemented; see [architecture](../context/architecture.md).
- At the time of these runs `base-html` had no commits, so recorded mode could not be used there; the tooling rejected it, as intended. Its dependencies were then local checkouts outside the repository; they are now the released crates.
- The per-process bimodality seen in `reference` remains unexplained.
