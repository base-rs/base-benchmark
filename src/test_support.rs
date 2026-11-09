// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Fixtures for unit tests: shell workloads and hand-built plans.
use crate::artifact::{
    Artifact, ArtifactManifest, ArtifactRole, BuildIdentity, MANIFEST_SCHEMA, MANIFEST_VERSION,
    SourceIdentity, sha256_file,
};
use crate::config::{Scenario, Suite, Variant};
use crate::plan::{Mode, Plan, PlanOptions};
use crate::role::{ReadyLine, Role, RoleKind, RoleProgram, Stop};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub const SHELL: &str = "/bin/sh";

/// A valid record line with the given wall time for 10 operations.
pub fn record(signature: &str, wall: u64) -> String {
    format!(
        r#"{{"schema":"base-benchmark/process-record","version":1,"workload":"fixture","parameters":{{}},"signature":"{signature}","samples":[{{"operations":10,"metrics":[{{"name":"wall_time","unit":"nanoseconds","phase":"measured","source":"wall","direction":"lower_is_better","measurement":{{"kind":"total","value":{wall}}}}}]}}]}}"#
    )
}

/// Write a shell script. It is run as an argument of `/bin/sh`, never executed directly, so writing it cannot race with `exec`.
pub fn script(directory: &Path, name: &str, body: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

/// Write an executable script through a child process. Written here, another thread could fork while it is open for writing, and running it would then fail with `ETXTBSY` until that child execs.
pub fn write_executable(path: &Path, body: &str) -> PathBuf {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = writer.stdin.take().unwrap();
    input.write_all(body.as_bytes()).unwrap();
    drop(input);
    assert!(writer.wait().unwrap().success());
    path.to_owned()
}

pub fn artifact(label: &str, role: ArtifactRole, executable: &Path) -> Artifact {
    Artifact {
        label: label.to_owned(),
        role,
        features: Some(Vec::new()),
        executable: executable.to_owned(),
        sha256: sha256_file(executable).unwrap(),
        source: SourceIdentity {
            path: PathBuf::from("/source"),
            revision: Some("0123456789abcdef".to_owned()),
            dirty: Some(false),
            lockfile_sha256: None,
            replica_of: (role == ArtifactRole::LayoutReplica).then(|| "baseline".to_owned()),
            path_dependencies: Some(Vec::new()),
            overlay: None,
            lockfile_changes: Vec::new(),
        },
        build: BuildIdentity {
            command: vec!["cargo".to_owned(), "build".to_owned()],
            toolchain: "rustc test".to_owned(),
            cargo: "cargo test".to_owned(),
            environment: Default::default(),
            configuration: Vec::new(),
            layout_salt: None,
        },
    }
}

pub fn manifest(artifacts: Vec<Artifact>) -> ArtifactManifest {
    ArtifactManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        version: MANIFEST_VERSION,
        package: "fixture".to_owned(),
        binary: "fixture".to_owned(),
        profile: "release".to_owned(),
        artifacts,
        ..ArtifactManifest::default()
    }
}

/// A suite whose scenario runs `script` through the shell, with optional variants passed as the script's argument.
pub fn suite(script: &Path, variants: &[&str]) -> Suite {
    Suite {
        package: "fixture".to_owned(),
        manifest_path: PathBuf::from("/source/Cargo.toml"),
        workspace_root: PathBuf::from("/"),
        binary: "fixture".to_owned(),
        profile: "release".to_owned(),
        features: Vec::new(),
        scenarios: vec![Scenario {
            name: "work".to_owned(),
            arguments: vec![script.display().to_string()],
            smoke_arguments: vec!["smoke".to_owned()],
            measure_arguments: vec!["measure".to_owned()],
            variants: variants
                .iter()
                .map(|name| Variant {
                    name: (*name).to_owned(),
                    arguments: vec![(*name).to_owned()],
                })
                .collect(),
            baseline: None,
            features: Vec::new(),
            roles: Vec::new(),
        }],
    }
}

/// A screening plan running `script` with `/bin/sh` for each artifact role.
pub fn plan(script: &Path, variants: &[&str], roles: &[(&str, ArtifactRole)], lock: &Path) -> Plan {
    let shell = Path::new(SHELL);
    let artifacts = manifest(
        roles
            .iter()
            .map(|(label, role)| artifact(label, *role, shell))
            .collect(),
    );
    let mut options = PlanOptions::new(suite(script, variants), "work", artifacts, Mode::Screening);
    options.rounds = Some(2);
    options.seed = Some(7);
    options.lock = lock.to_owned();
    options.timeout = Duration::from_secs(20);
    Plan::new(options).unwrap()
}

