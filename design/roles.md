# Multi-process Roles

**Status.** The maintainer agreed these requirements on 8 November 2026. M1 is being implemented: roles (requirement 1), the session lifecycle (2), the measured window (3), variants and helper roles (the M1 part of 4), external programs (5), CPU placement with the physical-core sharing checks (6), the process failures of requirement 7, and service CPU per request (9) are implemented; request errors and invalid responses are not yet. Requirement 11's boundary already holds. Each requirement takes effect with the milestone that [Milestones](#milestones) assigns it; until then the single-process runner is unchanged. Questions the proposals raised that were not decided are listed under [Open questions](#open-questions) and are not requirements; where M1 had to choose before one was decided, the open question says what M1 does.

## Requirement

### 1. Roles

A scenario may declare **roles**, each one supervised process, in its own process group, for every sample. A role has a kind:

- **Service**, such as a server: started, waited on until it is ready, kept running while the driver runs, then stopped.
- **Driver**, such as a load client: run to completion. A scenario with roles has exactly one driver. It owns the measured window (requirement 3), and its record supplies the sample's operation count (requirement 7). Which record supplies the sample's workload name and signature is open question 15.

A role runs one of:

- **the subject**: the artifact × variant being compared, as a single-process scenario runs it today;
- **a fixed workspace binary**, such as a load client or a competitor server built in the workspace (requirement 4);
- **an external program**, such as nginx, identified as requirement 5 describes.

Exactly one role runs the subject, and the other roles are held constant across subjects. Scenarios may give each role its own arguments, the same for every subject. A scenario without roles is a single-process workload, as today, and existing scenarios, records and summaries keep their meaning.

### 2. Session lifecycle

Each sample is one session: verify every role's executable, start the services in declared order and wait for each to be ready before starting the next, run the driver, then stop the services in reverse order and reap every role.

- **Readiness** is, for now, a line the service prints on stdout, within a declared timeout. Other forms, such as a TCP connect probe for servers like nginx that print no address, remain open question 1. Values the line carries, such as the address the server bound, are passed to later roles' arguments, so endpoints are transferred rather than fixed in advance. A ready pattern is literal text with named captures, each non-empty and separated from the next by text; earlier captures take precedence, each taking the longest text that lets the rest of the line match, so `{host}:{port}` splits `[::1]:8080` at its last colon. Matching takes time linear in the line's length, so a long or adversarial line cannot stall the session. In role arguments, `{ROLE.NAME}` names a captured value, `{{` and `}}` are literal braces, as in ready patterns, and any other brace, including a reference to a value no earlier service captures, is an error when the scenario is planned.
- **Stopping** is either a declared signal followed by a grace period after which the group is killed, or waiting for a service that exits by itself once the driver is done.
- **Launch controls** apply to every role as they do to a single-process workload today: equal-length staged paths for workspace binaries, a cleared and recorded environment, the workspace root as working directory, and stack padding.
- **Supervision**: one deadline covers the whole session. A deadline or cancellation kills every role's process group, in reverse start order, before the host lock is released. The host lock covers the whole session.
- **Journal**: every role's command, CPU set, executable identity, exit status and stderr tail are journalled with the sample, also when it fails.

### 3. The measured window

The driver marks the start and end of its measured window, because only it knows when its counted operations begin and end: after its connections are established and its warmup is done. When the runner receives the start marker it snapshots every service's CPU usage, and when it receives the stop marker it snapshots the services again without delaying the driver. Window metrics of services are taken only between these markers; the runner never approximates the window by timing it itself, as wrapping a service in `perf stat` for a fixed duration does. Whether the driver waits for the runner to acknowledge the start marker before its first counted operation is open question 13.

Under the runner, a driver that sends no markers, or malformed or unbalanced ones, has failed (requirement 7). Whether a driver run outside the runner, such as by hand, must still work and time its own window is open question 14.

Metrics state which role produced or was measured on them. A service's metrics over the window are distinguishable, in records and summaries, from the driver's measured totals and from process and attached totals, and are normalized only as requirement 9 says. How they are represented, for example as a phase of their own beside `measured`, `attached` and `process`, and whether that needs a new record version, is open question 16.

### 4. Subjects, helper roles and competitors

