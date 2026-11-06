# Bake-independent Shared Benchmarking

## Requirement

Maintain `base-benchmark` and `bake-benchmark` as separate repositories. The published core dependency graph and public APIs must not require Bake or the adapter. Private development automation may use Bake and `base-project`.

Implement shared measurement, collection, records, validation, provenance, persistence, and summaries in Rust, with no Python dependency for normal workflows. A standalone entry point and the separate Bake adapter must use the same semantic implementation rather than maintain parallel runners.

Workloads, correctness oracles, protocol semantics, operation boundaries, and acceptance budgets remain project-owned. Do not impose one network/CPU workload model or fabricate numerical targets. Distinguish whole-process counters from measured-loop counters; normalized metrics require compatible scopes and denominators. Unimplemented or unavailable capabilities must not be presented as measured or working.

Comparative evidence must distinguish same-binary repetition from same-source rebuild/layout controls and disclose missing controls. Smoke and quick screening must be explicitly non-acceptance evidence. Predeclared expected performance outcomes may be data, but must not turn malformed output, failed correctness checks, or violated required completion/cohort contracts into successful samples. Detailed preparation defaults and task interfaces remain proposals in the architecture plan.

When records are implemented, retain raw values and workload, revision/build/harness, metric-scope, baseline/pair, and measurement-host/environment identities so compatible historical series can be selected. Normalized results must identify their reference and method; do not imply arbitrary cross-host comparability. A historical analysis tool or comparison web UI is a lower-priority follow-up, not a required part of the initial implementation.

Preserve optimization investigations and their evidence regardless of an adopted, rejected, deferred, or open decision. Record the hypothesis, exact variants/method, validity, results or explicit absence of measurement, rationale, and revisit conditions. Keep engineering decisions separate from evidence validity and never present invalid experiments as valid performance conclusions. The [shared investigation convention](../context/investigations.md) defines the initial documentation format; automated tooling remains future work.

## Rationale

The user's 6 November 2026 decision explicitly selected two repositories and requested unified tooling without Python. The [source audit](../reports/benchmarking-landscape.md) found repeated collection machinery but materially different workload, cohort, and metric semantics. The split supersedes the earlier single-crate recommendation.

The [scheduler consumer review and response](../reports/external-review-response.md) identify rebuild/layout variation, quick screening, and declared load errors as design hazards. These motivate explicit evidence classifications, not adoption of the review's unverified numerical observations as thresholds.

The user also requested accumulating same-host or meaningfully normalized results for future graphs and analysis. Preserve evidence needed for that use without prematurely selecting a storage/UI architecture; see the [history plan](../context/history.md).

The user explicitly requested a standard record of measured and rejected optimization attempts, analogous to security audits. This makes negative/inconclusive evidence discoverable and prevents repeated work without new justification; it extends the existing shared performance skill's record-every-attempt guidance with a concrete format.

## Scope

The core library, any CLI, dependency direction to the separate Bake adapter, and claims made in documentation. Detailed runner and record contracts remain proposals until implementation review; this document does not claim they are implemented.

## Validation

Inspect package dependency graphs and public signatures separately from private `bake/` tooling. Review core and adapter together for duplicate semantic logic and test equivalence of their entry points when implemented. Exercise normal collection/summarization without Python, inspect raw metric scopes and normalization, and test differing workload shapes while retaining project-owned oracles. For this initial scaffold, validate the dependency direction and honest status documentation; runtime behavior remains unverified because it does not exist yet.

When comparative APIs exist, test replica/control labels, missing-control disclosures, mode-specific evidence classification, and declared performance outcomes separately from invalid/correctness/completion failures. Inspect paired raw records rather than treat rounds-improved counts as acceptance proof.

When records/analysis exist, verify that changed workload semantics, host/environment epochs, harnesses, or baseline definitions are separated or explicitly qualified; reproduce plotted/normalized values from retained raw evidence. This future runtime validation is not established by the initial scaffold.

Review investigation records for retained candidate identity/evidence, an explicit decision and reason, missing/invalid metrics, and revisit conditions. Test that future indexing/summary tooling does not drop rejected or incomplete attempts or confuse decisions with evidence validity. The initial scaffold supplies a template, not a fabricated completed investigation.
