# Performance Investigation: <topic>

> Template only. Replace placeholders with facts or explicit “not measured/not available”; do not imply a run occurred. Keep the finished record in the consuming project's performance records.

## Identity and status

- Investigation ID: <project-relative stable ID>.
- Date and author/reviewer: \<date; identities or not reviewed>.
- Evidence class: \<not-measured | smoke | screening | recorded>.
- Evidence validity: \<not-assessed | valid | inconclusive | invalid; scope/qualifications>.
- Decision: \<open | adopted | rejected | deferred>.
- Related/superseded investigations: <links or none>.

## Hypothesis and constraints

\<What is expected to become cheaper, why, for which workload, and against which reference? Link applicable design/correctness/ownership/cancellation constraints. State any standing budget; do not invent one.>

## Frozen variants and preparation

<Link authoritative build/run manifests: baseline/candidate/control/layout-replica source identities, committed candidate patch where needed, measured dependencies, harness, actual toolchain/features/profile/flags, source/build paths, lockfile policy, build commands, artifact hashes, and normal/diagnostic distinction. Explain missing variants or omitted controls.>

## Predeclared method

\<Workload/corpus/parameters and operation boundary; host/environment epoch and role placement; exact commands; round count/order/seed; metrics with scope/units/denominators; controls/pairing; correctness/completion and expected-error contracts; interpretation rules. Link metadata rather than hand-copying it. Name missing capabilities and deviations.>

## Evidence and results

- Raw bundles and hashes: \<durable paths/links or not measured>.
- Generated summary and analysis version: \<path/identity or unavailable>.
- Correctness/completion/failure state: <facts>.
- Diagnostics: <separate artifact links and their different scope>.
- Not measured or incompatible: <explicit list and reasons>.

| Metric and scope | Baseline median \[min, max] | Candidate median \[min, max] | Paired change / control behavior | Validity and interpretation |
| --- | --- | --- | --- | --- |
| \<metric; unit; phase/role; denominator> | <derived from summary or unavailable> | <derived from summary or unavailable> | \<paired method; same-binary/layout/control results> | <what is and is not supported> |

\<Keep screening non-acceptance, unavailable values distinct from zero, expected performance errors distinct from correctness failures, and invalid numbers out of valid comparative conclusions. Preserve incomplete/rejected runs.>

## Decision and code disposition

\<What was kept, rejected, or deferred, and why? Distinguish no measured benefit, noise/control ambiguity, trade-off, complexity, unsafe semantics, and invalid setup. Link implementation/patch disposition and review. Record uncertainty; no throughput claim from an invalid or incomparable run.>

## Revisit conditions

\<What specific new evidence, changed implementation, hardware, workload, or constraint would justify another attempt? Link a later investigation if this one is superseded; retain this record.>
