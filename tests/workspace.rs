// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

mod support;

use base_benchmark::{
    ArtifactManifest, ArtifactRole, CampaignStatus, Cancellation, Mode, PairKind, RunRequest, Tree,
    Workspace, read_metadata,
};
use std::path::Path;

fn workspace(path: &Path) -> Workspace {
    Workspace::new(support::workspace(path, ""))
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
    let output = directory.path().join("prepared");
    let prepared = workspace
        .prepare(None, trees, None, Some(output.clone()))
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
    assert_eq!(replica.source.path.parent(), Some(directory.path()));
    assert!(replica.build.toolchain.contains("rustc"));
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

    let mut request = RunRequest::new("work", &prepared.manifest_path, Mode::Screening);
    request.rounds = Some(2);
    request.seed = Some(1);
    request.lock = directory.path().join("lock");
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
    assert_eq!(variant.metrics[0].improved, 2);
    assert!(campaign.summary.warnings.iter().all(|warning| {
        warning.contains("within the layout replica's range")
            || warning.starts_with("no CPU placement")
    }));
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
            &directory.path().join("lock"),
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
    let manifest = ArtifactManifest::read(&output.join("artifacts/artifacts.json")).unwrap();
    assert_eq!(manifest.artifacts.len(), 1);
    assert_eq!(manifest.artifacts[0].label, "current");
    assert!(manifest.artifacts[0].source.revision.is_none());
}

#[test]
fn preparation_failures_name_the_tree() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = workspace(&project);
    std::fs::write(project.join("src/main.rs"), "fn main() { not rust }").unwrap();
    let error = workspace
        .prepare(
            None,
            Vec::new(),
            None,
            Some(directory.path().join("broken")),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("current: cargo build failed"),
        "{error}"
    );

    let mut missing_rustc = Workspace::new(support::workspace(&project, ""));
    missing_rustc.rustc = "/nonexistent/rustc".into();
    let error = missing_rustc
        .prepare(None, Vec::new(), None, Some(directory.path().join("rustc")))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("could not run /nonexistent/rustc"),
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
        replica: Some(true),
        output: directory.path().join("custom"),
        cargo: custom.cargo.clone(),
        rustc: custom.rustc.clone(),
    };
    let prepared = base_benchmark::prepare(&options).unwrap();
    assert!(
        prepared.manifest.artifacts[0]
            .build
            .command
            .contains(&"fast".to_owned())
    );
    assert_eq!(prepared.manifest.artifacts.len(), 2);

    let mut features = options.clone();
    features.suite.features = vec!["missing".to_owned()];
    features.output = directory.path().join("features");
    assert!(base_benchmark::prepare(&features).is_err());
}

#[test]
fn runs_reject_unknown_scenarios_and_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let request = RunRequest::new("work", directory.path().join("missing.json"), Mode::Smoke);
    assert!(
        workspace
            .run(request, &Cancellation::new(), &mut Vec::new())
            .is_err()
    );
    let none = Workspace::new(directory.path().join("missing/Cargo.toml"));
    assert!(none.suite(None).is_err());
    assert!(none.prepare(None, Vec::new(), None, None).is_err());
    assert!(
        none.smoke(
            None,
            None,
            &directory.path().join("lock"),
            &Cancellation::new(),
            &mut Vec::new()
        )
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
            &directory.path().join("lock"),
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
fn default_outputs_are_new_directories_under_the_target_directory() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = workspace(&project);
    let prepared = workspace.prepare(None, Vec::new(), None, None).unwrap();
    assert!(
        prepared
            .manifest_path
            .starts_with(project.join("target/base-benchmark"))
    );
    let mut request = RunRequest::new("work", &prepared.manifest_path, Mode::Smoke);
    request.lock = directory.path().join("lock");
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
            &directory.path().join("lock"),
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
fn smoke_reports_build_failures() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = workspace(&project);
    std::fs::write(project.join("src/main.rs"), "fn main() { broken }").unwrap();
    let error = workspace
        .smoke(
            None,
            Some(directory.path().join("smoke")),
            &directory.path().join("lock"),
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("cargo build failed"));
}
