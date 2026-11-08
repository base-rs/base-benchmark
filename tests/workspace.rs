// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#![cfg(feature = "runner")]

mod support;

use base_benchmark::{
    ArtifactManifest, ArtifactRole, CampaignStatus, Cancellation, Mode, PairKind, PrepareRequest,
    RunRequest, Tree, Workspace, read_metadata,
};
use std::path::Path;

/// The record the fixture's driver prints.
const RECORD: &str = r#"{"schema":"base-benchmark/process-record","version":2,"workload":"client","parameters":{},"samples":[{"operations":4,"metrics":[{"name":"wall_time","unit":"nanoseconds","phase":"measured","source":"wall","direction":"lower_is_better","measurement":{"kind":"total","value":400}}]}]}"#;

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
    // Rebuilding at another path alone gives the same executable; the salted replica differs.
    assert_eq!(
        replica.build.layout_salt.as_deref(),
        Some("base-benchmark-base-replica")
    );
    assert_ne!(replica.sha256, prepared.manifest.artifacts[0].sha256);
    assert!(prepared.manifest.artifacts[0].build.layout_salt.is_none());
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
        overlay: None,
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

#[test]
fn smoking_an_executable_that_can_run_no_scenario_fails() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let workspace = Workspace::new(support::workspace(
        &project,
        "\n[package.metadata.base-benchmark]\nfeatures = [\"extra\"]\n",
    ));
    let executable = directory.path().join("tool");
    std::fs::write(&executable, "").unwrap();
    let error = workspace
        .smoke_executable(
            None,
            &executable,
            &[],
            &directory.path().join("smoke"),
            None,
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no scenario of fixture can run with features []"),
        "{error}"
    );
}

