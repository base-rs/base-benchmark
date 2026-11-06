# Performance Investigation Records

## Purpose and status

The user requested a standard way to retain measured and rejected optimization attempts, analogous to security audit records, on 6 November 2026. The existing `base-project-performance` skill already says to record every attempt, including rejected and unsafe ideas. This guide supplies a **shared record convention and template** for doing that consistently; it does not replace that skill's measurement methodology or implement a record generator/indexer.

The durable unit is an **investigation**, not just a successful benchmark run. Record a hypothesis even if it is rejected for correctness or abandoned before valid measurement. Never invent numbers to complete the form. Preserve unsuccessful runs and discarded candidate source so another person or agent can understand the conclusion and avoid repeating it without new evidence.

## Location and identity

Use `performance/investigation-YYYY-MM-DD-topic.md` in the project that owns the optimization, with a stable repository-relative identifier. Link records from an existing performance overview or `performance/index.md`, and link that overview from the project's readme. Existing historical records may remain in place; do not force a rewrite or migration of their names/formats.

Keep raw machine-readable bundles, generated summaries, small committed patches, and exact drivers under the project's established benchmark results directory, normally `benchmarks/<subject>/results/<topic>/`. Link those artifacts from the investigation instead of copying hand-transcribed numbers. Large profiles can use a durable external artifact reference plus digest and retention/access notes; a machine-local path alone is not reproducible evidence. A record can reference several runs or projects, but identify which project owns each artifact and commit the dependent records together with the relevant dependency changes when possible.

Start from the [investigation template](https://github.com/base-rs/base-benchmark/blob/main/templates/investigation.md). The prose record and raw run journals are complementary: the former explains why and what was decided; the latter permits validation/recalculation. Future tooling may generate an initial record or index, but must use the same underlying evidence rather than a parallel statistics pipeline.

## Required content

1. **Identity and question:** date, author/reviewer where applicable, stable investigation ID, workload/corpus/scenario, hypothesis, expected mechanism, relevant design/correctness constraints, and links to earlier attempts.
2. **Candidate and baseline:** exact frozen revisions or base-plus-committed patch/source digest, measured dependencies, harness/toolchain/features/profile/flags, source/build paths and artifact identities. Preserve rejected candidate code or describe precisely why no runnable variant was produced.
3. **Predeclared method:** smoke/screening/recorded classification, metric scopes/units/denominators, host/environment/placement, commands, rounds/order/seed, baseline-self/layout-replica/unchanged controls, interpretation rules, and explicit unavailable capabilities or deviations. Link the run manifest instead of duplicating authoritative metadata unnecessarily.
4. **Evidence and validity:** raw bundles and generated summary paths/digests, correctness/completion outcomes, declared performance errors, failure/incomplete state, comparable metrics with medians/ranges and paired ratios where appropriate, control behavior, and measurements not taken. Label diagnostic results separately.
5. **Decision and rationale:** what was kept/rejected/deferred, what the evidence supports, trade-offs/unsafe invariants/complexity, missing evidence, code disposition, and exact reasons. Do not turn an invalid experiment into a claim that an optimization is slow or ineffective.
6. **Revisit conditions:** the changed workload, hardware, upstream implementation, correctness contract, or new measurement capability that could justify another attempt. Link superseding investigations rather than erasing the old reasoning.

## Separate evidence validity from engineering decision

Record these independently:

- **Evidence class:** `not-measured`, `smoke`, `screening`, or `recorded`.
- **Evidence validity:** `not-assessed`, `valid`, `inconclusive`, or `invalid`, qualified by the stated workload/metric/contract. A valid screen is still not acceptance evidence; a partial campaign may contain valid diagnostic facts without supporting a comparative verdict.
- **Decision:** `open`, `adopted`, `rejected`, or `deferred`. State the reason and actual code disposition. “Adopted” records an engineering action, not an automatic performance acceptance badge; any claimed gain must independently meet the applicable evidence requirements.

Typical rejected records include no meaningful improvement beyond controls, higher cost in another metric/workload, correctness/ownership/cancellation violations, unacceptable complexity, or an invalid measurement setup that requires a new run. State which reason applies. If an idea is unsafe and was never benchmarked, use `not-measured`; if the harness was wrong, retain `invalid` evidence and do not report its values as a valid regression. Never change a predeclared interpretation rule or expected-error contract after seeing a result without recording that as a new analysis/run.

## Retention and future analysis

Retain unfavorable, invalid, incomplete, and inconclusive attempts alongside wins; do not only publish the best run. Correct factual errors transparently and preserve supersession links. Before trying an optimization, search earlier investigations and explain what changed if revisiting one. Do not silently rewrite historical summaries when a schema or analysis bug is fixed: preserve original evidence and identify the regenerated analysis/tool version.

The [history/analysis follow-up](history.md) can index these decisions alongside run metadata. Graphs must exclude or clearly distinguish invalid, screening, missing, and incomparable measurements; rejected is an engineering decision, not a numeric metric or an instruction to discard its evidence. Standardizing this record now does not make a web UI or database an initial implementation requirement.