- **Variants** change only the subject. The roles that do not run the subject, such as the client in an HTTP server benchmark, are held constant across every subject in the campaign.
- **Helper roles** that run a workspace binary, such as the load client or a competitor server built in the workspace, are built once, from the baseline tree, by default, and every subject uses the same executable. They are not salted for layout replicas, and their provenance is recorded as a fixed artifact. Building a helper role from every tree could later be an explicit, declared option; it is not the default.
- **Separately published helpers**, such as base-benchmark-load, a binary crate in its own repository, are obtained by default as an external program (requirement 5) installed at a pinned version, for example with `cargo install --locked base-benchmark-load@VERSION`, and identified by its hash. A project may instead build it as a helper role from a wrapper binary in its workspace that depends on the published crate at a pinned version and calls its library entry point, as base-benchmark-load's `base_benchmark_load::command`. Both are supported.
- **Competitors**, such as Hyper or nginx in an HTTP server benchmark, run in place of the subject in its role. Every competitor runs in every round, in the round's seeded order, and is paired with every build of ours: each prepared tree and each layout replica. Competitor pairs are labelled as such and are distinct from build, variant and replica pairs; they measure the gap to a reference, not the effect of a change.

### 5. External programs

An external program is given by its path on the command line, never in package metadata, and the campaign records its resolved path, the SHA-256 of its executable and its reported version. Its hash is verified before every sample, as an artifact's is. The path names a file on the measuring host: a remote run passes it to the host unchanged, as it does `--lock`, and does not map it to the workspace mirror as it does path arguments. Recorded mode requires every external program to be unchanged throughout the campaign. How a program's version is obtained, and whether recorded mode refuses a program whose version cannot be determined as unidentified, is open question 19.

### 6. CPU placement

Each role has its own CPU set. Recorded mode requires a CPU set for every role, as it requires explicit placement today.

Two roles share a physical core when their CPU sets include the same logical CPU or SMT siblings of the same core, according to the measuring host's topology. Sharing that the scenario did not declare is accidental: an error in recorded mode and a warning in screening. A scenario may deliberately declare shared placement, for example to model contention on a cloud host; it is then allowed in both modes, and reports and summaries label the campaign's placement as deliberately shared.

### 7. Failures, request errors and invalid responses

- **Process failures stop the run with a clear error**, as a failed sample does today: a service that never becomes ready, exits before it is stopped (for a service that stops by exiting, before the driver has finished), or does not stop successfully, other than by being killed after its grace period; a driver that exits with an error, writes an invalid record, or sends invalid markers; and the session deadline passing. A service stops successfully when it exits with status 0 or dies from its declared stop signal; whether a service killed after its grace period fails the sample is open question 7. The error names the role and what it did, and the campaign keeps its journal and produces no summary.
- **Request errors are data.** Errors the driver observes, such as timeouts and connection resets, are counted by kind and reported with the successful requests. Throughput counts only successful requests, so the driver's operation count is its successful operations only. In a scenario with roles, this changes the meaning of every per-operation metric, including the driver's own: each is per successful operation.
- **Invalid responses make the comparison invalid.** A response that fails the driver's validation is counted and reported. In screening, the sample's numbers are kept and presented, and the summary states that comparisons involving that subject are invalid rather than reporting them as results. In recorded mode, an invalid response fails the campaign; dropping the sample instead would pick which rounds count.
- All captured numbers, including error and invalid-response counts, are presented in reports and retained in records.

### 8. Latency summaries

Latency verdicts stay paired per round: each round's latency statistic of a subject is compared with the baseline's in the same round, with the existing resolution, replica and sign-test rules. Summaries also report a merged histogram per subject, pooling its processes' latency histograms, labelled with how observations are weighted (for example, that under closed-loop load a faster process contributes more observations). The merged histogram is descriptive: it gives honest tail percentiles but is not a paired verdict. Histograms merge only when their bucket definitions match: histograms with different bucket definitions are not merged and are named in a warning, following the existing rule that metrics redefined between processes are excluded rather than merged ([architecture](../context/architecture.md#interpretation-safeguards)).

This revises the earlier rule against pooled latency percentiles only for this labelled merged histogram. Quantile metrics are still summarized per process, as today. Whether a subject with one process whose bucket definition differs still gets a merged histogram of its matching processes is open question 2.

### 9. Service CPU per request

A service's CPU time over the window divided by the driver's operations in the same window is reported as CPU per request, marked approximate. The label says why: the window's edges, failed requests, and work such as draining after the stop marker are not exactly the counted operations. Lifetime, attached and other non-window service totals are never divided by operations. It is the only exception to the rule, tested under the [architecture requirement](architecture.md), that only measured-phase totals are normalized by operations.

### 10. Cross-role consistency checks

Checks across roles, such as a server's request count equalling the client's warmup and measured requests, are declared per project. The core provides a way to declare them, and contains no protocol-specific checks of its own; how they are evaluated, and what a failed check does, is open question 6. A service may print a final record, which the runner validates and retains under the role, so that such checks have the service's counts to compare.

