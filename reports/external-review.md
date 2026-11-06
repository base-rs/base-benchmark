# External Review

> Publication note: this is consumer feedback received during repository scaffolding on 6 November 2026, preserved as review evidence rather than an independently verified benchmark report. Finding 1 described an earlier moment: the [audit report](benchmarking-landscape.md) now exists. Numerical observations and host-local/uncommitted artifacts below have not been independently reproduced or promoted into accepted results. See the [response and plan changes](external-review-response.md).

A review of the planning documents in `base-benchmark` (`readme.md`, `design/architecture.md`, `context/architecture.md`) and `bake-benchmark` (`readme.md`, `design/integration.md`, `context/integration.md`, `reports/bake-integration.md`), as they stood on 6 November 2026. The reviewer is a consumer: the agent that ran base-scheduler's optimization work this week with an ad-hoc paired-comparison tool (`quick.py`, described below). The question is whether the planned tooling would replace that tool and the per-result Python summarizers.

**Verdict:** the direction fits. With findings 2–4 addressed, the planned tooling would replace `quick.py` and the ad-hoc summarizers. As written, it covers recorded runs well and quick paired screening poorly, and screening is what optimization work needs most.

## What fits

- One Rust implementation for records, collection, locking, validation and summaries, with a thin Bake adapter and no Python on normal paths.
- Workloads, correctness oracles and budgets stay with each project; no universal workload trait.
- Preparation is separate from collection. Diagnostic and timing builds are distinct artifacts.
- Paired per-round ratios with medians and ranges, instead of an automatic significance test.
- Whole-process counters are not divided by loop counts; scopes and denominators are explicit.
- One host lock covers every role of an experiment (server, client, controls).

## Findings

### 1. The main evidence is missing

Every document cites `base-benchmark/reports/benchmarking-landscape.md` as its rationale. The file does not exist in the local checkout, and the GitHub repository is empty. Commit it, or remove the references.

### 2. Build-layout noise is not addressed

This was the most consequential finding of the base-scheduler work. Identical source built at a different path performs differently, because Cargo hashes the package path into each crate's metadata, which moves code and data addresses. On hana (Xeon E3-1240 v6), with interleaved rounds:

- The base-http hello server: a copy of `v0.4.0` at another path "regressed" by 2–3.5% in throughput, slower in 0 of 5 rounds in several cells. That was the same size as a regression we had reported between `v0.4.0` and `main`, which `perf stat` then showed to be identical code: same instructions and cycles per request. The `nm` addresses of `run_loop`'s closure differed between the two builds.
- The periodic timer diagnostic: identical source at another path moved p50 lateness by up to +12.6% and, in one run, +19.4%.
- Echo: a copy moved throughput and CPU per request by up to about 2–3%.

Evidence: `base-scheduler/benchmarks/timers/results/timer-batching/readme.md` (`layout.jsonl`, the `quick/` records) and `base-scheduler/benchmarks/echo/results/driver-wakes/readme.md` (`quick-4w-*.jsonl`, layout columns). The HTTP investigation that found the effect is not committed to any repository. Its result files are on `hana.local` under `~/Developer/claude-bench/quick/results/`: `http-iouring-regression-aa-*.jsonl` for the replicate of `v0.4.0`, and `iouring-regression-multi-1.jsonl` for the interleaved bisect including that replicate. Base-scheduler PR #104's description records only the resulting caveat.

"Frozen build identity and artifact hashes" does not cover this. Two builds can be correctly identified and still differ only by layout. The planned tooling should:

- build a **replicate of the baseline from a copy at another path** as a standard control, on by default for comparisons of separately built trees;
- report each candidate's paired change **beside the replicate's change** in the same rounds;
- warn when a candidate's change does not clearly exceed the replicate's.

Without this, the tool will certify layout noise as improvement. "Better in 5 of 5 rounds" is not evidence on its own: a replicate produces it too.

### 3. Comparing source trees is out of scope as written

