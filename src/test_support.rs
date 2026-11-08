// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Fixtures for unit tests: shell workloads and hand-built plans.
use crate::artifact::{
    Artifact, ArtifactManifest, ArtifactRole, BuildIdentity, MANIFEST_SCHEMA, MANIFEST_VERSION,
    SourceIdentity, sha256_file,
};
use crate::config::{Scenario, Suite, Variant};
use crate::plan::{Mode, Plan, PlanOptions};
use std::path::{Path, PathBuf};
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
