# `base-benchmark`

Bake-independent Rust measurement and experiment tooling for Base projects.

Workloads stay project-owned: a workload binary measures its own operations and writes one process record. `base-benchmark` does the rest:

- builds labelled source trees into frozen executables with build and dependency provenance, plus same-source layout replicas;
- runs every subject in seeded random rounds under a host-wide lock, from equal-length paths with a controlled environment and random stack padding, with CPU placement, deadlines, and process-group cleanup, alone or as one role of a session with services and a driver;
- rejects malformed or inconsistent evidence, including subjects that did different work;
- summarizes paired changes with explicit metric scopes, resolution-aware ratios, sign tests, and interpretation warnings.

The `base-benchmark` command and the [`bake-benchmark`](https://github.com/base-rs/bake-benchmark) tasks call the same operations. No Python is involved.

**Status:** an initial release, validated by migrating the `base-html` rendering benchmark. Multi-process scenarios, with services such as a server and a driver such as a load client, are being implemented: their session lifecycle is, and the driver's measured window, service CPU, and placement checks are not yet. Downstream patch preparation and phase-matched hardware counters are not implemented yet; see [architecture](context/architecture.md).

## Usage

See [Using the Shared Benchmark Tooling](context/usage.md) for writing a workload, declaring scenarios in Cargo metadata, and running smoke, screening, and recorded campaigns. Run `base-benchmark help` for command options. Workloads depend on the crate with `default-features = false`, which provides only records and `Interval`.

## Design and findings

- [Architecture](context/architecture.md) and [requirements](design/architecture.md), including the agreed, not yet implemented, [multi-process roles](design/roles.md).
- [`base-html` integration review](reports/base-html-integration.md): what the first migration exposed and what changed.
- [Organization-wide benchmarking audit](reports/benchmarking-landscape.md), and the [scheduler consumer feedback and response](reports/external-review-response.md).
- [Investigation records, including rejected attempts](context/investigations.md), with a [template](templates/investigation.md).
- [Follow-up: comparable historical data and analysis](context/history.md).

## Development

Install the [Bake launcher](https://github.com/base-rs/bake), then run `cargo bake test`, or `cargo bake test:coverage --all-targets true` for the coverage gate. Integration tests build small fixture workspaces with Cargo and run shell workloads; they need Linux, `cargo`, `rustc`, and `git`. The private `bake/` workspace member supplies shared Base development tasks and is not a dependency of the library.

## Releases

See [releases.md](releases.md).

## Contributing

Open an issue or pull request on [GitHub](https://github.com/base-rs/base-benchmark).

### Agent Context

Run `cargo bake agent:context:install` to install shared context and skills, then inspect `.agents/context/index.md`. Follow `.agents/implementation.md`, discover `design/*.md` before implementation, and use the installed design review skills.