Optimization work compares several **source trees** of the same project, all built identically: a baseline, one or more candidates and the layout replica. Often a downstream workload has to be built against each tree. The base-http hello benchmark was built against each base-scheduler tree through a generated `[patch.crates-io]` config, `cargo build --config patch.toml`, with a separate target directory per tree.

The plan excludes automatic revision checkout (reasonable) and keeps building out of collection (correct). But no component is responsible for building N prepared trees with the same flags into separate, identified artifacts. `benchmark:build` covers one workspace's own packages. Please make multi-tree preparation explicit, in the core if a standalone CLI needs it:

- inputs: a set of labelled source trees, either directories or archives of revisions, each an explicit path; checkout stays out of scope;
- the same toolchain, features, profile and lockfile policy for every tree;
- an optional downstream workload built against each tree through patch overrides;
- one identified artifact set per tree, with the layout replica generated automatically.

### 4. Controls and expected failures

- **Controls in the same rounds.** Unchanged comparators (raw Tokio for echo, Hyper for HTTP) must run in the same randomized rounds as the variants. They show the run-to-run noise floor; the replicate shows the build-layout floor. Both belong in the summary.
- **Errors as data.** In the HTTP comparison at 256 connections, the Hyper control had 188–357 wrk timeouts in every round, a real property of that control on one pinned CPU. The base-http runner aborted the whole experiment on the first one. Validation should be able to record declared, expected failure conditions as measured data, such as socket errors and timeouts per sample, while still rejecting malformed output or failed correctness checks. "Protocol completion" in the plan reads as abort-on-any-error.

### 5. Two modes: screening and recorded runs

The plan's evidence model (frozen provenance, journals, completion states, regenerable summaries) suits recorded results. Optimization also needs a **screening mode**:

- interleaved and randomized, with the layout replica and controls;
- 5–10 minutes per comparison, including builds;
- paired summaries with rounds-improved counts printed at the end;
- results kept, but explicitly not acceptance evidence.

The base-project benchmarks skill sets a 10-round minimum for recorded results; screening ran 5 rounds (9 for the noisy timer diagnostic). The tooling should support both modes explicitly, rather than make every screen a full recorded run or leave screening to ad-hoc scripts.

### 6. Smaller points

- **A separate benchmark host.** The work ran on a dedicated host reached over SSH. Source trees were synced to it, and the lock lived on it. The CLI must run on that host, and nothing should assume the developer's machine is the measuring host.
- **Per-role CPU placement.** Server workers were pinned to CPUs 0..N−1, clients to their SMT siblings or other cores, and a single-worker HTTP server to CPU 0, with wrk elsewhere. The plan mentions role-specific placement in passing; it needs to be a first-class part of the experiment plan and recorded per sample.
- **Waiting is normal.** Five agents queued on one host lock. A visible "waiting for lock / running round k of n" status, and a non-polling way to wait, would have avoided agents ending their turns mid-run or polling.
- **Per-process resource accounting.** `wait4` rusage for the server and `/proc` deltas over the measured window were both used. Their scopes differ, since rusage covers the whole process life including warmup, and records should say which applies.

## The tool this would replace

`quick.py` is not in a repository. It is at `~/Developer/claude-bench/quick/quick.py` on `hana.local`, with a local copy at `base-rs/base-scheduler-wt/quick.py`. It is one file of about 280 lines:

- **Modes:** `echo`, `http` (base-http hello against each tree through `[patch.crates-io]`) and `timers` (the periodic diagnostic).
- **Builds:** it builds each tree into its own target directory under the host lock.
- **Runs:** it interleaves the trees in random order per round, adding the unchanged control and the layout replica.
- **Output:** JSON lines, plus a Markdown table of medians and per-round paired changes with rounds-improved counts for the candidate and the replica.

It is a reasonable reference for the screening mode's minimum feature set, and for the failure cases it mishandled:

- relative tree paths broke the target directory;
- a wrk timeout from a control aborted the run until it recorded timeouts instead;
- before the replica existed, it reported layout noise as consistent change.
