# Multi-process Roles

**Status.** The maintainer agreed these requirements on 8 November 2026. None is implemented yet. Each takes effect with the milestone that delivers it (see [Milestones](#milestones)); until then the single-process runner is unchanged, and changes to it should not make these requirements harder to meet. Questions the proposals raised that were not decided are listed under [Open questions](#open-questions) and are not requirements.

## Requirement

### 1. Roles

A scenario may declare **roles**, each one supervised process, in its own process group, for every sample. A role has a kind:

- **Service**, such as a server: started, waited on until it is ready, kept running while the driver runs, then stopped.
- **Driver**, such as a load client: run to completion. A scenario with roles has exactly one driver. It owns the measured window (requirement 3), and its record supplies the sample's operation count, workload name and signature.

A role runs one of:

- **the subject**: the artifact × variant being compared, as a single-process scenario runs it today;
- **a fixed workspace binary**, such as a load client or a competitor server built in the workspace (requirement 4);
- **an external program**, such as nginx, identified as requirement 5 describes.

Exactly one role runs the subject, and the other roles are held constant across subjects. Scenarios may give each role its own arguments, the same for every subject. A scenario without roles is a single-process workload, as today, and existing scenarios, records and summaries keep their meaning.

### 2. Session lifecycle

Each sample is one session: verify every role's executable, start the services in declared order and wait for each to be ready before starting the next, run the driver, then stop the services in reverse order and reap every role.

- **Readiness** is a line the service prints on stdout, within a declared timeout. Values the line carries, such as the address the server bound, are passed to later roles' arguments, so endpoints are transferred rather than fixed in advance.
- **Stopping** is either a declared signal followed by a grace period after which the group is killed, or waiting for a service that exits by itself once the driver is done.
- **Launch controls** apply to every role as they do to a single-process workload today: equal-length staged paths for workspace binaries, a cleared and recorded environment, the workspace root as working directory, and stack padding.
- **Supervision**: one deadline covers the whole session. A deadline or cancellation kills every role's process group, in reverse start order, before the host lock is released. The host lock covers the whole session.
- **Journal**: every role's command, CPU set, executable identity, exit status and stderr tail are journalled with the sample, also when it fails.

### 3. The measured window

The driver marks the start and end of its measured window, because only it knows when its counted operations begin and end: after its connections are established and its warmup is done. When the runner receives the start marker it snapshots every service's CPU usage, and the driver's first counted operation happens only after that snapshot. When it receives the stop marker it snapshots the services again without delaying the driver. Window metrics of services are taken only between these markers; the runner never approximates the window by timing it itself, as wrapping a service in `perf stat` for a fixed duration does.

A driver that runs outside the runner, such as by hand, still works and times its own window. Under the runner, a driver that sends no markers, or malformed or unbalanced ones, has failed (requirement 7).

Metrics state which role produced or was measured on them. Service metrics over the window have a phase of their own, distinct from `measured`, `attached` and `process`, meaning "measured in another process between the driver's markers"; requirement 9 defines the only normalization applied to them.

### 4. Subjects, helper roles and competitors

- **Variants** change only the subject. The roles that do not run the subject, such as the client in an HTTP server benchmark, are held constant across every subject in the campaign.
- **Helper roles** that run a workspace binary, such as the load client or a competitor server built in the workspace, are built once, from the baseline tree, by default, and every subject uses the same executable. They are not salted for layout replicas, and their provenance is recorded as a fixed artifact. Building a helper role from every tree may later be added as an explicit, declared option; it is not the default.
- **Competitors**, such as Hyper or nginx in an HTTP server benchmark, run in place of the subject in its role. Every competitor runs in every round, in the round's seeded order, and is paired with every build of ours: each prepared tree and each layout replica. Competitor pairs are labelled as such and are distinct from build, variant and replica pairs; they measure the gap to a reference, not the effect of a change.

### 5. External programs

An external program is recorded by its resolved path, the SHA-256 of its executable and its reported version. Its hash is verified before every sample, as an artifact's is. Host-specific paths are given on the command line, not in package metadata. Recorded mode requires every external program to be identified and unchanged throughout the campaign.

### 6. CPU placement

Each role has its own CPU set. Recorded mode requires a CPU set for every role, as it requires explicit placement today.

Two roles share a physical core when their CPU sets include the same logical CPU or SMT siblings of the same core, according to the measuring host's topology. Sharing that the scenario did not declare is accidental: an error in recorded mode and a warning in screening. A scenario may deliberately declare shared placement, for example to model contention on a cloud host; it is then allowed in both modes, and reports and summaries label the campaign's placement as deliberately shared.

The default layout the usage guide will recommend for a small host such as hana (4 cores, 8 threads) is the client on one physical core and the server on two or three, with SMT siblings excluded. This is guidance for choosing CPU sets, not a check.

### 7. Failures, request errors and invalid responses

- **Process failures stop the run with a clear error**, as a failed sample does today: a service that never becomes ready, exits before it is stopped, or exits unsuccessfully when stopped; a driver that exits with an error, writes an invalid record, or sends invalid markers; and the session deadline passing. The error names the role and what it did, and the campaign keeps its journal and produces no summary.
- **Request errors are data.** Errors the driver observes, such as timeouts and connection resets, are counted by kind and reported with the successful requests. Throughput counts only successful requests, and the driver's operation count is its successful operations.
- **Invalid responses make the comparison invalid.** A response that fails the driver's validation is counted and reported. In screening, the sample's numbers are kept and presented, and the summary states that comparisons involving that subject are invalid rather than reporting them as results. Recorded mode refuses such samples.
- All captured numbers, including error and invalid-response counts, are presented in reports and retained in records.

### 8. Latency summaries

Latency verdicts stay paired per round: each round's latency statistic of a subject is compared with the baseline's in the same round, with the existing resolution, replica and sign-test rules. Summaries also report a merged histogram per subject, pooling its processes' latency histograms, labelled with how observations are weighted (for example, that under closed-loop load a faster process contributes more observations). The merged histogram is descriptive: it gives honest tail percentiles but is not a paired verdict. Histograms merge only when their bucket definitions match; otherwise they are reported separately with a warning.

This revises the earlier rule against pooled latency percentiles only for this labelled merged histogram. Quantile metrics are still summarized per process, as today.

### 9. Service CPU per request

A service's CPU time over the window divided by the driver's operations in the same window is reported as CPU per request, marked approximate. The label says why: the window's edges, failed requests, and work such as draining after the stop marker are not exactly the counted operations. Lifetime, attached and other non-window service totals are never divided by operations.

### 10. Cross-role consistency checks

Checks across roles, such as a server's request count equalling the client's warmup and measured requests, are declared per project. The core provides the means to declare them and evaluates them for every sample; it contains no protocol-specific checks of its own. A service may print a final record, which the runner validates and retains under the role, so that such checks have the service's counts to compare.

### 11. Load generation outside the core

The core contains no load generator and no protocol logic. HTTP scenarios use [base-benchmark-load](https://github.com/base-rs/base-benchmark-load), a Rust load client in its own repository, as their driver, not wrk. Its records report HDR-style latency histograms, errors by kind, invalid responses, and client saturation; base-benchmark-load's own requirements are kept in its repository. The core treats these as ordinary record contents and summarizes them as requirements 7 and 8 describe.

### 12. A later client host

A driver on a separate client host, planned over a 10 GbE link, comes after HTTP hello, echo and competitors work on one host, and so does profiling. Meanwhile, nothing in the role interface may assume all roles share a host: endpoints reach the driver as values captured from readiness rather than fixed loopback addresses, window markers are messages rather than shared files or memory, and records identify the host of each role.

## Milestones

1. **M1: HTTP hello on one host.** One service and one driver, ready-line readiness, per-role CPU sets, external program provenance, and window markers with service CPU snapshots.
2. **M2: echo and competitors.** Scheduler echo on roles, and competitor servers in every round.
3. **M3: counters, histograms and fixed rate.** Hardware counters over the window, latency histograms with merged summaries, and integration of base-benchmark-load's fixed-rate mode.
4. **M4: profiling and the client host.**

## Rationale

The maintainer decided these requirements on 8 November 2026 from a proposal for multi-process roles and a proposal for a Base HTTP load generator. The benchmarks they replace are Python drivers in base-scheduler (`benchmarks/echo/run.py` and the quick screens) and base-http (`hello/run.py`, `client/run.py`, the fixed-rate client's `fixed_rate.py`), which each reimplement readiness, placement, the measured window, shutdown and server CPU accounting in different ways. The [source audit](../reports/benchmarking-landscape.md) names the common lifecycle: readiness, endpoint transfer, role placement, deadlines, graceful then forced termination, and final statistics. The [architecture requirement](architecture.md) asks for this machinery in Rust, without Python, while workloads and protocol semantics stay project-owned.

- **Driver-owned window.** The quick screens divided a server's lifetime CPU by the client's window requests, and timed counters with `perf stat` around a fixed sleep; both mismatch scopes. Only the driver knows when counted work begins, so it marks the window and the runner snapshots services there.
- **Fixed helpers and variants on the subject only.** Holding the client constant keeps one instrument for every subject, as the quick screens did with "one fixed client for every sample"; a client that changed with the tree would move with the server and confound the comparison.
- **Competitors every round.** A reference such as Hyper or nginx measured in the same rounds as every build shows the gap to it under the same host conditions, which separate campaigns cannot.
- **Paired verdicts and merged histograms.** Paired per-round comparisons remain the evidence for change; a per-process median of p99s hides the tail, so the merged histogram reports it, labelled so that its weighting is not mistaken for a verdict.
- **Approximate CPU per request.** CPU per request is the figure server investigations rely on; dividing window totals by window operations is the closest honest scope, and the label keeps it from being read as exact.
- **Failures and errors.** A crashed or unready process means the sample measured something other than the scenario, so the run stops. Request errors under load are part of the result, and dropping them would overstate throughput. A server that answers incorrectly is not comparable at all.
- **Placement.** Roles sharing a physical core contend for it, which on a 4-core host can dominate the result; it is an error in recorded evidence unless the scenario intends it, as when modelling a contended cloud host.
- **Our own load client.** wrk is unmaintained, closed-loop only, shares one contended histogram, censors its tail at stop and validates nothing; the fixed-rate client already measures open-loop latency correctly. A load client inside the core would put protocol semantics there, which the architecture requirement forbids.
- **Client host later.** One host is enough to replace the Python drivers; a second host adds clock skew at the window edges and a second lock, so it waits until the single-host design is validated, without being precluded.

## Scope

Scenario declarations in `[package.metadata.base-benchmark]`, preparation of helper roles and competitors, the runner's session lifecycle and placement, external program provenance, record contents for roles and window metrics, recorded-mode prerequisites, and summaries of multi-process campaigns. The architecture requirement continues to govern the package boundary, records and evidence classes. base-benchmark-load's behaviour is governed by its own repository.

## Validation

Review each milestone's change against the requirements it delivers, with tests to the coverage requirement:

- **Roles and lifecycle (1, 2):** a fixture workspace with a service and a driver; tests that services start in order, wait for readiness, receive captured values, stop by signal and grace or by exit, run under the launch controls, and are killed with their groups on deadline and cancellation before the lock is released; inspect journals of successful and failed samples for every role's command, CPUs, identity, status and stderr.
- **Window (3):** a fixture service that burns CPU only before the start marker and after the stop marker shows no window CPU; a driver that never sends markers, sends them twice, or sends stop before start fails the sample; inspect the runner for any timed approximation of the window.
- **Subjects and competitors (4):** inspect prepared manifests for helper roles built once from the baseline tree, unsalted; check that variants alter only the subject's arguments; check that every competitor appears in every round and is paired with every build and replica, labelled as a competitor pair.
- **External programs (5):** a program changed between samples fails the campaign; recorded mode refuses an unidentified program; manifests contain no host paths.
- **Placement (6):** with an injected CPU topology, undeclared sharing of a logical CPU or SMT siblings fails in recorded mode and warns in screening; declared sharing passes and is labelled in reports and summaries.
- **Failures (7):** tests for each process failure stopping the run with an error naming the role; request errors reported by kind with throughput counting only successes; invalid responses making comparisons invalid in screening and refused in recorded mode.
- **Latency (8):** merging fixture histograms reproduces the pooled counts exactly; verdicts are unchanged by the merged histogram; mismatched bucket definitions are not merged; the weighting label is present.
- **CPU per request (9):** the reported value equals window service CPU over driver operations, is marked approximate, and no lifetime or attached total is divided.
- **Cross-role checks (10):** a declared check that fails is reported for the sample; the core contains no protocol-specific check.
- **Load generation (11):** inspect `cargo tree -p base-benchmark -e normal` and the source for a load generator or HTTP parsing.
- **Client host (12):** inspect the role interface for loopback addresses, shared files or shared memory between roles, and records for per-role host identity.

Before a milestone's Python driver is retired, reproduce its results on the same host within their noise, as the [architecture requirement](architecture.md) asks for multi-process roles to be validated against scheduler echo and HTTP hello before they are claimed.

## Open questions

These were raised by the proposals and not decided; they are not requirements.

1. **Readiness without a ready line.** External servers such as nginx print no address. Is a TCP connect probe on a runner-allocated port acceptable, given that allocating a port before the server binds it is racy?
2. **Histogram format and home.** Should the record carry a sparse histogram format defined by a histogram type in base-benchmark's `default-features = false` core, so producer and summarizer agree on buckets, or should drivers use `hdrhistogram`? Either needs a record version.
3. **Client saturation.** Should a client-saturated sample only be reported and warned about, or refused in recorded mode, and who owns the threshold?
4. **Competitor pairing and repeats.** Is each competitor paired with every variant of every build, or only with each build's baseline variant? Is a same-binary repeat subject wanted as a noise floor alongside competitors?
5. **Competitor configuration.** How are an external program's configuration and neighbouring files, such as an nginx configuration, identified and verified, and must external programs be staged under equal-length paths?
6. **Cross-role checks.** Are checks declared as equalities over role metrics in package metadata, or as a project-owned verification step, and does a failed check fail the sample, as a process failure does, or invalidate comparisons, as an invalid response does?
7. **Service lifetime usage and shutdown.** Should services also report lifetime `wait4` usage, unnormalized, alongside window metrics, as echo's `server_cpu_seconds` does today? Does a service that has to be killed after its stop grace period fail the sample, or is that only journalled?
8. **The collector's placement.** Should the runner itself be pinned to a declared CPU, as `fixed_rate.py` pins its controller?
9. **Rate sweeps.** Should a sweep for the highest rate within a latency limit live in the driver, emitting a sample per step, or should base-benchmark plan rate steps as scenarios?
10. **Shared fixtures and cooldowns.** How are fixtures shared between roles, such as the HTTP client benchmark's TLS identity, and pauses between samples declared?
11. **Downstream builds.** When do workloads in another repository, built against each tree through `[patch]`, arrive, so that scheduler trees can be screened with HTTP hello?
12. **wrk for archived results.** Should wrk be kept only to reproduce archived results, and is base-benchmark-load cross-validated against it before the Python drivers are retired?
