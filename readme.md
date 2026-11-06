# `base-benchmark`

Bake-independent Rust measurement and experiment tooling for Base projects.

**Status: planning scaffold, not a working benchmark library or CLI.** No crates.io release, shared runner, phase-matched hardware counters, or downstream migration exists yet. The first release awaits implementation and review; repository setup does not publish a crate.

## Findings and direction

- [Organization-wide benchmarking audit](reports/benchmarking-landscape.md): 19 repositories and 64 current Rust packages, inspected before these two repositories were created.
- [Architecture and initial implementation plan](context/architecture.md).
- [Current architectural requirements](design/architecture.md).
- [Scheduler consumer feedback and response](reports/external-review-response.md).
- [Follow-up: comparable historical data, graphs, and analysis](context/history.md).
- [Standard investigation records, including rejected attempts](context/investigations.md) and [template](templates/investigation.md).
- [`bake-benchmark`](https://github.com/base-rs/bake-benchmark): the separate Bake adapter.

`base-benchmark` will own one Rust implementation of measurement records, resource accounting, collection, process supervision, validation, evidence journals, provenance, and summaries. Any standalone CLI and the Bake adapter will use that implementation. Normal workflows must not require Python. Workloads, correctness oracles, protocol semantics, and acceptance budgets remain project-specific.

## Development

Install the [Bake launcher](https://github.com/base-rs/bake), then run `cargo bake test`. The private `bake/` workspace member supplies shared Base development tasks; it is not a dependency of the library. The empty library intentionally exposes no placeholder API. Passing scaffold checks is not evidence that the proposed benchmark behavior exists.

## Releases

See [releases.md](releases.md). No releases yet.

## Contributing

Open an issue or pull request on [GitHub](https://github.com/base-rs/base-benchmark).

### Agent Context

Run `cargo bake agent:context:install` to install shared context and skills, then inspect `.agents/context/index.md`. Follow `.agents/implementation.md`, discover `design/*.md` before implementation, and use the installed design review skills.
