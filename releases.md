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
