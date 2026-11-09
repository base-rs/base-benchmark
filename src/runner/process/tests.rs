// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::test_support::SHELL;
use std::os::unix::fs::PermissionsExt;

fn environment() -> BTreeMap<String, String> {
    BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())])
}

fn launch_with(
    program: &Path,
    script: &str,
    cpus: &[usize],
    timeout: Duration,
    cancellation: &Cancellation,
) -> io::Result<Execution> {
    let arguments = vec!["-c".to_owned(), script.to_owned()];
    let environment = environment();
    let launch = Launch {
        program,
        arguments: &arguments,
        environment: &environment,
        directory: Path::new("/"),
        cpus,
    };
    execute(&launch, timeout, cancellation)
}

fn run(script: &str, timeout: Duration, cancellation: &Cancellation) -> Execution {
    launch_with(Path::new(SHELL), script, &[], timeout, cancellation).unwrap()
}

#[test]
fn captures_output_and_exit_status() {
    let execution = run(
        "echo out; echo err >&2; exit 3",
        Duration::from_secs(10),
        &Cancellation::new(),
    );
    assert_eq!(
        (
            execution.stdout.as_slice(),
            execution.stderr.as_slice(),
            execution.code
        ),
        (&b"out\n"[..], &b"err\n"[..], Some(3))
    );
    assert!(!execution.succeeded());
    assert!(execution.duration_ns > 0);
}

#[test]
fn workloads_see_only_the_given_environment_and_directory() {
    // The test process has HOME, USER, and Cargo variables; none may reach the workload.
    let execution = run(
        "env | sort; pwd",
        Duration::from_secs(10),
        &Cancellation::new(),
    );
    assert!(execution.succeeded());
    let text = String::from_utf8(execution.stdout).unwrap();
    assert!(
        text.lines()
            .filter(|line| line.contains('='))
            .all(|line| line.starts_with("PATH=")
                || line.starts_with("PWD=")
                || line.starts_with("SHLVL=")
                || line.starts_with("_=")),
        "{text}"
    );
    assert_eq!(text.lines().last(), Some("/"));
}

#[test]
fn timeouts_kill_the_whole_process_group() {
    // The background child keeps stdout open; only group cleanup lets the reader finish.
    let execution = run(
        "sleep 30 & sleep 30",
        Duration::from_millis(200),
        &Cancellation::new(),
    );
    assert!(execution.timed_out);
    assert_eq!(execution.signal, Some(9));
    assert!(!execution.succeeded());
}

#[test]
fn descendants_are_killed_after_the_leader_exits() {
    let execution = run(
        "sleep 30 & echo done",
        Duration::from_secs(20),
        &Cancellation::new(),
    );
    // A surviving descendant would hold the output open past the grace period, and the sample would fail as escaped.
    assert!(execution.succeeded());
    assert_eq!(execution.stdout, b"done\n");
}

#[test]
fn escaped_descendants_fail_the_sample_instead_of_hanging() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left");
    // The leader exits once the background process has left the group, which it records after `setsid`.
    let execution = run(
        &format!(
            "setsid sh -c 'touch \"$0\"; exec sleep 30' '{0}' & until [ -e '{0}' ]; do sleep 0.01; done; echo done",
            left.display()
        ),
        Duration::from_secs(20),
        &Cancellation::new(),
    );
    // Escaped means the runner stopped waiting after the grace period while the output was still open.
    assert!(execution.escaped);
    assert!(!execution.succeeded());
}

#[test]
fn cancellation_stops_running_and_new_processes() {
    let cancellation = Cancellation::new();
    let canceller = cancellation.clone();
    let stop = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        canceller.cancel();
    });
    let execution = run("sleep 30", Duration::from_secs(20), &cancellation);
    stop.join().unwrap();
    assert!(execution.cancelled && !execution.succeeded());
    // `sleep 30` outlasts the deadline, so only cancellation's kill stops it before the watchdog does.
    assert!(!execution.timed_out);
    assert_eq!(execution.signal, Some(9));
    let execution = run("sleep 30", Duration::from_secs(20), &cancellation);
    assert!(execution.cancelled);
    assert_eq!(execution.signal, Some(9));
}

#[test]
fn the_first_termination_signal_cancels_guarded_campaigns() {
    let cancellation = Cancellation::new();
    let guard = cancellation.on_termination_signals();
    let other = Cancellation::new();
    drop(other.on_termination_signals());
    nix::sys::signal::raise(Signal::SIGTERM).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cancellation.is_cancelled() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(cancellation.is_cancelled());
    assert!(
        !other.is_cancelled(),
        "a dropped guard no longer receives signals"
    );
    drop(guard);
    assert!(
        !guarded()
            .iter()
            .any(|guarded| Arc::ptr_eq(&guarded.0, &cancellation.0))
    );
}

#[test]
fn busy_executables_are_retried() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("busy");
    let mut file = std::fs::File::create(&path).unwrap();
    std::io::Write::write_all(&mut file, b"#!/bin/sh\necho ready\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = launch_with(
        &path,
        "",
        &[],
        Duration::from_secs(10),
        &Cancellation::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ExecutableFileBusy);
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        drop(file);
    });
    let execution = launch_with(
        &path,
        "",
        &[],
        Duration::from_secs(10),
        &Cancellation::new(),
    )
    .unwrap();
    closer.join().unwrap();
    assert_eq!(execution.stdout, b"ready\n");
}

