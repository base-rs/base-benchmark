# Response to Scheduler Consumer Review

The [external review](external-review.md) was received on 6 November 2026 while these repositories were being scaffolded. It usefully tests whether the proposed core would replace real optimization tooling rather than just reproduce archival collection. This response changes the **plan**, not implementation status. The reviewer-reported percentages and host-local experiments have not been independently verified; they are not new performance claims or accepted thresholds from this project.

| Finding | Disposition |
| --- | --- |
| 1. Missing audit report | Resolved in the scaffold: [public findings](benchmarking-landscape.md) are included and cross-linked. The review captured an earlier intermediate state. |
| 2. Build-layout noise | Adopted in the comparative preparation plan: baseline-copy rebuild control on by default for separately built trees, paired alongside candidates, distinct from rerunning one binary. Warn when a claimed change is not distinguishable from that control under the declared interpretation; no universal numeric cutoff or causal proof. |
| 3. Comparing prepared trees | Adopted: labelled explicit source directories or revision archives, common build policy, optional downstream patch overrides, isolated identified artifact sets, and replica preparation. No implicit revision checkout or source rewriting. Shared preparation belongs in the core if both CLI and Bake use it. |
| 4. Same-round controls and expected errors | Adopted: named comparator/replica roles in recorded randomized rounds. Explicitly declared performance outcomes may include timeout/socket-error counts; they cannot override correctness, schema, arithmetic, lifecycle, or required-cohort validity. Invalid samples remain retained but excluded from successful summaries. |
| 5. Screening versus recorded runs | Adopted: smoke, screening, and recorded modes with explicit evidence classifications. Screening retains provenance/raw records, uses configurable reduced rounds, and prints paired changes and rounds-improved counts without acceptance claims. The suggested 5–10 minute total is a usability goal to evaluate, not a guaranteed budget. |
| 6. Host, placement, waiting, resources | Adopted as plan details: collector/measurement host distinction, first-class per-role placement, host-side lock covering preparation and collection, blocking cancellation-aware waiting with progress, and explicit process-lifetime versus measured-window resource sources. Remote transport/deployment automation remains undecided. |

## Interpretation limits

A same-source rebuild can reveal build/layout sensitivity, but it is not a calibrated statistical bound and cannot prove that a candidate effect is causal. Likewise, consistently better rounds are descriptive, not sufficient acceptance evidence. Preserve source/build path identities, replica construction, random seed and actual execution order, paired raw values, unavailable metrics, and declared interpretation rules. If a replica is disabled or cannot be built, say so and retain the resulting evidence limitation.

An expected load-generator timeout may be part of a declared outcome contract, but it must not silently convert a broken correctness check or an incomplete required cohort into success. A failure-aware report should show throughput/error counts and the precise completion semantics; comparisons that require an error-free matched workload must still reject incompatible samples. The contract is set before collection, never broadened after observing an unfavorable result.

## Implementation validation still required

Test deterministic/shuffled schedules, paired summary arithmetic, replica omission/availability, prepared-tree paths and archives, downstream patch identity, immutable source handling, interrupted builds, lock ownership/wait cancellation, role placement, per-host provenance, and declared-error versus invalid-sample separation. Then validate a small HTML path plus scheduler echo and a downstream HTTP workload. None of these runtime capabilities or migrations exists in the initial scaffold.

## Implementation status, 7 November 2026

The first implementation covers these items:
- labelled explicit source trees with a common build policy;
- a default same-source layout replica for comparisons of two or more trees;
- seeded random rounds that include the replica;
- side-by-side replica pairs with warnings;
- smoke, screening, and recorded modes, with recorded prerequisites enforced;
- a host-wide lock with visible, cancellation-aware waiting and round progress;
- recorded host identity and CPU placement.

Not yet implemented:
- revision archives as inputs, and downstream workloads built through patch overrides;
- unchanged external comparators as plan subjects;
- predeclared expected-error outcome contracts, beyond reporting load errors as ordinary workload metrics;
- per-role server/client placement and per-role resource sources.

See the [architecture](../context/architecture.md). The [`base-html` integration](base-html-integration.md) independently reproduced the review's central point on a different workload: an apparently consistent improvement disappeared under CPU placement, and its sign reversed in the layout replica.
