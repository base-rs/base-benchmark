// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#![cfg(feature = "runner")]

mod support;

use base_benchmark::remote::{Outcome, Remote};
use base_benchmark::{ArtifactManifest, CampaignStatus, Cancellation, read_metadata};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Remote commands that are never cancelled.
trait Uncancelled {
    fn execute_now(&self, command: &str, arguments: Vec<String>) -> io::Result<Outcome>;
}

impl Uncancelled for Remote {
    fn execute_now(&self, command: &str, arguments: Vec<String>) -> io::Result<Outcome> {
        self.execute(command, arguments, &Cancellation::new())
    }
}

/// An executable script. This file has a single test, so no other thread can fork while it is open for writing.
fn script(path: &Path, body: &str) -> PathBuf {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_owned()
}

/// Everything remote happens through a stand-in for `ssh` that runs the command locally, with a temporary directory as the host's.
#[test]
fn remote_commands_mirror_their_inputs_and_bring_back_their_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let manifest = support::workspace(
        &project,
        "\n[[package.metadata.base-benchmark.scenario]]\nname = \"broken\"\narguments = [\"fail\"]\n",
    );
    let host = directory.path().join("host");
    let installs = directory.path().join("installs");
    let ssh = script(
        &directory.path().join("ssh"),
        "#!/bin/sh\nshift\nexec \"$@\"\n",
    );
    // A stand-in for the host's `cargo install`, which installs this test's build of the command.
    let cargo = script(
        &directory.path().join("cargo"),
        &format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in --root) root=$2; shift;; esac; shift; done\necho installed >> {installs}\nmkdir -p \"$root/bin\" && cp {binary} \"$root/bin/base-benchmark\"\n",
            installs = installs.display(),
            binary = env!("CARGO_BIN_EXE_base-benchmark"),
        ),
    );
    support::workspace(&directory.path().join("plain"), "");
    let remote = Remote {
        directory: host.display().to_string(),
        cargo: cargo.display().to_string(),
        ssh,
        ..Remote::new("bench")
    };
    let arguments = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    };
    let manifest_argument = manifest.display().to_string();

    // Preparation uploads the workspace, installs the command once, and brings the artifacts back.
    let prepared = directory.path().join("prepared");
    let outcome = remote
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &manifest_argument,
                "--tree",
                &format!("current={}", project.display()),
                "--output",
                &prepared.display().to_string(),
            ]),
        )
        .unwrap();
    assert!(
        outcome.stdout.contains(&remote.mirror(&prepared)),
        "{}",
        outcome.stdout
    );
    assert_eq!(outcome.output, prepared);
    let mirrored = host.join(format!("mirror{}", project.display()));
    assert!(mirrored.join("Cargo.toml").is_file());
    let manifest_file = ArtifactManifest::read(&prepared.join("artifacts.json")).unwrap();
    assert!(manifest_file.artifacts[0].executable.starts_with(&host));
    assert_eq!(
        std::fs::read_to_string(&installs).unwrap().lines().count(),
        1
    );

    // Preparing again into an output deleted locally replaces the host's stale mirror of it.
    std::fs::remove_dir_all(&prepared).unwrap();
    remote
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &manifest_argument,
                "--output",
                &prepared.display().to_string(),
            ]),
        )
        .unwrap();
    assert!(prepared.join("artifacts.json").is_file());
    // An output that exists locally is not cleared, so the host refuses it as preparation would here.
    let error = remote
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &manifest_argument,
                "--output",
                &prepared.display().to_string(),
            ]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("failed"), "{error}");
    assert!(prepared.join("artifacts.json").is_file());
    // A run uses the host's artifacts, and its default output comes back under the local target directory.
    let outcome = remote
        .execute_now(
            "run",
            arguments(&[
                "work",
                "--manifest-path",
                &manifest_argument,
                "--artifacts",
                &prepared.join("artifacts.json").display().to_string(),
                "--rounds",
                "1",
                "--lock",
                &directory.path().join("lock").display().to_string(),
            ]),
        )
        .unwrap();
    assert!(outcome.stdout.contains("screening"), "{}", outcome.stdout);
    let campaigns = std::fs::read_dir(project.join("target/base-benchmark"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(campaigns.len(), 1);
    assert!(
        campaigns[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("work-screening-")
    );
    assert_eq!(
        read_metadata(&campaigns[0]).unwrap().status,
        CampaignStatus::Complete
    );
    assert_eq!(
        std::fs::read_to_string(&installs).unwrap().lines().count(),
        1,
        "an installed command of this version is reused"
    );

    // A failed smoke run still brings its campaigns back, so the failure can be inspected.
    let smoke = directory.path().join("smoke");
    let error = remote
        .execute_now(
            "smoke",
            arguments(&[
                "--manifest-path",
                &manifest_argument,
                "--output",
                &smoke.display().to_string(),
                "--lock",
                &directory.path().join("lock").display().to_string(),
            ]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("failed"), "{error}");
    assert_eq!(
        read_metadata(&smoke.join("broken")).unwrap().status,
        CampaignStatus::Failed
    );

    // Explicit remote programs skip installation; failures to reach the host are reported.
    let explicit = Remote {
        program: Some(env!("CARGO_BIN_EXE_base-benchmark").to_owned()),
        ..remote.clone()
    };
    let error = explicit
        .execute_now(
            "prepare",
            arguments(&["--manifest-path", &manifest_argument, "--tree", "current"]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("expected LABEL=PATH"), "{error}");
    let unreachable = Remote {
        ssh: directory.path().join("missing-ssh"),
        rsync: directory.path().join("missing-rsync"),
        ..remote.clone()
    };
    assert!(
        unreachable
            .execute_now(
                "prepare",
                arguments(&["--manifest-path", &manifest_argument])
            )
            .is_err()
    );
    // An rsync that fails is reported, with what it was copying.
    let failing_rsync = Remote {
        rsync: PathBuf::from("false"),
        ..remote.clone()
    };
    let error = failing_rsync
        .execute_now(
            "prepare",
            arguments(&["--manifest-path", &manifest_argument]),
        )
        .unwrap_err();
    assert!(
        error.to_string().starts_with("rsync ") && error.to_string().contains("failed with"),
        "{error}"
    );
    let outside = Remote {
        local_cargo: directory.path().join("missing-cargo"),
        ..remote.clone()
    };
    assert!(outside.execute_now("smoke", Vec::new()).is_err());
    // Empty paths cannot be made absolute.
    for (name, value) in [
        ("--manifest-path", ""),
        ("--output", ""),
        ("--artifacts", ""),
        ("--tree", "x="),
    ] {
        let mut values = vec!["--manifest-path", &manifest_argument, name, value];
        if name == "--manifest-path" {
            values.drain(..2);
        }
        assert!(
            remote.execute_now("prepare", arguments(&values)).is_err(),
            "{name}"
        );
    }
    // A command that fails on the host before writing its output has nothing to bring back, so no download is attempted. A download is an rsync whose destination is local.
    let downloads = directory.path().join("downloads");
    let recording_rsync = script(
        &directory.path().join("recording-rsync"),
        &format!(
            "#!/bin/sh\nfor last; do :; done\ncase \"$last\" in bench:*) ;; *) echo \"$last\" >> {};; esac\nexec rsync \"$@\"\n",
            downloads.display()
        ),
    );
    let recorded = Remote {
        rsync: recording_rsync.clone(),
        ..explicit.clone()
    };
    std::fs::write(directory.path().join("invalid.json"), "{}").unwrap();
    let invalid_run = arguments(&[
        "work",
        "--manifest-path",
        &manifest_argument,
        "--artifacts",
        &directory.path().join("invalid.json").display().to_string(),
        "--mode",
        "smoke",
    ]);
    let error = recorded
        .execute_now("run", invalid_run.clone())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("base-benchmark run on bench failed"),
        "{error}"
    );
    assert!(
        !downloads.exists(),
        "{:?}",
        std::fs::read_to_string(&downloads)
    );
    // On the command line, the host's error is what remains on standard error.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_base-benchmark"))
        .args(["run", "--host", "bench", "--remote-directory"])
        .arg(&host)
        .args(["--remote-program", env!("CARGO_BIN_EXE_base-benchmark")])
        .args(&invalid_run)
        .env("BASE_BENCHMARK_SSH", directory.path().join("ssh"))
        .env("BASE_BENCHMARK_RSYNC", &recording_rsync)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("invalid.json"), "{stderr}");
    assert!(!stderr.contains("rsync"), "{stderr}");
    assert!(!downloads.exists());
    // A failed command whose output exists still brings it back, also when its path has spaces, which the host's check for it quotes.
    let failed_smoke = directory.path().join("failed smoke");
    recorded
        .execute_now(
            "smoke",
            arguments(&[
                "--manifest-path",
                &manifest_argument,
                "--output",
                &failed_smoke.display().to_string(),
                "--lock",
                &directory.path().join("lock").display().to_string(),
            ]),
        )
        .unwrap_err();
    assert_eq!(
        std::fs::read_to_string(&downloads).unwrap().trim(),
        format!("{}/", failed_smoke.display())
    );
    assert!(failed_smoke.join("broken").is_dir());
    // The default smoke output comes back too.
    let plain = directory
        .path()
        .join("plain/Cargo.toml")
        .display()
        .to_string();
    let lock = directory.path().join("lock").display().to_string();
    explicit
        .execute_now(
            "smoke",
            arguments(&["--manifest-path", &plain, "--lock", &lock]),
        )
        .unwrap();
    assert!(
        directory
            .path()
            .join("plain/target/base-benchmark")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );
    // A failed installation, and a host that cannot be reached once files are mirrored, are reported.
    let failing_install = Remote {
        directory: directory.path().join("other-host").display().to_string(),
        cargo: "false".to_owned(),
        ..remote.clone()
    };
    let error = failing_install
        .execute_now(
            "prepare",
            arguments(&["--manifest-path", &manifest_argument]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("on bench failed"), "{error}");
    let no_ssh = Remote {
        ssh: directory.path().join("missing-ssh"),
        rsync: PathBuf::from("true"),
        ..remote.clone()
    };
    assert!(
        no_ssh
            .execute_now(
                "prepare",
                arguments(&["--manifest-path", &manifest_argument])
            )
            .is_err()
    );

    // The command line hands remote commands to the same code.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_base-benchmark"))
        .args([
            "smoke",
            "--host",
            "bench",
            "--remote-directory",
            &host.display().to_string(),
            "--remote-program",
            env!("CARGO_BIN_EXE_base-benchmark"),
            "--manifest-path",
            &plain,
            "--lock",
            &lock,
        ])
        .env("BASE_BENCHMARK_SSH", directory.path().join("ssh"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("smoke"));
    // A relative remote directory is under the remote user's home.
    let home = directory.path().join("home");
    let homed_ssh = script(
        &directory.path().join("homed-ssh"),
        &format!("#!/bin/sh\nshift\nHOME={} exec \"$@\"\n", home.display()),
    );
    let relative = Remote {
        directory: "relative-host".to_owned(),
        ssh: homed_ssh,
        ..explicit.clone()
    };
    relative
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &plain,
                "--output",
                &directory
                    .path()
                    .join("relative-prepared")
                    .display()
                    .to_string(),
            ]),
        )
        .unwrap();
    assert!(
        home.join(format!(
            "relative-host/mirror{}",
            directory.path().join("plain").display()
        ))
        .is_dir()
    );
    // Resolving a relative directory needs the host.
    let unresolvable = Remote {
        directory: "relative-host".to_owned(),
        ssh: directory.path().join("missing-ssh"),
        ..explicit.clone()
    };
    assert!(
        unresolvable
            .execute_now("prepare", arguments(&["--manifest-path", &plain]))
            .is_err()
    );
    let no_workspace = remote
        .execute_now(
            "smoke",
            arguments(&[
                "--manifest-path",
                &directory
                    .path()
                    .join("none/Cargo.toml")
                    .display()
                    .to_string(),
            ]),
        )
        .unwrap_err();
    assert!(
        no_workspace.to_string().contains("none/Cargo.toml"),
        "{no_workspace}"
    );

    // Missing trees are reported, naming the path, before the host is contacted.
    let offline = Remote {
        ssh: directory.path().join("missing-ssh"),
        rsync: directory.path().join("missing-rsync"),
        ..remote.clone()
    };
    let absent = directory.path().join("absent");
    let error = offline
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &plain,
                "--tree",
                &format!("x={}", absent.display()),
            ]),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains(&absent.display().to_string()),
        "{error}"
    );
    // A manifest Cargo cannot read has no workspace to mirror.
    let unreadable = directory.path().join("unreadable");
    std::fs::create_dir_all(&unreadable).unwrap();
    std::fs::write(unreadable.join("Cargo.toml"), "not a manifest").unwrap();
    let error = offline
        .execute_now(
            "smoke",
            arguments(&[
                "--manifest-path",
                &unreadable.join("Cargo.toml").display().to_string(),
            ]),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("cannot find its workspace"),
        "{error}"
    );
    // A tree whose metadata and Git directory cannot be read is still mirrored as it is.
    std::fs::write(unreadable.join(".git"), "gitdir: /nowhere\n").unwrap();
    let error = offline
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &plain,
                "--tree",
                &format!("x={}", unreadable.display()),
            ]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("missing-rsync"), "{error}");
    let error = Remote {
        host: "-oProxyCommand=true".to_owned(),
        ..remote.clone()
    }
    .execute_now("prepare", Vec::new())
    .unwrap_err();
    assert!(
        error.to_string().contains("not an SSH destination"),
        "{error}"
    );

    // `..` in an output is resolved locally, so its mirror stays inside the mirror.
    let dotted = directory.path().join("dotted");
    explicit
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &plain,
                "--output",
                &format!("{}/plain/../dotted", directory.path().display()),
            ]),
        )
        .unwrap();
    assert!(dotted.join("artifacts.json").is_file());
    assert!(host.join(format!("mirror{}", dotted.display())).is_dir());

    // An output named through a symlink maps as an input through it does, so its artifacts can be run.
    let real = directory.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = directory.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let linked_output = link.join("prepared");
    explicit
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &plain,
                "--output",
                &linked_output.display().to_string(),
            ]),
        )
        .unwrap();
    assert!(
        host.join(format!("mirror{}/prepared", real.display()))
            .is_dir()
    );
    explicit
        .execute_now(
            "run",
            arguments(&[
                "work",
                "--manifest-path",
                &plain,
                "--artifacts",
                &linked_output.join("artifacts.json").display().to_string(),
                "--mode",
                "smoke",
                "--lock",
                &lock,
                "--output",
                &link.join("campaign").display().to_string(),
            ]),
        )
        .unwrap();
    assert!(real.join("campaign/summary.json").is_file());
    // A linked worktree keeps its Git state on the host, whose mirror names the mirrored repository.
    let repository = directory.path().join("repository");
    support::workspace(&repository, "");
    support::commit(&repository);
    let worktree = directory.path().join("worktree");
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["worktree", "add", "--quiet", "--detach"])
        .arg(&worktree)
        .status()
        .unwrap();
    assert!(status.success());
    let outcome = explicit
        .execute_now(
            "prepare",
            arguments(&[
                "--manifest-path",
                &plain,
                "--tree",
                &format!("linked={}", worktree.display()),
                "--output",
                &directory.path().join("linked").display().to_string(),
            ]),
        )
        .unwrap();
    let linked = ArtifactManifest::read(&outcome.output.join("artifacts.json")).unwrap();
    let source = &linked.artifacts[0].source;
    assert!(source.revision.is_some(), "{source:?}");
    assert_eq!(source.dirty, Some(false), "{source:?}");

    // Cancelling stops the host's command, and what it wrote still comes back.
    let stopped = directory.path().join("stopped");
    let slow = script(
        &directory.path().join("slow"),
        "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in --output) output=$2; shift;; esac; shift; done\nmkdir -p \"$output\"\nif [ -e /dev/fd/3 ]; then touch \"$output/descriptor\"; fi\ntrap 'echo stopped > \"$output/status\"; kill $s; exit 7' TERM\nsleep 30 >/dev/null 2>&1 &\ns=$!\ntouch \"$output/started\"\nwait $s\n",
    );
    // SSH, here and for rsync, records the signals it starts with ignored, then runs as before.
    let ignored = directory.path().join("ignored");
    let recording_ssh = script(
        &directory.path().join("recording-ssh"),
        &format!(
            "#!/bin/sh\ngrep SigIgn /proc/$$/status >> {} 2>/dev/null\nshift\nexec \"$@\"\n",
            ignored.display()
        ),
    );
    let slow = Remote {
        program: Some(slow.display().to_string()),
        ssh: recording_ssh,
        ..remote.clone()
    };
    let started = host.join(format!("mirror{}/started", stopped.display()));
    let cancellation = Cancellation::new();
    let canceller = {
        let cancellation = cancellation.clone();
        std::thread::spawn(move || {
            while !started.exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            cancellation.cancel();
        })
    };
    let begun = std::time::Instant::now();
    let run = arguments(&[
        "work",
        "--manifest-path",
        &plain,
        "--artifacts",
        &prepared.join("artifacts.json").display().to_string(),
        "--output",
        &stopped.display().to_string(),
    ]);
    let error = slow.execute("run", run.clone(), &cancellation).unwrap_err();
    canceller.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted, "{error}");
    assert!(begun.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(
        std::fs::read_to_string(stopped.join("status")).unwrap(),
        "stopped\n"
    );
    assert!(
        !stopped.join("descriptor").exists(),
        "the host's command does not inherit the watcher's input"
    );
    // SIGINT (2) and SIGQUIT (3) are ignored in the command's SSH, so a terminal's interrupt reaches only this process.
    #[cfg(target_os = "linux")]
    {
        // rsync's connections record theirs too; the command's connection is the one with both ignored.
        let recorded = std::fs::read_to_string(&ignored).unwrap();
        let both = recorded
            .lines()
            .filter_map(|line| line.split_whitespace().last())
            .map(|mask| u64::from_str_radix(mask, 16).unwrap())
            .filter(|mask| mask & 0b110 == 0b110)
            .count();
        assert_eq!(both, 1, "{recorded}");
    }
    // A cancelled command never starts.
    let error = slow.execute("run", run, &cancellation).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted, "{error}");
}
