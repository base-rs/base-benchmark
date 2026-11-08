// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#![cfg(feature = "runner")]

mod support;

use base_benchmark::{CampaignStatus, read_entries, read_metadata};
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

fn command(arguments: &[&str]) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let code = base_benchmark::cli::main(
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        &mut out,
        &mut errors,
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(errors).unwrap(),
    )
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

#[test]
fn every_command_uses_the_shared_workspace_operations() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = text(&support::workspace(&directory.path().join("project"), ""));
    let lock = text(&directory.path().join("lock"));

    let (code, out, _) = command(&["list", "--manifest-path", &manifest]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("\"package\": \"fixture\""));

    let prepared = text(&directory.path().join("prepared"));
    let (code, out, errors) = command(&[
        "prepare",
        "--manifest-path",
        &manifest,
        "--package",
        "fixture",
        "--replicas",
        "1",
        "--scenario",
        "work",
        "--output",
        &prepared,
    ]);
    assert_eq!(code, ExitCode::SUCCESS, "{errors}");
    assert!(out.contains("current-replica"));
    let (code, _, errors) = command(&[
        "prepare",
        "--manifest-path",
        &manifest,
        "--tree",
        &format!("one={}", directory.path().join("project").display()),
        "--output",
        &prepared,
    ]);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(errors.contains("new directory"));

    let artifacts = text(&directory.path().join("prepared/artifacts.json"));
    let campaign = text(&directory.path().join("campaign"));
    let (code, out, errors) = command(&[
        "run",
        "work",
        "--manifest-path",
        &manifest,
        "--package",
        "fixture",
        "--artifacts",
        &artifacts,
        "--mode",
        "screening",
        "--rounds",
        "2",
        "--seed",
        "3",
        "--cpus",
        "0",
        "--timeout-seconds",
        "60",
        "--lock",
        &lock,
        "--output",
        &campaign,
        "--decision-rule",
        "descriptive",
        "--idle-host",
        "false",
        "--stack-padding",
        "true",
    ]);
    assert_eq!(code, ExitCode::SUCCESS, "{errors}");
    assert!(out.contains("screening: paired triage"), "{out}");
    assert!(errors.contains("round 2/2"));
    let metadata = read_metadata(Path::new(&campaign)).unwrap();
    assert_eq!(metadata.plan.invocation[1], "run");
    assert_eq!(metadata.plan.cpus, [0]);

    let (code, out, _) = command(&["summarize", &campaign]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("Variant current/reference -> current/fast"));

    let (code, out, errors) = command(&[
        "smoke",
        "--manifest-path",
        &manifest,
        "--package",
        "fixture",
        "--lock",
        &lock,
        "--output",
        &text(&directory.path().join("smoke")),
    ]);
    assert_eq!(code, ExitCode::SUCCESS, "{errors}");
    assert!(out.contains("smoke: correctness"));
}