/// A tree with a `.cargo/config.toml`, a registry dependency that no registry has, locked, and a path dependency outside it.
pub fn configured_tree(directory: &Path, configuration: &str) -> PathBuf {
    let tree = directory.join("configured");
    let outside = directory.join("outside");
    for (path, contents) in [
        (
            tree.join("Cargo.toml"),
            format!(
                "[package]\nname = \"configured\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nbase-benchmark-absent-fixture = \"0.1\"\noutside = {{ path = \"{}\" }}\n",
                outside.display()
            ),
        ),
        (tree.join("src/lib.rs"), String::new()),
        (
            tree.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"base-benchmark-absent-fixture\"\nversion = \"0.1.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"0000000000000000000000000000000000000000000000000000000000000000\"\n\n[[package]]\nname = \"configured\"\nversion = \"0.1.0\"\ndependencies = [\n \"base-benchmark-absent-fixture\",\n \"outside\",\n]\n\n[[package]]\nname = \"outside\"\nversion = \"0.1.0\"\n".to_owned(),
        ),
        (tree.join(".cargo/config.toml"), configuration.to_owned()),
        (
            outside.join("Cargo.toml"),
            "[package]\nname = \"outside\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".to_owned(),
        ),
        (outside.join("src/lib.rs"), String::new()),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    tree
}

/// Real Cargo, never with network access, recording the directory and arguments of each call.
pub fn recording_cargo(directory: &Path) -> PathBuf {
    write_executable(
        &directory.join("recording-cargo"),
        &format!(
            "#!/bin/sh\necho \"$PWD $*\" >> '{calls}'\nCARGO_NET_OFFLINE=true exec '{cargo}' \"$@\"\n",
            calls = directory.join("recording-cargo.calls").display(),
            cargo = crate::config::cargo_program().display(),
        ),
    )
}

/// A configured tree whose `.cargo/config.toml` replaces crates.io with a `vendor` directory that has the dependency no registry has, so only Cargo running in the tree can read its metadata offline.
pub fn vendored_tree(directory: &Path) -> PathBuf {
    let tree = configured_tree(
        directory,
        "[source.crates-io]\nreplace-with = \"vendored\"\n\n[source.vendored]\ndirectory = \"vendor\"\n",
    );
    let vendored = tree.join("vendor/base-benchmark-absent-fixture");
    std::fs::create_dir_all(vendored.join("src")).unwrap();
    std::fs::write(
        vendored.join("Cargo.toml"),
        "[package]\nname = \"base-benchmark-absent-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(vendored.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        vendored.join(".cargo-checksum.json"),
        "{\"files\":{},\"package\":\"0000000000000000000000000000000000000000000000000000000000000000\"}",
    )
    .unwrap();
    tree
}

/// A service role named `name`, running `program`, ready on `pattern`, stopped with SIGTERM.
pub fn service(name: &str, program: RoleProgram, arguments: &[&str], pattern: &str) -> Role {
    Role {
        name: name.to_owned(),
        kind: RoleKind::Service,
        program,
        arguments: arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        smoke_arguments: Vec::new(),
        measure_arguments: Vec::new(),
        ready: Some(ReadyLine {
            pattern: pattern.to_owned(),
            timeout_ms: 20_000,
        }),
        stop: Some(Stop::Signal {
            signal: "SIGTERM".to_owned(),
            grace_ms: 20_000,
        }),
    }
}

/// The external program `shell`, `/bin/sh`.
pub fn shell() -> RoleProgram {
    RoleProgram::External {
        name: "shell".to_owned(),
    }
}

/// A suite whose scenario `work` runs `service` as the subject in the service role `server`, ready on `ready {address}`, and the driver `client`, the external program `shell` running `driver` with the server's address and `smoke` or `measure`.
pub fn roles_suite(service_script: &Path, driver: &Path, variants: &[&str]) -> Suite {
    let mut suite = suite(service_script, variants);
    suite.scenarios[0].roles = vec![
        Role {
            name: "client".to_owned(),
            kind: RoleKind::Driver,
            program: shell(),
            arguments: vec![driver.display().to_string(), "{server.address}".to_owned()],
            smoke_arguments: vec!["smoke".to_owned()],
            measure_arguments: vec!["measure".to_owned()],
            ready: None,
            stop: None,
        },
        service("server", RoleProgram::Subject, &[], "ready {address}"),
    ];
    suite
}

/// A screening plan of one round of a suite with roles, running `/bin/sh` as the subject and as the external program `shell`.
pub fn roles_plan(suite: Suite, lock: &Path) -> Plan {
    let artifacts = manifest(vec![artifact(
        "baseline",
        ArtifactRole::Baseline,
        Path::new(SHELL),
    )]);
    let mut options = PlanOptions::new(suite, "work", artifacts, Mode::Screening);
    options.rounds = Some(1);
    options.seed = Some(7);
    options.lock = lock.to_owned();
    options.timeout = Duration::from_secs(20);
    options.programs = [("shell".to_owned(), PathBuf::from(SHELL))].into();
    Plan::new(options).unwrap()
}

/// Run the test named `test`, its path within the crate, again in a child process that starts with `signals` ignored, as `&` in a non-interactive shell leaves `SIGINT` and `SIGQUIT` and `nohup` leaves `SIGHUP`, and require that it passes there. In that child, which first checks that the signals are ignored, run `body` instead.
pub fn with_signals_ignored(test: &str, signals: &[nix::sys::signal::Signal], body: impl FnOnce()) {
    const PASSED: &str = "BASE_BENCHMARK_TEST_IGNORING_PASSED";
    if let Some(passed) = std::env::var_os(PASSED) {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let ignored = status
            .lines()
            .find_map(|line| line.strip_prefix("SigIgn:"))
            .map(|mask| u64::from_str_radix(mask.trim(), 16).unwrap())
            .unwrap();
        for &ignoring in signals {
            assert_ne!(
                ignored & (1 << (ignoring as i32 - 1)),
                0,
                "{ignoring} is ignored"
            );
        }
        body();
        std::fs::write(passed, "").unwrap();
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let passed = directory.path().join("passed");
    // A shell ignores a signal for the programs it then runs with `trap ''`, as `nohup` does with `SIGHUP`.
    let names: Vec<&str> = signals
        .iter()
        .map(|ignoring| ignoring.as_str().trim_start_matches("SIG"))
        .collect();
    let mut command = Command::new(SHELL);
    command
        .arg("-c")
        .arg(format!("trap '' {}; exec \"$0\" \"$@\"", names.join(" ")))
        .arg(std::env::current_exe().unwrap())
        .args([test, "--exact", "--test-threads=1", "--nocapture"])
        .env(PASSED, &passed);
    let output = command.output().unwrap();
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{report}");
    // A filter that matches no test also passes.
    assert!(passed.exists(), "{test} ran in the child: {report}");
}