### 11. Load generation outside the core

The core contains no load generator and no protocol logic. HTTP scenarios use [base-benchmark-load](https://github.com/base-rs/base-benchmark-load), a Rust load client in its own repository, as their driver, not wrk. Its records report HDR-style latency histograms, errors by kind, invalid responses, and client saturation; base-benchmark-load's own requirements are kept in its repository, and a workspace obtains it as requirement 4 describes for separately published helpers. The core treats these as ordinary record contents and summarizes them as requirements 7 and 8 describe.

### 12. A later client host

A driver on a separate client host, planned over a 10 GbE link, comes after HTTP hello, echo and competitors work on one host, and so does profiling. Meanwhile, nothing in the role interface may assume all roles share a host: endpoints reach the driver as values captured from readiness rather than fixed loopback addresses, window markers are messages rather than shared files or memory, and records identify the host of each role.

## Milestones

1. **M1: HTTP hello on one host.** One service and one driver, ready-line readiness, per-role CPU sets, external program provenance, and window markers with service CPU snapshots.
2. **M2: echo and competitors.** Scheduler echo on roles, and competitor servers in every round.
3. **M3: counters, histograms and fixed rate.** Hardware counters over the window, latency histograms with merged summaries, and integration of base-benchmark-load's fixed-rate mode.
4. **M4: profiling and the client host.**

Each requirement, or each part where it is split, takes effect with this milestone:

| Requirement | Milestone |
| --- | --- |
| 1. Roles | M1. Scenarios without roles keep their meaning in every milestone. |
| 2. Session lifecycle | M1. |
| 3. The measured window | M1. |
| 4. Subjects, helper roles and competitors | Variants and helper roles in M1; competitors in M2. |
| 5. External programs | M1. External competitors such as nginx run once competitors arrive in M2, and their readiness is open question 1. |
| 6. CPU placement | M1, including the physical-core sharing checks. |
| 7. Failures, request errors and invalid responses | M1. |
| 8. Latency summaries | M3, with latency histograms. |
| 9. Service CPU per request | M1, with the window's service CPU snapshots. |
| 10. Cross-role consistency checks | M2, with scheduler echo, whose server request count is the example check. |
| 11. Load generation outside the core | The boundary, no load generator or protocol logic in the core, already holds; HTTP scenarios use base-benchmark-load from M1. |
| 12. A later client host | M1 for the role interface's independence from one host; the client host itself in M4. |

## Rationale

The maintainer decided these requirements on 8 November 2026 from a proposal for multi-process roles and a proposal for a Base HTTP load generator, and on 9 November 2026 how separately published helpers are obtained, which had been an open question. The benchmarks they replace are Python drivers in base-scheduler (`benchmarks/echo/run.py` and the quick screens) and base-http (`hello/run.py`, `client/run.py`, the fixed-rate client's `fixed_rate.py`), which each reimplement readiness, placement, the measured window, shutdown and server CPU accounting in different ways. The [source audit](../reports/benchmarking-landscape.md) names the common lifecycle: readiness, endpoint transfer, role placement, deadlines, graceful then forced termination, and final statistics. The [architecture requirement](architecture.md) asks for this machinery in Rust, without Python, while workloads and protocol semantics stay project-owned.

- **Driver-owned window.** The quick screens divided a server's lifetime CPU by the client's window requests, and timed counters with `perf stat` around a fixed sleep; both mismatch scopes. Only the driver knows when counted work begins, so it marks the window and the runner snapshots services there.
- **Fixed helpers and variants on the subject only.** Holding the client constant keeps one instrument for every subject, as the quick screens did with "one fixed client for every sample"; a client that changed with the tree would move with the server and confound the comparison.
- **Competitors every round.** A reference such as Hyper or nginx measured in the same rounds as every build shows the gap to it under the same host conditions, which separate campaigns cannot.
- **Paired verdicts and merged histograms.** Paired per-round comparisons remain the evidence for change; a per-process median of p99s hides the tail, so the merged histogram reports it, labelled so that its weighting is not mistaken for a verdict.
- **Approximate CPU per request.** CPU per request is the figure server investigations rely on; dividing window totals by window operations is the closest honest scope, and the label keeps it from being read as exact.
- **Failures and errors.** A crashed or unready process means the sample measured something other than the scenario, so the run stops. Request errors under load are part of the result, so they are reported by kind; counting them as successes would overstate throughput. A server that answers incorrectly is not comparable at all.
- **Placement.** Roles sharing a physical core contend for it, which on a 4-core host can dominate the result; it is an error in recorded evidence unless the scenario intends it, as when modeling a contended cloud host. For a small host such as hana (4 cores, 8 threads), a reasonable placement is the client on one physical core and the server on two or three, with SMT siblings excluded; that is guidance for choosing CPU sets, not a check.
- **Our own load client.** wrk is unmaintained, closed-loop only, shares one contended histogram, censors its tail at stop and validates nothing; the fixed-rate client already measures open-loop latency correctly. A load client inside the core would put protocol semantics there, which the architecture requirement forbids.
- **Client host later.** One host is enough to replace the Python drivers; a second host adds clock skew at the window edges and a second lock, so it waits until the single-host design is validated, without being precluded.

## Scope

Scenario declarations in `[package.metadata.base-benchmark]`, preparation of helper roles and competitors, the runner's session lifecycle and placement, external program provenance, record contents for roles and window metrics, recorded-mode prerequisites, and summaries of multi-process campaigns. The architecture requirement continues to govern the package boundary, records and evidence classes. base-benchmark-load's behavior is governed by its own repository.

## Validation

Review each milestone's change against the requirements it delivers, with tests to the coverage requirement:

- **Roles and lifecycle (1, 2):** a fixture workspace with a service and a driver; tests that services start in order, wait for readiness, receive captured values, stop by signal and grace or by exit, run under the launch controls, and are killed with their groups on deadline and cancellation before the lock is released; inspect journals of successful and failed samples for every role's command, CPUs, identity, status and stderr.
- **Window (3):** a fixture service that burns CPU before printing its ready line and after receiving its stop signal, and otherwise blocks, under a driver that sends no requests between its markers, shows window CPU no greater than the snapshot's declared resolution, while a service that burns CPU serving the driver's requests between the markers shows that CPU; a driver that never sends markers, sends them twice, or sends stop before start fails the sample; window metrics are distinguishable from the driver's measured totals and from process and attached totals, and are normalized only as requirement 9 says; inspect the runner for any timed approximation of the window.
- **Subjects and competitors (4):** inspect prepared manifests for helper roles built once from the baseline tree, unsalted; check that a separately published helper can run both as an external program and from a workspace wrapper binary; check that variants alter only the subject's arguments; check that every competitor appears in every round and is paired with every build and replica, labelled as a competitor pair.
- **External programs (5):** a program changed between samples fails the campaign; package metadata in Cargo manifests names no host path, and the campaign records each program's resolved path, hash and version; a remote run passes program paths to the host unchanged rather than mapping them to the mirror. Tests of refusing an unidentified program wait for open question 19.
- **Placement (6):** with an injected CPU topology, undeclared sharing of a logical CPU or SMT siblings fails in recorded mode and warns in screening; declared sharing passes and is labelled in reports and summaries.
- **Failures (7):** tests for each process failure stopping the run with an error naming the role, including a service that exits with a nonzero status or dies from a signal other than its declared stop signal when stopped, other than the kill after the grace period (open question 7), while exit status 0 and the declared stop signal both stop it successfully; request errors reported by kind, with throughput, the driver's operation count and every per-operation metric counting only successes; invalid responses making comparisons invalid in screening and failing the campaign in recorded mode.
- **Latency (8):** merging fixture histograms reproduces the pooled counts exactly; verdicts are unchanged by the merged histogram; mismatched bucket definitions are not merged and are named in a warning; the weighting label is present.
- **CPU per request (9):** the reported value equals window service CPU over driver operations, is marked approximate, and no lifetime or attached total is divided.
- **Cross-role checks (10):** a project can declare a check across roles, and a service's final record is validated and retained under its role; the core contains no protocol-specific check. Tests of how a declared check is evaluated wait for open question 6.
- **Load generation (11):** inspect `cargo tree -p base-benchmark -e normal` and the source for a load generator or HTTP parsing.
- **Client host (12):** inspect the role interface for loopback addresses, shared files or shared memory between roles, and records for per-role host identity.

Before a milestone's Python driver is retired, run it and its replacement on the same host and compare their results, as the [architecture requirement](architecture.md) asks for multi-process roles to be validated against scheduler echo and HTTP hello before they are claimed. The criterion for agreement is open question 17.

## Open questions

These were raised by the proposals and not decided; they are not requirements.

1. **Readiness without a ready line.** External servers such as nginx print no address. Is a TCP connect probe on a runner-allocated port acceptable, given that allocating a port before the server binds it is racy?
2. **Histogram format and home.** Should the record carry a sparse histogram format defined by a histogram type in base-benchmark's `default-features = false` core, so producer and summarizer agree on buckets, or should drivers use `hdrhistogram`? Either needs a record version. When one of a subject's processes has a different bucket definition from the others, does the subject still get a merged histogram of the processes whose definitions match, or none?
3. **Client saturation.** Should a client-saturated sample only be reported and warned about, or refused in recorded mode, and who owns the threshold?
4. **Competitor pairing and repeats.** Is each competitor paired with every variant of every build, or only with each build's baseline variant? Is a same-binary repeat subject wanted as a noise floor alongside competitors?
5. **Competitor configuration.** How are an external program's configuration and neighboring files, such as an nginx configuration, identified and verified, and must external programs be staged under equal-length paths? *M1 runs external programs from their resolved paths, unstaged, and identifies only the executable.*
6. **Cross-role checks.** Are checks declared as equalities over role metrics in package metadata, or as a project-owned verification step? Does the core evaluate them for every sample, and does a failed check fail the sample, as a process failure does, or invalidate comparisons, as an invalid response does?
7. **Service lifetime usage and shutdown.** Should services also report lifetime `wait4` usage, unnormalized, alongside window metrics, as echo's `server_cpu_seconds` does today? Does a service that has to be killed after its stop grace period fail the sample, or is that only journalled? *M1 reports no lifetime usage of services, and only journals a kill after the grace period, as `killed_after_grace`; the sample does not fail.*
8. **The collector's placement.** Should the runner itself be pinned to a declared CPU, as `fixed_rate.py` pins its controller?
9. **Rate sweeps.** Should a sweep for the highest rate within a latency limit live in the driver, emitting a sample per step, or should base-benchmark plan rate steps as scenarios?
10. **Shared fixtures and cooldowns.** How are fixtures shared between roles, such as the HTTP client benchmark's TLS identity, and pauses between samples declared?
11. **Downstream builds.** When do workloads in another repository, built against each tree through `[patch]`, arrive, so that scheduler trees can be screened with HTTP hello?
12. **wrk for archived results.** Should wrk be kept only to reproduce archived results, and is base-benchmark-load cross-validated against it before the Python drivers are retired?
13. **Acknowledging the start marker.** Should the driver wait for the runner to acknowledge its start marker, so that the services' snapshot precedes its first counted operation, or is the gap between marker and snapshot left to the approximate window edges that requirement 9's label names? *M1's runner acknowledges every start marker once it has taken its snapshots; `Window::start_acknowledged` waits for that and `Window::start` does not, so either answer remains open to drivers.*
14. **Drivers outside the runner.** Must a driver run by hand, outside the runner, still work and time its own window? *M1's `Window::from_environment` returns `None` outside the runner, so such a driver can run without markers; what it must then measure is still open.*
15. **Workload identity across roles.** Does the driver's record supply the sample's workload name and signature, and how do the campaign's checks for one workload name and signature, and for equal parameters for each variant across artifacts, apply to service records and to competitors? *M1 makes the driver's record the sample's record, so the existing checks apply to it alone; services print no record in M1.*
16. **Representing window metrics.** Are service metrics over the window a phase of their own beside `measured`, `attached` and `process`, or distinguished another way, such as by role, and does the chosen form need a new record version? *M1 journals them with each service's role in the sample, in a `window` phase that records may not contain, so records keep their version; summaries name them `ROLE/METRIC` and are version 3.*
17. **Retiring the Python drivers.** What agreement between a Python driver's results and its replacement's on the same host is enough to retire the driver: for example, medians within the noise between rounds, or agreement within the layout replicas' range?
18. **Separately published helpers.** Decided on 9 November 2026; see requirement 4.
19. **Identifying external programs.** How is an external program's version obtained, and does recorded mode refuse a program whose version cannot be determined, as unidentified, or only record that its version is unknown? *M1 records the first line the program prints for `--version`, or that its version is unknown, and refuses nothing for it.*
20. **CPU spent on a service's behalf.** A service's process CPU time, from its CPU-time clock, `/proc/PID/stat`, or `getrusage`, excludes softirq time the kernel spends on its behalf, such as receiving and sending its network traffic. Measuring base-http's hello server with base-benchmark-load on hana, that was about a quarter of the server's CPU, 1.2 to 1.8 µs per request, so CPU per request from process snapshots understates a network server's cost. Should the window also measure CPU attributable to a service, for example softirq and system time from `/proc/stat` on the service's pinned CPUs between the markers, which is valid only when the service has those CPUs to itself, and how would such a value be labelled and checked?
