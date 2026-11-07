// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#![cfg(feature = "runner")]

mod support;

use base_benchmark::{
    ArtifactManifest, ArtifactRole, CampaignStatus, Cancellation, Mode, PairKind, PrepareRequest,
    RunRequest, Tree, Workspace, read_metadata,
};
use std::path::Path;

fn workspace(path: &Path) -> Workspace {
    Workspace::new(support::workspace(path, ""))
}

fn request(artifacts: &Path, lock: &Path) -> RunRequest {
    RunRequest {
        rounds: Some(2),
        seed: Some(1),
        lock: Some(lock.to_owned()),
        ..RunRequest::new("work", artifacts)
    }
}

#[test]
fn prepares_trees_with_a_layout_replica_and_compares_them() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = directory.path().join("baseline");
    let candidate = directory.path().join("candidate");
    let workspace = workspace(&baseline);
    support::workspace(&candidate, "");
    let suites = workspace.suites().unwrap();
    assert_eq!(suites[0].scenarios[0].variants.len(), 2);
    assert_eq!(
        suites[0].scenarios[0].baseline.as_deref(),
        Some("reference"),
        "listings show the resolved baseline"
    );

    let trees = vec![
        Tree {
            label: "base".to_owned(),
            path: baseline.clone(),
        },
        Tree {
            label: "change".to_owned(),
            path: candidate,
        },
    ];
    let prepared = workspace
        .prepare(
            PrepareRequest {
                trees,
                output: Some(directory.path().join("prepared")),
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    let roles: Vec<_> = prepared
        .manifest
        .artifacts
        .iter()
        .map(|artifact| (artifact.label.as_str(), artifact.role))
        .collect();
    assert_eq!(
        roles,
        [
            ("base", ArtifactRole::Baseline),
            ("change", ArtifactRole::Candidate),
            ("base-replica", ArtifactRole::LayoutReplica)
        ]
    );
    let replica = &prepared.manifest.artifacts[2];
    assert_eq!(replica.source.replica_of.as_deref(), Some("base"));
    assert!(
        !replica.source.path.exists(),
        "the replica source copy is removed after building"
    );
    assert_eq!(
        replica.source.path.parent(),
        Some(directory.path().canonicalize().unwrap().as_path())
    );
    assert!(replica.build.toolchain.contains("rustc") && replica.build.cargo.contains("cargo"));
    assert!(
        prepared.manifest.artifacts[0]
            .source
            .lockfile_sha256
            .is_some()
    );
    assert_eq!(
        ArtifactManifest::read(&prepared.manifest_path).unwrap(),
        prepared.manifest
    );

    let mut request = request(&prepared.manifest_path, &directory.path().join("lock"));
    request.output = Some(directory.path().join("campaign"));
    let campaign = workspace
        .run(request, &Cancellation::new(), &mut Vec::new())
        .unwrap();
    assert_eq!(campaign.summary.subjects.len(), 6);
    assert!(
        campaign
            .summary
            .pairs
            .iter()
            .any(|pair| pair.kind == PairKind::Replica)
    );
    let variant = campaign
        .summary
        .pairs
        .iter()
        .find(|pair| pair.kind == PairKind::Variant)
        .unwrap();
    assert_eq!(
        variant
            .metrics
            .iter()
            .find(|metric| metric.name == "wall_time")
            .unwrap()
            .improved,
        2
    );
    assert_eq!(
        read_metadata(&campaign.output).unwrap().status,
        CampaignStatus::Complete
    );
}

#[test]
fn smoke_builds_the_current_workspace_and_runs_every_scenario() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let output = directory.path().join("smoke");
    let campaigns = workspace
        .smoke(
            Some("fixture"),
            Some(output.clone()),
            Some(directory.path().join("lock")),
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(campaigns.len(), 1);
    assert!(
        campaigns[0]
            .report
            .starts_with("smoke: correctness and lifecycle only")
    );
    assert!(campaigns[0].summary.pairs.is_empty());
    let manifest = ArtifactManifest::read(&output.join(".artifacts/artifacts.json")).unwrap();
    assert_eq!(manifest.artifacts.len(), 1);
    assert_eq!(manifest.artifacts[0].label, "current");
    assert!(manifest.artifacts[0].source.revision.is_none());
}

#[test]
fn smoke_runs_an_unprepared_executable() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = workspace(&project);
    let prepared = workspace
        .prepare(
            PrepareRequest {
                output: Some(directory.path().join("prepared")),
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    let executable = &prepared.manifest.artifacts[0].executable;
    let campaigns = workspace
        .smoke_executable(
            None,
            executable,
            &[],
            &directory.path().join("smoke"),
            Some(directory.path().join("lock")),
        )
        .unwrap();
    assert_eq!(campaigns.len(), 1);
    assert!(
        workspace
            .smoke_executable(
                None,
                &directory.path().join("missing"),
                &[],
                &directory.path().join("missing-smoke"),
                None
            )
            .is_err()
    );
    assert!(
        workspace
            .smoke_executable(
                Some("missing"),
                executable,
                &[],
                &directory.path().join("other"),
                None
            )
            .is_err()
    );
}

#[test]
fn preparation_failures_name_the_tree() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = workspace(&project);
    std::fs::write(project.join("src/main.rs"), "fn main() { not rust }").unwrap();
    let error = workspace
        .prepare(
            PrepareRequest {
                output: Some(directory.path().join("broken")),
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("current: cargo build failed"),
        "{error}"
    );

    let custom = Workspace::new(support::workspace(
        &project,
        "\n[profile.fast]\ninherits = \"release\"\n",
    ));
    let mut suites = custom.suites().unwrap();
    suites[0].profile = "fast".to_owned();
    let options = base_benchmark::PrepareOptions {
        suite: suites.remove(0),
        trees: vec![Tree {
            label: "current".to_owned(),
            path: project.clone(),
        }],
        replicas: Some(1),
        output: directory.path().join("custom"),
        feature_sets: Vec::new(),
        cargo: custom.cargo.clone(),
        rustc: custom.rustc.clone(),
    };
    let prepared = base_benchmark::prepare(&options, &Cancellation::new()).unwrap();
    assert!(
        prepared.manifest.artifacts[0]
            .build
            .command
            .contains(&"fast".to_owned())
    );
    assert_eq!(prepared.manifest.artifacts.len(), 2);

    let mut features = options.clone();
    features.feature_sets = vec![vec!["missing".to_owned()]];
    features.output = directory.path().join("features");
    assert!(base_benchmark::prepare(&features, &Cancellation::new()).is_err());
}

#[test]
fn runs_reject_unknown_scenarios_and_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    assert!(
        workspace
            .run(
                RunRequest::new("work", directory.path().join("missing.json")),
                &Cancellation::new(),
                &mut Vec::new()
            )
            .is_err()
    );
    let none = Workspace::new(directory.path().join("missing/Cargo.toml"));
    assert!(none.suite(None).is_err());
    assert!(
        none.prepare(
            PrepareRequest {
                ..PrepareRequest::default()
            },
            &Cancellation::new()
        )
        .is_err()
    );
    assert!(
        none.smoke(None, None, None, &Cancellation::new(), &mut Vec::new())
            .is_err()
    );
}

#[test]
fn smoke_failures_stop_the_command() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = Workspace::new(support::workspace(
        &project,
        "\n[[package.metadata.base-benchmark.scenario]]\nname = \"broken\"\narguments = [\"fail\"]\n",
    ));
    let output = directory.path().join("smoke");
    let error = workspace
        .smoke(
            None,
            Some(output.clone()),
            Some(directory.path().join("lock")),
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("exit code Some(3)"), "{error}");
    assert_eq!(
        read_metadata(&output.join("broken")).unwrap().status,
        CampaignStatus::Failed
    );
}