#[test]
fn workloads_inherit_the_requested_cpu() {
    let allowed = sched_getaffinity(Pid::from_raw(0)).unwrap();
    let cpu = (0..CpuSet::count())
        .find(|cpu| allowed.is_set(*cpu).unwrap())
        .unwrap();
    let execution = launch_with(
        Path::new(SHELL),
        "grep Cpus_allowed_list /proc/self/status",
        &[cpu],
        Duration::from_secs(10),
        &Cancellation::new(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(execution.stdout)
            .unwrap()
            .split_whitespace()
            .last()
            .unwrap(),
        cpu.to_string()
    );
    assert_eq!(sched_getaffinity(Pid::from_raw(0)).unwrap(), allowed);

    let too_large = launch_with(
        Path::new(SHELL),
        "true",
        &[CpuSet::count()],
        Duration::from_secs(10),
        &Cancellation::new(),
    )
    .unwrap_err();
    assert!(too_large.to_string().contains("exceeds"));
    let offline = (0..CpuSet::count())
        .rev()
        .find(|cpu| !allowed.is_set(*cpu).unwrap())
        .unwrap();
    let unavailable = launch_with(
        Path::new(SHELL),
        "true",
        &[offline],
        Duration::from_secs(10),
        &Cancellation::new(),
    )
    .unwrap_err();
    assert!(unavailable.to_string().contains("cannot place workloads"));
}

#[test]
fn missing_programs_are_errors() {
    let error = launch_with(
        Path::new("/nonexistent/workload"),
        "",
        &[],
        Duration::from_secs(1),
        &Cancellation::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("/nonexistent/workload"));
}

struct Failing;

impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("broken"))
    }
}

#[test]
fn readers_bound_output() {
    assert_eq!(read_head(&b"abcdef"[..], 4), (b"abcd".to_vec(), true));
    assert_eq!(read_head(&b"abcd"[..], 4), (b"abcd".to_vec(), false));
    assert_eq!(read_head(Failing, 4), (Vec::new(), false));
    assert_eq!(read_tail(&vec![1; 20_000][..], 10), vec![1; 10]);
    assert!(read_tail(Failing, 10).is_empty());
}

#[test]
fn signals_ignored_at_start_are_left_alone() {
    assert_eq!(
        ignored_signals(Some("Name:\tx\nSigIgn:\t0000000000000001\n")),
        1,
        "SIGHUP is bit 0"
    );
    assert_eq!(ignored_signals(Some("SigIgn:\tzz\n")), 0);
    assert_eq!(ignored_signals(None), 0);
}

#[test]
fn each_guard_releases_only_its_own_registration() {
    let cancellation = Cancellation::new();
    let first = cancellation.on_termination_signals();
    let second = cancellation.on_termination_signals();
    let count = || {
        guarded()
            .iter()
            .filter(|guarded| Arc::ptr_eq(&guarded.0, &cancellation.0))
            .count()
    };
    assert_eq!(count(), 2);
    drop(first);
    assert_eq!(count(), 1);
    drop(second);
    assert_eq!(count(), 0);
}

#[test]
fn exits_are_observed_without_reaping() {
    let arguments = vec!["-c".to_owned(), "sleep 30".to_owned()];
    let environment = environment();
    let launch = Launch {
        program: Path::new(SHELL),
        arguments: &arguments,
        environment: &environment,
        directory: Path::new("/"),
        cpus: &[],
    };
    let cancellation = Cancellation::new();
    let (mut child, group) = start(&launch, Stdio::null(), &cancellation).unwrap();
    assert!(!has_exited(group));
    kill_group(group);
    wait_exit(group);
    assert!(has_exited(group), "an exited leader stays unreaped");
    assert_eq!(reap(&mut child, group, &cancellation).signal(), Some(9));
}

#[test]
fn workloads_start_with_the_stop_signals_at_their_defaults_and_unblocked() {
    use crate::test_support::{mask_of, signal_mask};
    let this_thread = || std::fs::read_to_string("/proc/thread-self/status").unwrap();
    let stopping = mask_of(&STOP_SIGNALS);
    assert_eq!(STOP_SIGNALS.map(Signal::as_str), crate::role::STOP_SIGNALS);
    crate::test_support::with_signals(
        "runner::process::tests::workloads_start_with_the_stop_signals_at_their_defaults_and_unblocked",
        &STOP_SIGNALS,
        &STOP_SIGNALS,
        || {
            let execution = run(
                "grep -E '^Sig(Ign|Blk):' /proc/self/status",
                Duration::from_secs(20),
                &Cancellation::new(),
            );
            assert!(execution.succeeded());
            let status = String::from_utf8(execution.stdout).unwrap();
            assert_eq!(signal_mask(&status, "SigIgn") & stopping, 0, "{status}");
            assert_eq!(signal_mask(&status, "SigBlk") & stopping, 0, "{status}");
            // What the child runs before it execs.
            reset_stop_signals().unwrap();
            let status = this_thread();
            assert_eq!(signal_mask(&status, "SigIgn") & stopping, 0, "{status}");
            assert_eq!(signal_mask(&status, "SigBlk") & stopping, 0, "{status}");
        },
    );
}