/// Start the binary and wait until its campaign is in progress, and so guarded against signals. The `slow` scenario's workload creates `started` in `directory` once it runs.
fn waiting_campaign(directory: &Path, scenario: &str) -> (Child, std::path::PathBuf) {
    let manifest = support::workspace(
        &directory.join("project"),
        &format!(
            "\n[[package.metadata.base-benchmark.scenario]]\nname = \"slow\"\narguments = [\"slow\", {:?}]\n",
            text(&directory.join("started"))
        ),
    );
    let prepared = directory.join("prepared");
    let (code, _, errors) = command(&[
        "prepare",
        "--manifest-path",
        &text(&manifest),
        "--output",
        &text(&prepared),
    ]);
    assert_eq!(code, ExitCode::SUCCESS, "{errors}");
    let campaign = directory.join("campaign");
    let child = Command::new(env!("CARGO_BIN_EXE_base-benchmark"))
        .args([
            "run",
            scenario,
            "--manifest-path",
            &text(&manifest),
            "--artifacts",
            &text(&prepared.join("artifacts.json")),
        ])
        .args([
            "--lock",
            &text(&directory.join("lock")),
            "--output",
            &text(&campaign),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    wait_for(started, "the campaign did not start", || {
        std::fs::read_to_string(campaign.join("metadata.json"))
            .is_ok_and(|text| text.contains("in_progress"))
    });
    (child, campaign)
}

/// Poll until `ready`, with a watchdog that only turns a hang into a failure.
fn wait_for(started: Instant, hang: &str, ready: impl Fn() -> bool) {
    while !ready() {
        assert!(started.elapsed() < Duration::from_secs(60), "{hang}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn signal(child: &Child, signal: nix::sys::signal::Signal) {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id() as i32), signal).unwrap();
}

#[test]
fn the_binary_stops_and_records_cancelled_campaigns_on_sigterm() {
    let directory = tempfile::tempdir().unwrap();
    let (child, campaign) = waiting_campaign(directory.path(), "slow");
    // Signal only once the workload runs, so that cancellation has a process to kill.
    wait_for(Instant::now(), "the workload did not start", || {
        directory.path().join("started").exists()
    });
    signal(&child, nix::sys::signal::Signal::SIGTERM);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    // A cancelled sample is journaled with an error whether or not its workload was killed, so check how it ended: a workload left to run would exit normally after 30 seconds, well within the default timeout.
    let entries = read_entries(&campaign.join("samples.jsonl")).unwrap();
    assert!(
        matches!(&entries[..], [entry] if entry.error.is_some() && entry.signal == Some(9) && !entry.timed_out),
        "the slow workload was not killed: {entries:?}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("stopping the campaign"));
    assert_eq!(
        read_metadata(&campaign).unwrap().status,
        CampaignStatus::Cancelled
    );
}

#[test]
fn a_second_signal_exits_immediately() {
    let directory = tempfile::tempdir().unwrap();
    let lock = std::fs::File::create(directory.path().join("lock")).unwrap();
    let _held = nix::fcntl::Flock::lock(lock, nix::fcntl::FlockArg::LockExclusive).unwrap();
    let (child, _) = waiting_campaign(directory.path(), "work");
    // Different signals are not merged while pending, so both reach the handler before the lock wait notices either.
    signal(&child, nix::sys::signal::Signal::SIGTERM);
    signal(&child, nix::sys::signal::Signal::SIGINT);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Pending signals are delivered in signal-number order, so either can be the second.
    assert!(matches!(output.status.code(), Some(130 | 143)), "{stderr}");
    assert!(stderr.contains("exiting"));
}

struct Rejecting;

impl std::io::Write for Rejecting {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("closed"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn closed_stdout_fails_report_commands() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = text(&support::workspace(&directory.path().join("project"), ""));
    let arguments = [
        "smoke",
        "--manifest-path",
        &manifest,
        "--lock",
        &text(&directory.path().join("lock")),
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(
        base_benchmark::cli::main(arguments, &mut Rejecting, &mut Vec::new()),
        ExitCode::FAILURE
    );
    assert!(std::io::Write::flush(&mut Rejecting).is_ok());
}

#[test]
fn remote_options_hand_the_command_to_another_host() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = text(&support::workspace(&directory.path().join("project"), ""));
    // A malformed tree is reported before any host is contacted.
    let (code, _, errors) = command(&[
        "prepare",
        "--host",
        "bench",
        "--remote-directory",
        "/srv/benchmarks",
        "--remote-program",
        "/opt/base-benchmark",
        "--manifest-path",
        &manifest,
        "--tree",
        "current",
    ]);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(errors.contains("expected LABEL=PATH"), "{errors}");
    for repeated in ["host", "remote-directory", "remote-program"] {
        let flag = format!("--{repeated}");
        let mut values = vec!["run", "--host", "bench"];
        if repeated != "host" {
            values.extend([flag.as_str(), "1"]);
        }
        values.extend([flag.as_str(), "1"]);
        let (code, _, errors) = command(&values);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(errors.contains("more than once"), "{repeated}: {errors}");
    }
    let (code, _, errors) = command(&[
        "smoke",
        "--host",
        "bench",
        "--manifest-path",
        "",
        "--remote-program",
        "x",
    ]);
    assert_eq!(code, ExitCode::FAILURE, "{errors}");
    let (code, out, _) = command(&["version"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!("base-benchmark {}\n", env!("CARGO_PKG_VERSION"))
    );
}
