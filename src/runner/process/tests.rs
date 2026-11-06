// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::test_support::SHELL;

fn shell(script: &str) -> Vec<String> {
    vec!["-c".to_owned(), script.to_owned()]
}

fn run(script: &str, timeout: Duration, cancellation: &Cancellation) -> Execution {
    execute(Path::new(SHELL), &shell(script), &[], timeout, cancellation).unwrap()
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
    let execution = run(
        "printf %s \"$LC_ALL\"",
        Duration::from_secs(10),
        &Cancellation::new(),
    );
    assert!(execution.succeeded());
    assert_eq!(execution.stdout, b"C");
    assert!(execution.duration_ns > 0);
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
    let started = Instant::now();
    let execution = run(
        "sleep 30 & echo done",
        Duration::from_secs(20),
        &Cancellation::new(),
    );
    assert!(execution.succeeded());
    assert_eq!(execution.stdout, b"done\n");
    assert!(started.elapsed() < Duration::from_secs(10));
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
    let execution = run("sleep 30", Duration::from_secs(20), &cancellation);
    assert!(execution.cancelled);
    assert_eq!(execution.signal, Some(9));
}

#[test]
fn termination_signals_cancel_campaigns() {
    let cancellation = Cancellation::new();
    let guard = cancellation.on_termination_signals();
    nix::sys::signal::raise(Signal::SIGHUP).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cancellation.is_cancelled() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(guard);
    assert!(cancellation.is_cancelled());
}

#[test]
fn workloads_inherit_the_requested_cpu() {
    let allowed = sched_getaffinity(Pid::from_raw(0)).unwrap();
    let cpu = (0..CpuSet::count())
        .find(|cpu| allowed.is_set(*cpu).unwrap())
        .unwrap();
    let execution = execute(
        Path::new(SHELL),
        &shell("grep Cpus_allowed_list /proc/self/status"),
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

    let too_large = execute(
        Path::new(SHELL),
        &shell("true"),
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
    let unavailable = execute(
        Path::new(SHELL),
        &shell("true"),
        &[offline],
        Duration::from_secs(10),
        &Cancellation::new(),
    )
    .unwrap_err();
    assert!(unavailable.to_string().contains("cannot place workloads"));
}

#[test]
fn missing_programs_are_errors() {
    let error = execute(
        Path::new("/nonexistent/workload"),
        &[],
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
