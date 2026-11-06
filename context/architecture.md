# Architecture and Implementation Plan

## Status and package boundary

This is a proposed implementation plan, not an available API. The user selected two repositories on 6 November 2026, superseding the audit's earlier single feature-gated package recommendation.

`base-benchmark` owns Bake-independent records, measurement, the experiment runner, validation, provenance, persistence, and statistics. A future standalone Rust CLI belongs with this implementation. [`bake-benchmark`](https://github.com/base-rs/bake-benchmark) owns thin task integration and translates project configuration into these same APIs. It must not implement a competing collector, validator, or summarizer. Dependency direction is adapter → core, never core → adapter/Bake. The initial empty crates deliberately have no cross-repository dependency until there is an API to consume.

Optional features may later separate records/resources from process orchestration and CLI dependencies. Exact features, public names, schema version, platform support, and cross-package compatibility policy remain implementation decisions. Do not assume a mandatory executor or benchmark registration framework.

## Proposed core responsibilities

| Area | Responsibility |
| --- | --- |
| Records | Versioned process envelopes containing ordered samples; explicit units, phase, process/thread/cohort, privilege, availability, denominators, parameters, and project extensions. |
| Measurement | Fallible wall/resource snapshots and checked deltas/normalization; preserve sampling resolution and differing capture boundaries. Optional fixed/calibrated loop helpers must not decide operation lifetimes. |
| Runner | Immutable prebuilt commands, interleaving, role-specific placement, bounded readiness/output, endpoint transfer, deadlines, cooperative host locks, owned-child cleanup/reaping, and final completion assertions. |
| Evidence | Frozen source/build/corpus identities and artifact hashes, append-only raw journals, new result directories, explicit in-progress/failed/complete states, and retained failure evidence. |
| Validation | Strict types, finite values, arithmetic consistency, expected sample counts, protocol completion, compatible outputs, and scope-compatible normalization. Project hooks decide semantic equivalence. |
| Summaries | Grouped raw evidence, process-versus-batch identity, paired ratios where declared, medians/minima/maxima, and reproducible regeneration. No automatic universal significance test or acceptance budget. |

Core collection must not build code or prepare fixtures while measuring. Shared Cargo preparation can be an optional Bake-independent facility here if both a CLI and Bake need it; the adapter remains responsible for Bake task registration/configuration. No normal path may depend on Python or wrap historical Python runners.

## Prepared-tree comparisons and build-layout controls

The [scheduler consumer review](https://github.com/base-rs/base-benchmark/blob/main/reports/external-review-response.md) adds concrete preparation needs. Accept labelled explicit source directories or revision archives, a common toolchain/features/profile/flags and lockfile policy, and optionally a downstream workload with per-tree dependency patch overrides. Prepare separate target/artifact sets without checking out revisions, overwriting input trees, or modifying a user's source. Resolve and record each tree's actual dependency graph; identical build policy is not a claim that differing source/dependency graphs are identical.

For comparisons of separately built trees, plan a same-source baseline copy at another recorded path and rebuild it as a **default layout-replica control** during preparation. Record copy/archive policy, source/path identity, and artifact identity; fail or explicitly classify missing control evidence rather than silently treating it as available. Disabling this control must be explicit and visible in results. Rerunning the same executable establishes run-to-run variation, not this rebuild/path variation. No collection step creates a replica or builds code.

Include baseline, candidates, layout replica, and unchanged comparators in the same randomized rounds; record seed and actual order, pair keys, raw values, and per-role placement. Summaries show candidate and replica paired changes, medians/ranges, and rounds-improved counts side by side. Emit an interpretation warning when a claimed effect is not distinguishable from replica/control variation under the predeclared rule. These controls cannot prove causality or create an automatic acceptance threshold.

## Modes, failure outcomes, and measurement-host ownership

- **Smoke:** small correctness, configuration, and lifecycle checks; not performance evidence.
- **Screening:** reduced configurable rounds and paired randomized controls, retained raw results/provenance, concise progress and summaries, explicitly not acceptance evidence. The consumer's suggested 5–10 minute total including preparation is a usability goal, not a guaranteed performance budget.
- **Recorded:** declared policy prerequisites, frozen builds, required rounds/noise controls, complete evidence, and project-owned interpretation. The existing Base guidance's ten-round minimum applies to relevant recorded measurements, not smoke/screening. Mode selection never fabricates missing metrics or proves acceptance.

Allow predeclared **performance outcome contracts** to record socket errors/timeouts and other expected load outcomes as named data, not automatically abort every campaign. They must not relax schema/arithmetic checks, correctness oracles, owned-process cleanup, or required completion/cohort invariants. Invalid samples remain failed/incomplete evidence, never silently successful aggregates. State any error-free assumptions and comparability restrictions in summaries; do not weaken a contract after inspecting outcomes.

The runner executes on the declared measuring host, which may differ from the developer/collector host. Record both identities and per-role CPUs/topology/affinity per sample. Host-side cooperative locking can cover preparation followed by collection to prevent another experiment measuring during builds; preparation still finishes before any measured phase. Provide cancellation-aware blocking lock waiting and visible waiting/round progress, not a requirement for agents to poll. Lock release follows owned-child cleanup. SSH synchronization/deployment automation remains an optional future transport decision, not a dependency of measurement APIs.

Name resource sources and windows explicitly: `wait4` exit rusage covers a child's lifetime including startup/warmup, while selected `/proc` snapshots may cover a narrower measured interval. Neither is automatically the other's denominator or a thread-only metric.

## First implementation and adoption

1. Implement records/strict validation and resource snapshots with small deterministic tests. Keep an explicit capability/availability model for unsupported metrics.
2. Port the reusable local HTML collector safeguards into Rust: whole-process-group cleanup, output/configuration validation, frozen build provenance, exclusive locking, journaling, completion, and grouped summaries. Retain HTML's own output oracle and body lifecycle. Validate without a Python interpreter.
3. Exercise the same runner with scheduler echo, including bounded readiness, endpoints, separate server/client roles, failure cleanup, screening controls, and accurate lifecycle resource scopes. Validate multi-tree preparation with a downstream HTTP workload and patch overrides. Use HTTP full-cohort/final-STATS cases before generalizing phase semantics; Markdown adds nested batches/corpora and optional external comparators.
4. Add the Bake adapter against actual core APIs and verify identical collection/summary semantics through both entry points. Adopt into `base-project` only after demonstrated consumer use.

The local HTML source was unpublished/uncommitted at audit time and is not reproduced here. Its tests and scripts are migration leads, not publicly pinned evidence or a dependency of these repositories. See the report's explicit source limitations.

## Deferred and unverified

The [performance investigation convention](investigations.md) and template standardize narrative records of all attempts, including rejected, unsafe, invalid, and inconclusive ones. Link durable candidate/patch identity and raw results, record why an idea was rejected and when to revisit it, and keep engineering decisions separate from evidence validity. No investigation generator/indexer is implemented; the convention complements existing `base-project-performance` methodology rather than forking it.

The [longitudinal data and analysis follow-up](history.md) captures the user's lower-priority goal of collecting comparable history, graphs, and a small analysis tool or web UI. Preserve record/host/workload identities and raw provenance now; defer storage/UI implementation. Prefer same-host compatible series or explicitly defined paired ratios, never implicit cross-host normalization. The UI must consume shared validated evidence, not a second statistics engine.

Phase-matched hardware counting needs enable/disable boundaries, thread/inheritance semantics, availability/scaling records, and matching denominators. Whole-process `perf stat` totals must not be divided by timed-loop iterations. Neither counter mode is implemented here yet.

Do not invent numeric performance thresholds, portable PMU support, peak-memory/allocation guarantees, pooled latency percentiles, automatic revision checkout/install, or one universal workload trait. Historical scripts/results stay historical; rebuild comparable frozen variants with the same harness for new performance claims. Hosted CI should validate correctness and failures, not performance acceptance.
