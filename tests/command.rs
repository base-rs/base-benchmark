// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

mod support;

use base_benchmark::{CampaignStatus, read_metadata};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
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
        "--replica",
        "false",
        "--output",
        &prepared,
    ]);
    assert_eq!(code, ExitCode::SUCCESS, "{errors}");
    assert!(out.contains("artifacts.json"));
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
        "--manifest-path",
        &manifest,
        "--package",
        "fixture",
        "--scenario",
        "work",
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

#[test]
fn the_binary_stops_and_records_cancelled_campaigns_on_sigterm() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = support::workspace(
        &directory.path().join("project"),
        "\n[[package.metadata.base-benchmark.scenario]]\nname = \"slow\"\narguments = [\"slow\"]\n",
    );
    let (code, _, errors) = command(&[
        "prepare",
        "--manifest-path",
        &text(&manifest),
        "--output",
        &text(&directory.path().join("prepared")),
    ]);
    assert_eq!(code, ExitCode::SUCCESS, "{errors}");
    let campaign = directory.path().join("campaign");
    let child = Command::new(env!("CARGO_BIN_EXE_base-benchmark"))
        .args([
            "run",
            "--manifest-path",
            &text(&manifest),
            "--scenario",
            "slow",
            "--artifacts",
            &text(&directory.path().join("prepared/artifacts.json")),
        ])
        .args([
            "--lock",
            &text(&directory.path().join("lock")),
            "--output",
            &text(&campaign),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !std::fs::read_to_string(campaign.join("metadata.json"))
        .is_ok_and(|text| text.contains("in_progress"))
        || started.elapsed() < Duration::from_millis(500)
    {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the campaign did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the slow workload was not killed"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("received signal"));
    assert_eq!(
        read_metadata(&campaign).unwrap().status,
        CampaignStatus::Cancelled
    );
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
