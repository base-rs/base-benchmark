# Base Benchmarking Landscape

> Update, 7 November 2026: an initial implementation exists and replaced the `base-html` Python collector; see the [architecture](../context/architecture.md) and the [integration review](base-html-integration.md). The audit below describes the 6 November source snapshot.

## Decision

Create **two repositories with one shared Rust implementation**:

- [`base-benchmark`](https://github.com/base-rs/base-benchmark): Bake-independent measurement, records, collection, process supervision, validation, provenance, evidence journals, and summaries. A future standalone CLI uses these same APIs.
- [`bake-benchmark`](https://github.com/base-rs/bake-benchmark): thin Bake task/configuration integration depending on the core, not a second runner.

The user selected this split on **6 November 2026**, superseding the audit's earlier recommendation to begin with one feature-gated `bake-benchmark` package. Normal workflows should require **no Python**: port reusable behavior and regression tests into Rust rather than wrap existing scripts. Workloads, oracles, protocol semantics, operation boundaries, and acceptance budgets remain project-specific.

**Implementation status:** these repositories contain initial scaffolding, findings, and design documents only. No shared runner, record API, benchmark task family, hardware-counter implementation, or downstream migration exists yet. This audit ran no upstream builds or benchmarks and establishes no speed ranking or accepted numerical performance target.

## Scope and inventory

The pre-creation GitHub inventory on 6 November 2026 contained **19 API-visible public, unarchived repositories**, all on `main`; `base-markdown` was the sole fork. The inventory excludes these two new repositories and the unpublished local HTML prototype. The [organization API](https://api.github.com/orgs/base-rs/repos?per_page=100\&page=1\&type=all\&sort=full_name\&direction=asc) is mutable; the source trees below pin the revisions inspected. Invisible/private repositories, other branches, deleted history, external artifacts, and full historical reproduction are outside scope. No returned repository was inaccessible.

Sources were inspected for Cargo manifests/targets, `benches/`, `benchmarks/`, examples and drivers, performance/result records, profiling/resource accounting, and task/guidance integration. Negative findings are bounded to that tracked-source snapshot, not a claim that no historical benchmark ever existed. Current source does not imply verified current build compatibility.

**E** = owns executable benchmark/probe source or its driver, including helper clients. **R** = historical performance documentation only. **N** = neither an owned current harness nor performance results. A library measured downstream is not itself E. At repository level, E takes precedence over R. Categories are exclusive; E does not mean an independently maintained harness or a successfully run workload.

| Unit | E | R | N | Total |
| --- | ---: | ---: | ---: | ---: |
| Pre-creation repositories | 3 | 1 | 15 | **19** |
| Current Rust package manifests | 14 | 1 | 49 | **64** |

The package arithmetic is **25 tooling + 14 scheduler + 14 HTTP + 11 remainder = 64**. Separately, 48 retained historical/template manifests are experiment inputs, not 48 additional maintained packages. Local HTML has four packages, two E and two N, excluded from all organization counts.

### Complete current repository/package inventory

Each linked tree pins the inspected revision. Parenthesized categories apply to the listed packages; semicolons separate differing categories within a repository.

| Repository / pinned source | Current packages | Count | Repository disposition |
| --- | --- | ---: | --- |
| [bake](https://github.com/base-rs/bake/tree/8d4c0739694c0c305922fedc7f6ebefbfffd38fb) | `bake`, `bake-macros`, `bake-rust-tasks` (N) | 3 | N: task APIs/launcher |
| [bake-agent-context](https://github.com/base-rs/bake-agent-context/tree/591b5af4162f397cbdd5356b0ff596d86cbccd42) | `bake-agent-context`, `bake-agent-context-tasks` (N) | 2 | N: guidance installation |
| [bake-cargo](https://github.com/base-rs/bake-cargo/tree/8687e0392a7425bf2b6dfe2f5fb6f38ae58f12fc) | `bake-cargo`, `bake-cargo-project` (N) | 2 | N: release/build tooling |
| [bake-license](https://github.com/base-rs/bake-license/tree/d6d31ce245d23d631f17a6348d29c59ac72d28b7) | `bake-license`, `bake-license-project` (N) | 2 | N |
| [bake-markdown](https://github.com/base-rs/bake-markdown/tree/daf30a04d3e593f2ee902c098d348cc1a17f5fb4) | `bake-markdown`, `bake-markdown-project` (N) | 2 | N: normalization, not parser measurement |
| [bake-readme](https://github.com/base-rs/bake-readme/tree/819cb17a9120e915dcc07458948279c3136a67ec) | `bake-readme`, `bake-readme-project` (N) | 2 | N |
| [bake-releases](https://github.com/base-rs/bake-releases/tree/a3f18be4e01b0e46dd5942125e8ecbe9de0f7a01) | `bake-releases`, `bake-releases-project` (N) | 2 | N |
| [bake-releases-cargo](https://github.com/base-rs/bake-releases-cargo/tree/50cb9a8f6dfde703ad6a74adf5a6ed68dbd70773) | `bake-releases-cargo`, `bake-releases-cargo-project` (N) | 2 | N |
| [bake-test](https://github.com/base-rs/bake-test/tree/72de25667bad4b1ce048fe6d9446ec55ef5e3909) | `bake-test`, `bake-test-project` (N) | 2 | N: tests/coverage |
| [base-design](https://github.com/base-rs/base-design/tree/1241c9fb9a9c54f8538e83b86748732dc6c2d8c8) | `base-design`, `base-design-bake` (N) | 2 | N |
| [base-project](https://github.com/base-rs/base-project/tree/5de408e4cd77a2ff9eb21a9039d27c2094f8939e) | `base-project`, `base-project-bake` (N) | 2 | N: benchmark/performance policy |
| [base-security](https://github.com/base-rs/base-security/tree/f3c50c77957e74656aee4ca710dfe40433165f28) | `base-security`, `base-security-bake` (N) | 2 | N |
| [base-scheduler](https://github.com/base-rs/base-scheduler/tree/cd51eeeee29c0658722868355b1bc04f7e2679ec) | `base-scheduler`, `base-scheduler-bake`, `base-scheduler-conformance`, `base-scheduler-tokio`, `base-scheduler-native`, `base-scheduler-selector`, `base-scheduler-iouring`, `base-scheduler-timers` (N); `base-scheduler-examples`, `base-scheduler-benchmark-echo`, `base-scheduler-benchmark-timers`, `base-scheduler-benchmark-files`, `base-scheduler-benchmark-buffers`, `base-scheduler-benchmark-writes` (E) | 14 | E |
| [base-http](https://github.com/base-rs/base-http/tree/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5) | `base-http`, `base-http-bake`, `base-http-client`, `base-http-server`, `base-http1`, `base-http-protocol`, `base-http-fixtures`, `base-http1-fuzz` (N); `base-http-client-benchmark`, `base-http-hello-benchmark`, `base-http-fixed-rate-client`, `base-http-layer-probe`, `base-http-ring-probe`, `base-http-remote-echo-client` (E) | 14 | E |
| [base-markdown](https://github.com/base-rs/base-markdown/tree/e968682e8d70da184f69ae693de2d4c3811b0407) | `base-markdown`, `markdown-comparison` (E); `markdown-generate`, `base-markdown-project`, `markdown-fuzz` (N) | 5 | E |
| [base-console](https://github.com/base-rs/base-console/tree/1e79cd4545e7e68bbc8993f8ed865030cb8235b9) | `base-console`, `base-console-bake` (N) | 2 | N |
| [base-pool](https://github.com/base-rs/base-pool/tree/9f5bdd61902c6d1d3c30a43d5c80892f9be99888) | `base-pool` (R); `base-pool-bake` (N) | 2 | R: downstream [observations](https://github.com/base-rs/base-pool/blob/9f5bdd61902c6d1d3c30a43d5c80892f9be99888/design/port.md#L55-L67), not an owned suite |
| [base-endpoint](https://github.com/base-rs/base-endpoint/tree/783f4cb13aa98cb74e5ac0a2a1cbff0eb3bc9d96) | `base-endpoint`, `base-endpoint-bake` (N) | 2 | N |
| [.github](https://github.com/base-rs/.github/tree/03191e042e70114b0b3f561bdb5852d8a8915e70) | None | 0 | N: organization profile |

Scheduler retains [two rescue-cost template manifests](https://github.com/base-rs/base-scheduler/tree/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/timers/results/rescue-costs/harness); HTTP retains 46 manifests in [historical hello studies](https://github.com/base-rs/base-http/tree/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/benchmarks/hello/results), including copied before/after library workspaces. These are excluded from current totals. Markdown's historical generated driver packages are described in scripts but are not extra tracked manifests. Root-only discovery would miss HTTP's excluded suites and Markdown's separate comparison workspace; recursive discovery would mistake snapshots for active workloads. See [HTTP workspace](https://github.com/base-rs/base-http/blob/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/Cargo.toml#L1-L36) and [Markdown comparison manifest](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/Cargo.toml#L1-L13).

## What exists today

### Scheduler: process orchestration and component-specific accounting

Five explicit benchmark packages plus an example driver cover echo, timers, files, buffers, and writes. Echo launches separate server/client processes, validates returned bytes, and counts requests sent and completed **inside the measurement window**. Paced latency begins at scheduled send, but backlog is abandoned at the stop boundary. This is not the same cohort policy as HTTP fixed-rate draining. [Echo client](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/echo/src/client.rs#L36-L149).

Timer scenarios have different operation meanings: `cycle` counts **33 operation attempts per iteration**. Aggregate elapsed/operations under concurrency is throughput, not response latency. Periodic lateness has a warmup/sample window while its CPU envelope spans runtime construction through shutdown. Files measure page-cache-hot I/O without a durability/fsync claim and report both aggregate elapsed/operations and summed task elapsed/operations. [Timers](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/timers/src/lib.rs#L49-L178), [periodic](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/timers/src/periodic.rs#L49-L124), [files](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/files/src/lib.rs#L22-L111).

Buffer pipelines add ownership, registration, zero-copy, queue depth, TCP/UDP, placement, and retirement checks. Writes adds synthetic preparation and send-buffer pressure with perf/tracepoint diagnostics. Preserve their named metrics and measurement boundaries rather than converting everything into closure throughput. [Pipeline accounting](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/buffers/src/bin/pipeline/linux.rs#L585-L684), [writes runner](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/writes/run.py#L56-L184).

### HTTP: readiness, protocol equivalence, and complete cohorts

The client suite compares Base/Hyper pooled paths, direct calls and connection-only controls, TCP/TLS, fresh/reused connections, and concurrency. It coordinates preparation, settling, warmup, timing, and cleanup; throughput includes the final in-flight drain. A reduced control is not automatically subtractable from a full path to identify one component's cost. [Client methodology](https://github.com/base-rs/base-http/blob/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/benchmarks/client/readme.md#L5-L67), [barriers](https://github.com/base-rs/base-http/blob/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/benchmarks/client/src/established.rs#L14-L64).

Hello performs independent status/version/body/header/reuse preflight and rejects wrk errors, but wrk does **not** validate timed response-body bytes. Its fixed-rate client instead validates payloads and drains the **entire scheduled rate×duration cohort**, including late completions. Scheduled latency, wire RTT, send delay, dispatch lag, and completion-in-window remain different metrics. Do not silently replace these semantics with echo's stop-window policy or pool unlike histograms. [Hello preflight](https://github.com/base-rs/base-http/blob/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/benchmarks/hello/run.py#L34-L79), [fixed-rate client](https://github.com/base-rs/base-http/blob/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/benchmarks/hello/fixed-rate-client/src/main.rs#L195-L382).

The server announces `READY address`; teardown sends SIGTERM, escalates on timeout, and checks final `STATS`. Client exit alone is insufficient: server failures must be zero, request counts must cover preparation/warmup/measurement, and fresh-mode connection totals must match requests. These are reusable lifecycle mechanics with project-owned final assertions. [HTTP supervision](https://github.com/base-rs/base-http/blob/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/benchmarks/client/run.py#L28-L87), [echo readiness/cleanup](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/echo/run.py#L44-L104).

### Markdown: calibrated CPU loops, corpora, and nested statistics

The [Criterion target](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/bench.rs#L1-L29) loads README input outside timing and repeatedly converts it. This convenience target need not be replaced. The separate comparison workspace is richer: Rust/Ruby adapters, synthetic/real documents, correctness checks, allocation/profiling paths, and 39 immediate historical study directories.

The current custom adapter warms each document for at least 200 ms, calibrates approximately 150 ms batches, measures seven batches, and emits a median plus batch values. It sorts batches in place, losing temporal order; historical drivers sometimes sort a clone. **Seven batches in one process are not seven independent process rounds.** Construction/input setup is outside timing, but destruction of each returned string is inside the conversion iteration. [Rust adapter](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/src/main.rs#L11-L169), [Ruby adapter](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/adapter.rb#L32-L52).

Synthetic inputs repeat complete UTF-8 blocks to a minimum size; actual bytes matter. Real inputs carry digests, licenses, attribution, and upstream identity. Exact and normalized HTML agreement are different claims. Corpus/configuration semantics must stay project-owned. [Preparation](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/prepare.py#L14-L73), [real-document identity](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/real-documents.json#L1-L82), [correctness qualifications](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/readme.md#L62-L88).

Quick collection builds once and runs sequentially; archived studies have richer alternating collectors. A preserved stale-binary incident retained baseline-only records after a failed rebuild, motivating immutable artifacts and explicit incomplete-run state. Historical three-round/macOS studies remain evidence, not failures against later Linux/ten-round guidance. [Quick driver](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/run.py#L1-L23), [alternating collector](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/measurements/2026-10-25-footnotes-indexes-and-escape-context/measure.py#L1-L47), [stale build record](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/measurements/2026-10-24-escape-output/exploratory/readme.md#L1-L15).

### Local HTML: useful migration input, not publicly reproducible evidence

The unpublished, uncommitted local HTML tree was inspected separately. Its four packages are `base-html`, `base-html-macros`, `base-html-bake`, and `base-html-benchmark-rendering`. It has no immutable public revision to cite; the source is not copied into these repositories. These observations are explicitly weaker than the pinned organization evidence above.

The local workload covers static/list/escape/http, engine/reference, and plain/escaped/Unicode inputs. It preflights complete output bytes and frame parity for both variants before timing. Timed sinks count/observe bytes without a per-byte hash; the recorded checksum identifies untimed expected output. HTTP is an in-process pull-body lifecycle, not a socket or browser benchmark. First-frame averages are not request percentiles.

Its prototype collector provides strict record/configuration/arithmetic validation, interleaving, provenance and binary hashes, cooperative locking, append-only samples, explicit completion state, and process-group cleanup on timeout/cancellation. Those safeguards and regression cases are extraction candidates. External perf totals are whole-process only; no phase-matched counters or controlled campaign exists. The audit did not execute its tests. A proposed first adoption replaces its Python runner with core Rust while preserving HTML's own oracle and operation boundaries.

## What should be standardized

### Measurement scope before normalization

A metric must identify its **phase, process/thread/cohort, privilege mode, units, availability, and denominator**. Wall, resource, and hardware capture windows often differ. `RUSAGE_SELF` is process-wide, not one thread; a lifecycle CPU delta may legitimately surround a narrower wall interval. Preserve boundaries and sampling resolution rather than call every result simply “CPU per operation.”

An external whole-executable `perf stat` wrapper can count startup, preparation, warmup, measurement, and shutdown. Those totals are **not measured-loop counters** and must not be divided by loop-only iterations. A different case is HTTP hello's script: it starts/warmups the server, then attaches `perf stat -p` for 3.2 seconds around a 3-second wrk run. That is an **attached process interval**, neither a whole-executable envelope nor exact phase matching. Its counter/request normalization therefore needs that boundary caveat. Phase-matched counting needs explicit enable/disable barriers and inheritance/scaling/capability semantics before supporting loop instruction budgets. [HTTP attachment window](https://github.com/base-rs/base-http/blob/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/benchmarks/hello/instructions.sh#L30-L52), [Base measurement guidance](https://github.com/base-rs/base-project/blob/5de408e4cd77a2ff9eb21a9039d27c2094f8939e/context/performance.md#L28-L40).

### Lifecycle, placement, and isolation

One runner should own bounded readiness/output, endpoint transfer, role-specific placement, deadlines, graceful-to-forced termination, and reaping across success/error/cancellation. Keep the experiment lock until all owned children/wrapper descendants are cleaned up. Preserve failed output while rejecting partial successful aggregates. [Profiler-descendant handling](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/echo/results/readiness-profiling/run.py#L21-L98).

Placement/locking currently differs: writes holds an internal lock, timers documents external locking, and echo needs physical-core/topology care. Shared configuration should name server/client/controller roles and a cooperative host exclusion protocol. Logical CPU pinning and an idle-host acknowledgement do not prove an idle SMT sibling or host. Builds, tests, fixture preparation, and diagnostics must not overlap measured collection. [Writes](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/writes/run.py#L56-L184), [timers](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/timers/readme.md#L27-L96).

### Validation, journals, and summaries

Use a versioned **process envelope with ordered sample payloads**, not a single global workload enum. Retain original child output and explicit adapter contracts for one-object JSON, per-document lines, readiness, and final statistics. Validate counts/completion rather than accept the first parseable line and ignore a truncated tail. Shared checks cover schema/arithmetic/availability; project hooks cover semantic equivalence.

Retain raw batches, process identity, interleaving order, rejected/incomplete cohorts, artifact indexes, and explicit in-progress/failed/complete state. Summaries need grouping/pair keys and declared aggregation: median-of-process-medians, median of paired ratios, and ratio of separate medians are not interchangeable. Median per-run p99 is not pooled-request p99. [Markdown summary](https://github.com/base-rs/base-markdown/blob/e968682e8d70da184f69ae693de2d4c3811b0407/benches/comparison/measurements/2026-10-25-normalize-identifiers/summarize.py#L1-L28), [timer ratios](https://github.com/base-rs/base-scheduler/blob/cd51eeeee29c0658722868355b1bc04f7e2679ec/benchmarks/timers/summarize.py#L8-L17).

Bind frozen executables to source revision or explicit source/patch digest, lockfile/resolved dependencies, features, toolchain/profile/flags, build command, harness/corpus identity, and hashes. Record collector environment separately from actual per-build provenance. Hashes detect changes but do not prove a claimed build produced a binary. Keep diagnostic allocation/tracing/profiling builds separate from ordinary timing; feature state alone is not a complete instrumentation declaration. [HTTP provenance](https://github.com/base-rs/base-http/blob/90d6b1f6cc0e99094b2f3c5d5548b3e7366e64b5/benchmarks/client/run.py#L115-L177).

## Why split the packages

One feature-gated package was a plausible smaller initial release surface. The selected two-package design makes the Bake boundary explicit at package level and avoids later extraction/import-path churn. Cargo features are additive: `default-features=false` cannot disable a task feature another dependency enables, and separate target directories do not prevent feature unification. A separate core has no Bake dependency edge even when adapter features are enabled. This does not mean an entire application workspace using the adapter contains no Bake dependencies.

The trade-off is coordinating two package versions/releases and schema compatibility. It must **not** produce two implementations of scheduling, supervision, statistics, or validation. Detailed APIs/features/version policy remain open; no cross-repository dependency is added to the empty scaffolds merely to pretend integration exists. See [core plan](../context/architecture.md), [core requirements](../design/architecture.md), and [Bake findings](https://github.com/base-rs/bake-benchmark/blob/main/reports/bake-integration.md).

The adapter should reuse ordinary Bake registration and configuration. Discover explicit unpublished workspace targets and configured standalone manifests; do not reuse release-filtered `bake_cargo::packages` or recursively execute historical snapshots. Shared Cargo preparation belongs in Bake-independent core facilities if both CLI and Bake require it. [Task API boundaries](https://github.com/base-rs/bake/blob/8d4c0739694c0c305922fedc7f6ebefbfffd38fb/context/task-libraries.md#L48-L67), [publication filtering](https://github.com/base-rs/bake-cargo/blob/8687e0392a7425bf2b6dfe2f5fb6f38ae58f12fc/src/cargo.rs#L77-L123).

## Adoption and limits

### Standardized investigation records, including negative results

The user requested retaining measured/rejected attempts like security audit records. The [shared investigation convention](../context/investigations.md) and [template](../templates/investigation.md) provide a stable format for hypotheses, exact variants, methods, raw evidence, validity, decisions, rejection rationale, and revisit conditions. Rejected, unsafe, inconclusive, and invalid attempts remain discoverable; missing measurements stay explicit. This makes the existing shared skill's record-every-attempt policy concrete without claiming an automatic recorder or new benchmark results.

### Follow-up: history and comparison tools

The user's later request is to accumulate comparable same-host or meaningfully normalized data over time and eventually graph it through a small analysis tool or web server. This is lower priority than the core. The [history plan](../context/history.md) preserves raw records plus workload/revision/build/harness/metric/baseline/host identities, treats environment changes as comparison boundaries, and distinguishes paired ratios from unsupported universal cross-host normalization. Storage, indexing, UI packaging, and access control remain open; no server is implemented.

### Additional consumer feedback received during scaffolding

The scheduler optimization agent supplied an [external review](external-review.md), with a [point-by-point response](external-review-response.md). Its numerical experiments are not independently verified additions to this pinned-source audit. The plan now explicitly covers multi-tree/downstream preparation, default baseline-copy rebuild controls, randomized same-round comparators, screening distinct from recorded runs, predeclared expected-error outcomes, host-local lock waiting/progress, and per-role resource/placement records. These are proposed capabilities, not implemented features. In particular, baseline-self repetition cannot substitute for a rebuild/layout control, and consistent paired wins alone do not establish causality or acceptance.

1. Implement strict records/resources and small deterministic tests, then port local HTML collection safeguards into Rust without a Python prerequisite. Preserve its oracle/body lifecycle and retain old scripts as migration evidence.
2. Validate the same core against scheduler echo's multiprocess shape, then HTTP full-cohort/final-statistics semantics and Markdown nested batches/corpora. Do not force those projects into one workload trait.
3. Add thin `benchmark:list`, `benchmark:build`, `benchmark:smoke`, `benchmark:run`, and `benchmark:summarize` Bake tasks. Prove equivalent CLI/Bake semantics and private-package discovery before composing the provider through `base-project`. **These task names are proposals, not registered tasks.**

The existing [benchmark](https://github.com/base-rs/base-project/blob/5de408e4cd77a2ff9eb21a9039d27c2094f8939e/context/benchmarks.md#L13-L37) and [performance](https://github.com/base-rs/base-project/blob/5de408e4cd77a2ff9eb21a9039d27c2094f8939e/context/performance.md#L13-L65) skills remain methodology authorities. Their frozen-build, Linux/physical-core, interleaving/noise-floor, raw-evidence, and diagnostic-separation guidance is policy, not proof that every historical driver enforced it. No universal numerical regression threshold has been adopted here.

Defer universal heap/peak-memory profiling, portable PMU promises, automatic significance/acceptance, automatic source checkout/package installation, and blanket migration. Optional external diagnostics such as perf or non-Rust comparators are distinct from a Python orchestration dependency. Historical scripts/results remain evidence; new controlled claims require comparable frozen builds and honestly scoped metrics. Passing scaffold checks does not establish any proposed runtime capability.