#[test]
fn smoke_reports_build_failures() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = workspace(&project);
    std::fs::write(project.join("src/main.rs"), "fn main() { broken }").unwrap();
    let error = workspace
        .smoke(
            None,
            Some(directory.path().join("smoke")),
            Some(directory.path().join("lock")),
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("cargo build failed"));
}

#[test]
fn default_outputs_are_new_directories_under_the_target_directory() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = workspace(&project);
    let prepared = workspace
        .prepare(
            PrepareRequest {
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    assert!(
        prepared
            .manifest_path
            .starts_with(project.join("target/base-benchmark"))
    );
    let mut request = RunRequest {
        mode: Some(Mode::Smoke),
        ..request(&prepared.manifest_path, &directory.path().join("lock"))
    };
    request.rounds = None;
    let campaign = workspace
        .run(request.clone(), &Cancellation::new(), &mut Vec::new())
        .unwrap();
    assert!(
        campaign
            .output
            .starts_with(project.join("target/base-benchmark"))
    );
    let campaigns = workspace
        .smoke(
            None,
            None,
            Some(directory.path().join("lock")),
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap();
    assert!(
        campaigns[0]
            .output
            .starts_with(project.join("target/base-benchmark"))
    );

    request.scenario = "missing".to_owned();
    assert!(
        workspace
            .run(request.clone(), &Cancellation::new(), &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("no scenario")
    );
    request.package = Some("missing".to_owned());
    assert!(
        workspace
            .run(request, &Cancellation::new(), &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("no benchmark suite")
    );
}

#[test]
fn measure_arguments_are_used_outside_smoke_runs() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    // `fail` is a measurement-only argument, so smoke runs succeed and screening runs see it.
    let workspace = Workspace::new(support::workspace(
        &project,
        "\n[[package.metadata.base-benchmark.scenario]]\nname = \"measured\"\nmeasure-arguments = [\"fail\"]\n",
    ));
    let prepared = workspace
        .prepare(
            PrepareRequest {
                output: Some(directory.path().join("prepared")),
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    let smoke = RunRequest {
        mode: Some(Mode::Smoke),
        output: Some(directory.path().join("smoke")),
        ..RunRequest {
            scenario: "measured".to_owned(),
            ..request(&prepared.manifest_path, &directory.path().join("lock"))
        }
    };
    workspace
        .run(smoke, &Cancellation::new(), &mut Vec::new())
        .unwrap();
    let screening = RunRequest {
        output: Some(directory.path().join("screening")),
        ..RunRequest {
            scenario: "measured".to_owned(),
            ..request(&prepared.manifest_path, &directory.path().join("lock"))
        }
    };
    assert!(
        workspace
            .run(screening, &Cancellation::new(), &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("exit code Some(3)")
    );
}

#[test]
fn scenarios_run_the_builds_made_with_their_features() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    // `needs-extra` fails unless the workload was built with the `extra` feature.
    let workspace = Workspace::new(support::workspace(
        &project,
        "\n[[package.metadata.base-benchmark.scenario]]\nname = \"featured\"\narguments = [\"needs-extra\"]\nfeatures = [\"extra\"]\n",
    ));
    let lock = directory.path().join("lock");
    let prepared = workspace
        .prepare(
            PrepareRequest {
                replicas: Some(1),
                output: Some(directory.path().join("prepared")),
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    let builds: Vec<_> = prepared
        .manifest
        .artifacts
        .iter()
        .map(|artifact| (artifact.label.as_str(), artifact.features.clone().unwrap()))
        .collect();
    assert_eq!(
        builds,
        [
            ("current", Vec::new()),
            ("current", vec!["extra".to_owned()]),
            ("current-replica", Vec::new()),
            ("current-replica", vec!["extra".to_owned()]),
        ]
    );
    assert!(
        prepared.manifest.artifacts[1]
            .build
            .command
            .ends_with(&["--features".to_owned(), "extra".to_owned()])
    );
    assert!(
        !directory
            .path()
            .join("prepared/targets")
            .read_dir()
            .unwrap()
            .any(|_| true),
        "each tree's shared target directory is removed"
    );
    for scenario in ["work", "featured"] {
        let request = RunRequest {
            scenario: scenario.to_owned(),
            output: Some(directory.path().join(scenario)),
            ..request(&prepared.manifest_path, &lock)
        };
        let campaign = workspace
            .run(request, &Cancellation::new(), &mut Vec::new())
            .unwrap();
        assert!(
            campaign
                .summary
                .pairs
                .iter()
                .any(|pair| pair.kind == PairKind::Replica)
        );
    }

    // Preparing only `work` builds only its features, so `featured` cannot run.
    let plain = workspace
        .prepare(
            PrepareRequest {
                output: Some(directory.path().join("plain")),
                scenarios: vec!["work".to_owned()],
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(plain.manifest.artifacts.len(), 1);
    let error = workspace
        .run(
            RunRequest {
                scenario: "featured".to_owned(),
                ..request(&plain.manifest_path, &lock)
            },
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no artifact was built with features [\"extra\"]"),
        "{error}"
    );
    assert!(
        workspace
            .prepare(
                PrepareRequest {
                    scenarios: vec!["missing".to_owned()],
                    ..PrepareRequest::default()
                },
                &Cancellation::new(),
            )
            .unwrap_err()
            .to_string()
            .contains("no scenario")
    );

    // An unprepared executable runs only the scenarios whose features it was built with.
    let plain_executable = &plain.manifest.artifacts[0].executable;
    let campaigns = workspace
        .smoke_executable(
            None,
            plain_executable,
            &[],
            &directory.path().join("smoke-plain"),
            Some(lock.clone()),
        )
        .unwrap();
    assert_eq!(campaigns.len(), 1);
    let extra_executable = &prepared.manifest.artifacts[1].executable;
    let campaigns = workspace
        .smoke_executable(
            None,
            extra_executable,
            &["extra"],
            &directory.path().join("smoke-extra"),
            Some(lock),
        )
        .unwrap();
    assert_eq!(campaigns.len(), 2);
}
