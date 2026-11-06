// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn failure(arguments: &[&str]) -> String {
    let mut errors = Vec::new();
    assert_eq!(
        main(strings(arguments), &mut Vec::new(), &mut errors),
        ExitCode::FAILURE
    );
    String::from_utf8(errors).unwrap()
}

#[test]
fn cpu_lists_accept_ranges() {
    assert_eq!(parse_cpus("4,0-2,1").unwrap(), [0, 1, 2, 4]);
    for invalid in ["", "a", "3-1", "a-1", "1-a"] {
        assert!(parse_cpus(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn options_are_strict() {
    assert!(failure(&["frobnicate"]).contains("unknown command \"frobnicate\""));
    assert!(failure(&["list", "--manifest-path"]).contains("needs a value"));
    assert!(failure(&["list", "--unknown", "1"]).contains("unknown option --unknown"));
    assert!(failure(&["list", "extra"]).contains("positional"));
    assert!(failure(&["prepare", "--package", "a", "--package", "b"]).contains("more than once"));
    assert!(failure(&["prepare", "--replica", "maybe"]).contains("invalid --replica"));
    assert!(failure(&["prepare", "--tree", "nolabel"]).contains("LABEL=PATH"));
    assert!(failure(&["run", "--artifacts", "a.json"]).contains("--scenario is required"));
    assert!(
        failure(&["run", "--scenario", "s", "--artifacts", "a", "--cpus", "x"])
            .contains("invalid CPU list")
    );
    assert!(
        failure(&[
            "run",
            "--scenario",
            "s",
            "--artifacts",
            "a",
            "--mode",
            "fast"
        ])
        .contains("invalid --mode")
    );
    assert!(failure(&["summarize"]).contains("positional"));
    assert!(failure(&["summarize", "/nonexistent"]).contains("metadata.json"));
    let mut out = Vec::new();
    assert_eq!(
        main(strings(&["help"]), &mut out, &mut Vec::new()),
        ExitCode::SUCCESS
    );
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("usage: base-benchmark")
    );
}

struct Rejecting;

impl Write for Rejecting {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_failures_are_command_failures() {
    assert_eq!(
        main(strings(&["help"]), &mut Rejecting, &mut Rejecting),
        ExitCode::FAILURE
    );
    assert!(Rejecting.flush().is_ok());
}

/// Each option is rejected when repeated or malformed, so every parse step reports its own error.
#[test]
fn every_option_reports_repetition_and_parse_errors() {
    let commands: [(&str, &[&str], &[&str]); 4] = [
        ("prepare", &["package", "replica", "output"], &["replica"]),
        ("smoke", &["package", "output", "lock"], &[]),
        (
            "run",
            &[
                "scenario",
                "artifacts",
                "mode",
                "package",
                "rounds",
                "seed",
                "cpus",
                "timeout-seconds",
                "lock",
                "output",
                "decision-rule",
                "idle-host",
            ],
            &[
                "mode",
                "rounds",
                "seed",
                "cpus",
                "timeout-seconds",
                "idle-host",
            ],
        ),
        ("list", &[], &[]),
    ];
    for (command, options, parsed) in commands {
        let required = if command == "run" {
            vec!["--scenario", "s", "--artifacts", "a"]
        } else {
            Vec::new()
        };
        for option in options {
            let flag = format!("--{option}");
            let mut arguments = vec![command];
            arguments.extend(required.iter().filter(|value| **value != flag).copied());
            arguments.extend([flag.as_str(), "1", flag.as_str(), "1"]);
            assert!(
                failure(&arguments).contains("more than once"),
                "{command} {option}"
            );
        }
        for option in parsed {
            let flag = format!("--{option}");
            let mut arguments = vec![command];
            arguments.extend(required.iter().copied());
            arguments.extend([flag.as_str(), "invalid"]);
            assert!(
                failure(&arguments).contains("invalid"),
                "{command} {option}"
            );
        }
        let mut unknown = vec![command];
        unknown.extend(required.iter().copied());
        unknown.extend(["--unknown", "1"]);
        assert!(failure(&unknown).contains("unknown option"), "{command}");
        let mut missing = vec![command, "--manifest-path", "/nonexistent/Cargo.toml"];
        missing.extend(required.iter().copied());
        assert!(
            failure(&missing).contains("/nonexistent/Cargo.toml"),
            "{command}"
        );
    }
    assert!(failure(&["run", "--scenario", "s"]).contains("--artifacts is required"));
}