#[test]
fn overlays_build_every_tree_with_the_current_workload_package() {
    let directory = tempfile::tempdir().unwrap();
    let old = directory.path().join("old");
    let current = directory.path().join("current");
    // The old tree's workload reports 2000 ns; the current one reports 1000 ns and adds a dependency the old lockfile lacks.
    support::member_workspace(&old, 2000, false);
    // The old tree also locks a package nothing uses, which an unlocked build drops: no change to what is measured.
    let mut lockfile = std::fs::read_to_string(old.join("Cargo.lock")).unwrap();
    lockfile.push_str("\n[[package]]\nname = \"stale\"\nversion = \"0.1.0\"\n");
    std::fs::write(old.join("Cargo.lock"), lockfile).unwrap();
    let workspace = Workspace::new(support::member_workspace(&current, 1000, true));
    support::commit(&current);
    let old_lockfile = std::fs::read(old.join("Cargo.lock")).unwrap();
    let trees = vec![
        Tree {
            label: "old".to_owned(),
            path: old.clone(),
        },
        Tree {
            label: "new".to_owned(),
            path: current.clone(),
        },
    ];
    let wall = |manifest: &Path, output: &str| {
        let campaign = workspace
            .run(
                RunRequest {
                    output: Some(directory.path().join(output)),
                    ..request(manifest, &directory.path().join("lock"))
                },
                &Cancellation::new(),
                &mut Vec::new(),
            )
            .unwrap();
        campaign
            .summary
            .subjects
            .iter()
            .map(|subject| {
                let wall = subject
                    .metrics
                    .iter()
                    .find(|metric| metric.name == "wall_time");
                (
                    subject.label.clone(),
                    wall.unwrap().distribution.unwrap().median,
                )
            })
            .collect::<Vec<_>>()
    };

    let overlaid = workspace
        .prepare(
            PrepareRequest {
                trees: trees.clone(),
                output: Some(directory.path().join("overlaid")),
                overlay: true,
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        wall(&overlaid.manifest_path, "overlaid-campaign"),
        [
            ("old".to_owned(), 100.0),
            ("new".to_owned(), 100.0),
            ("old-replica".to_owned(), 100.0)
        ]
    );
    let old_artifact = &overlaid.manifest.artifacts[0];
    assert_eq!(old_artifact.source.path, old.canonicalize().unwrap());
    let harness = old_artifact.source.overlay.as_ref().unwrap();
    assert_eq!(harness.path, current.join("bench").canonicalize().unwrap());
    assert_eq!(harness.dirty, Some(false));
    assert!(harness.revision.is_some());
    assert!(!old_artifact.build.command.contains(&"--locked".to_owned()));
    assert!(old_artifact.source.lockfile_sha256.is_some());
    assert!(old_artifact.source.lockfile_changes.is_empty());
    assert!(
        overlaid.manifest.artifacts[1]
            .source
            .lockfile_changes
            .is_empty(),
        "the current tree's harness dependencies come from its own lockfile"
    );
    assert_eq!(
        overlaid.manifest.artifacts[2].source.overlay.as_ref(),
        Some(harness),
        "replicas repeat their baseline's overlay"
    );
    // The trees themselves are never modified.
    assert_eq!(std::fs::read(old.join("Cargo.lock")).unwrap(), old_lockfile);
    assert!(!old.join("bench/helper").exists());
    let leftovers = std::fs::read_dir(directory.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
        .count();
    assert_eq!(leftovers, 0, "overlaid copies are removed");

    // Without an overlay each tree runs its own workload.
    let own = workspace
        .prepare(
            PrepareRequest {
                trees,
                output: Some(directory.path().join("own")),
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        wall(&own.manifest_path, "own-campaign")[0],
        ("old".to_owned(), 200.0)
    );
    assert!(own.manifest.artifacts[0].source.overlay.is_none());
}

#[test]
fn overlays_need_a_workload_below_the_workspace_root() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let error = workspace
        .prepare(
            PrepareRequest {
                overlay: true,
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot be overlaid onto other trees"),
        "{error}"
    );
}

#[test]
fn run_outputs_are_made_absolute() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let prepared = workspace
        .prepare(
            PrepareRequest {
                output: Some(directory.path().join("prepared")),
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    // Workloads start in the workspace root, so a relative output must not be resolved there.
    let relative = Path::new("target").join(format!("relative-campaign-{}", std::process::id()));
    // Removed even when an assertion fails, since it is inside the package rather than a temporary directory.
    struct Removed<'a>(&'a Path);
    impl Drop for Removed<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0);
        }
    }
    let _removed = Removed(&relative);
    let campaign = workspace
        .run(
            RunRequest {
                mode: Some(Mode::Smoke),
                output: Some(relative.clone()),
                ..request(&prepared.manifest_path, &directory.path().join("lock"))
            },
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap();
    assert!(campaign.output.is_absolute());
    let error = workspace
        .run(
            RunRequest {
                output: Some(std::path::PathBuf::new()),
                ..request(&prepared.manifest_path, &directory.path().join("lock"))
            },
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
}

#[test]
fn scenarios_with_roles_run_the_subject_as_a_service_under_a_driver() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let addresses = directory.path().join("addresses");
    let driver = directory.path().join("driver.sh");
    std::fs::write(
        &driver,
        format!(
            "echo \"$*\" >> '{}'\necho '{}'\necho '{}'\necho '{RECORD}'\n",
            addresses.display(),
            base_benchmark::WINDOW_START,
            base_benchmark::WINDOW_STOP
        ),
    )
    .unwrap();
    let scenario = format!(
        "\n[[package.metadata.base-benchmark.scenario]]\nname = \"hello\"\narguments = [\"serve\"]\nvariants = [{{ name = \"reference\" }}, {{ name = \"fast\", arguments = [\"fast\"] }}]\n\n[[package.metadata.base-benchmark.scenario.role]]\nname = \"server\"\nkind = \"service\"\nsubject = true\nready = \"listening on {{address}}\"\n\n[[package.metadata.base-benchmark.scenario.role]]\nname = \"client\"\nkind = \"driver\"\nexternal = \"shell\"\narguments = [\"{}\", \"{{server.address}}\"]\nsmoke-arguments = [\"smoke\"]\nmeasure-arguments = [\"measure\"]\n",
        driver.display()
    );
    let workspace = Workspace::new(support::workspace(&project, &scenario));
    let lock = directory.path().join("lock");
    // Smoke runs every scenario, so they need the driver's program too.
    let error = workspace
        .smoke(
            None,
            Some(directory.path().join("unprovided")),
            Some(lock.clone()),
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("give its path with --program shell=PATH"),
        "{error}"
    );
    let workspace = workspace.with_program("shell", "/bin/sh");
    let campaigns = workspace
        .smoke(
            None,
            Some(directory.path().join("smoke")),
            Some(lock.clone()),
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(campaigns.len(), 2);

    let prepared = workspace
        .prepare(
            PrepareRequest {
                output: Some(directory.path().join("prepared")),
                ..PrepareRequest::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    let request = RunRequest {
        scenario: "hello".to_owned(),
        output: Some(directory.path().join("campaign")),
        role_cpus: [("client".to_owned(), vec![0])].into(),
        ..request(&prepared.manifest_path, &lock)
    };
    let campaign = workspace
        .run(request, &Cancellation::new(), &mut Vec::new())
        .unwrap();
    // The driver's record is each subject's sample, and the variant reaches only the subject.
    let subjects: Vec<_> = campaign
        .summary
        .subjects
        .iter()
        .map(|subject| (subject.label.as_str(), subject.processes))
        .collect();
    assert_eq!(subjects, [("current/reference", 2), ("current/fast", 2)]);
    assert!(
        campaign
            .warnings
            .iter()
            .any(|warning| warning.contains("no CPU placement for role(s) server")),
        "{:?}",
        campaign.warnings
    );
    let entries = base_benchmark::read_entries(&campaign.output.join("samples.jsonl")).unwrap();
    let addresses = std::fs::read_to_string(&addresses).unwrap();
    for entry in &entries {
        let server = &entry.roles[0];
        let address = &server.captures["address"];
        assert!(address.starts_with("127.0.0.1:"), "{address}");
        assert_eq!(
            server.signal,
            Some(15),
            "the service died from its stop signal"
        );
        assert_eq!(entry.roles[1].cpus, [0]);
        assert!(
            addresses.contains(&format!("{address} measure\n")),
            "{addresses}"
        );
        // The fast variant's ready line carries its argument, which the pattern's single capture takes too.
        assert_eq!(entry.subject.ends_with("fast"), address.ends_with(" fast"));
    }
}
