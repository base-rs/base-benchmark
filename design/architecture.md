# Bake-independent Shared Benchmarking

## Requirement

Maintain `base-benchmark` and `bake-benchmark` as separate repositories. The published core dependency graph and public APIs must not require Bake or the adapter. Private development automation may use Bake and `base-project`.

Implement shared measurement, collection, records, validation, provenance, persistence, and summaries in Rust, with no Python dependency for normal workflows. A standalone entry point and the separate Bake adapter must use the same semantic implementation rather than maintain parallel runners.

Workloads, correctness oracles, protocol semantics, operation boundaries, and acceptance budgets remain project-owned. Do not impose one network/CPU workload model or fabricate numerical targets. Distinguish whole-process counters from measured-loop counters; normalized metrics require compatible scopes and denominators. Unimplemented or unavailable capabilities must not be presented as measured or working.

Comparative evidence must distinguish same-binary repetition from same-source rebuild/layout controls and disclose missing controls. Smoke and quick screening must be explicitly non-acceptance evidence. Predeclared expected performance outcomes may be data, but must not turn malformed output, failed correctness checks, or violated required completion/cohort contracts into successful samples. Comparisons must not report changes below the measurement's resolution as percentages, and smoke reports must not report comparisons.

Records must retain raw values and workload, revision/build/harness, metric-scope, baseline/pair, and measurement-host/environment identities so compatible historical series can be selected. Normalized results must identify their reference and method; do not imply arbitrary cross-host comparability. A historical analysis tool or comparison web UI is a lower-priority follow-up, not a required part of the initial implementation.

Preserve optimization investigations and their evidence regardless of an adopted, rejected, deferred, or open decision. Record the hypothesis, exact variants/method, validity, results or explicit absence of measurement, rationale, and revisit conditions. Keep engineering decisions separate from evidence validity and never present invalid experiments as valid performance conclusions. The [shared investigation convention](../context/investigations.md) defines the initial documentation format; automated tooling remains future work.

## Rationale

The user's 6 November 2026 decision explicitly selected two repositories and requested unified tooling without Python. The [source audit](../reports/benchmarking-landscape.md) found repeated collection machinery but materially different workload, cohort, and metric semantics. The split supersedes the earlier single-crate recommendation.

The [scheduler consumer review and response](../reports/external-review-response.md) identify rebuild/layout variation, quick screening, and declared load errors as design hazards. These motivate explicit evidence classifications, not adoption of the review's unverified numerical observations as thresholds.

The user also requested accumulating same-host or meaningfully normalized results for future graphs and analysis. Preserve evidence needed for that use without prematurely selecting a storage/UI architecture; see the [history plan](../context/history.md).

The user explicitly requested a standard record of measured and rejected optimization attempts, analogous to security audits. This makes negative/inconclusive evidence discoverable and prevents repeated work without new justification; it extends the existing shared performance skill's record-every-attempt guidance with a concrete format.

## Scope

The core library, the `base-benchmark` command, dependency direction to the separate Bake adapter, record and summary semantics, and claims made in documentation. The architecture guide lists which planned capabilities are not yet implemented.

## Validation

Inspect `cargo tree -p base-benchmark -e normal` for Bake, the adapter, or Python, and the adapter's tasks for semantic logic not delegated to `Workspace`. The adapter's tests compare the command and the tasks under equivalent plans for identical summaries and errors. Workspace integration tests build a fixture workspace and collect it without Python.

Tests cover:

- process-group cleanup on timeout, cancellation, and signals, and failure rather than a hang when a descendant escapes the group;
- rejection of malformed, oversized, signature-mismatched, workload-mismatched, parameter-mismatched, or changed-executable samples;
- the controlled launch: cleared environment, working directory, equal-length staged paths, and stack padding;
- journal failure reporting;
- recorded prerequisites, including path-dependency state and build settings;
- layout replica preparation, exclusive creation, and cleanup after a failed build, and cancellation between builds;
- normalization only of measured-phase totals, except the approximate service CPU per request over the driver's window that [multi-process roles](roles.md) requirement 9 adds;
- all-or-nothing resolution-aware ratios, ties below the quantum, and the exact sign test;
- incompatible and missing metrics between subjects;
- empty smoke pairs;
- each interpretation warning.

Test differing workload shapes while keeping oracles project-owned. So far only single-process, single-sample workloads (`base-html`) have been exercised; multi-sample batches remain to be validated against Markdown, and the [multi-process roles](roles.md) against scheduler echo and HTTP hello, before they are claimed. Test declared performance outcomes separately from invalid, correctness, and completion failures. Failures are tested; declared outcome contracts are not implemented, so nothing yet distinguishes them.

The shared coverage task requires every measured source region to be covered.

For comparative claims, inspect paired raw records and replica/control pairs rather than treat rounds-improved counts as acceptance proof. The [`base-html` integration report](../reports/base-html-integration.md) records how real screening runs exposed placement and layout hazards.

When history or analysis tooling exists, verify that changed workload semantics, host/environment epochs, harnesses, or baseline definitions are separated or explicitly qualified; reproduce plotted or normalized values from retained raw evidence.

Review investigation records for retained candidate identity and evidence, an explicit decision and reason, missing or invalid metrics, and revisit conditions. Future indexing or summary tooling must not drop rejected or incomplete attempts, or confuse decisions with evidence validity.
