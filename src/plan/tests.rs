// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::test_support::{SHELL, artifact, manifest, suite};
use std::path::{Path, PathBuf};

fn options(roles: &[(&str, ArtifactRole)], variants: &[&str], mode: Mode) -> PlanOptions {
    let artifacts = manifest(
        roles
            .iter()
            .map(|(label, role)| artifact(label, *role, Path::new(SHELL)))
            .collect(),
    );
    PlanOptions::new(
        suite(Path::new("/work.sh"), variants),
        "work",
        artifacts,
        mode,
    )
}

#[test]
fn modes_parse_and_describe_their_evidence() {
    for mode in [Mode::Smoke, Mode::Screening, Mode::Recorded] {
        assert_eq!(mode.to_string().parse::<Mode>().unwrap(), mode);
        assert!(mode.evidence().starts_with(&mode.to_string()));
    }
    assert_eq!(
        [Mode::Smoke, Mode::Screening, Mode::Recorded].map(Mode::default_rounds),
        [1, 5, 10]
    );
    assert!("fast".parse::<Mode>().is_err());
    assert_eq!(
        default_lock_path(),
        PathBuf::from("/tmp/base-benchmark-host.lock")
    );
}

#[test]
fn expands_variants_builds_and_replicas_into_pairs() {
    let roles = [
        ("baseline", ArtifactRole::Baseline),
        ("candidate", ArtifactRole::Candidate),
        ("baseline-replica", ArtifactRole::LayoutReplica),
    ];
    let mut options = options(&roles, &["reference", "engine"], Mode::Smoke);
    options.decision_rule = Some(" ".to_owned());
    let plan = Plan::new(options).unwrap();
    assert_eq!(plan.rounds, 1);
    assert_eq!(plan.decision_rule, None);
    let labels: Vec<_> = plan
        .subjects
        .iter()
        .map(|subject| subject.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "baseline/reference",
            "baseline/engine",
            "candidate/reference",
            "candidate/engine",
            "baseline-replica/reference",
            "baseline-replica/engine"
        ]
    );
    assert_eq!(
        plan.subject("candidate/engine").unwrap().arguments,
        ["/work.sh", "engine", "smoke"]
    );
    assert!(plan.subject("missing").is_none());
    let pairs: Vec<_> = plan
        .pairs
        .iter()
        .map(|pair| (pair.baseline.as_str(), pair.candidate.as_str(), pair.kind))
        .collect();
    assert_eq!(
        pairs,
        [
            ("baseline/reference", "baseline/engine", PairKind::Variant),
            ("baseline/reference", "candidate/reference", PairKind::Build),
            ("baseline/engine", "candidate/engine", PairKind::Build),
            ("candidate/reference", "candidate/engine", PairKind::Variant),
            (
                "baseline/reference",
                "baseline-replica/reference",
                PairKind::Replica
            ),
            (
                "baseline/engine",
                "baseline-replica/engine",
                PairKind::Replica
            ),
            (
                "baseline-replica/reference",
                "baseline-replica/engine",
                PairKind::Variant
            ),
        ]
    );
}

#[test]
fn scenarios_without_variants_use_artifact_labels() {
    let mut options = options(
        &[("baseline", ArtifactRole::Baseline)],
        &[],
        Mode::Screening,
    );
    options.rounds = Some(3);
    let plan = Plan::new(options).unwrap();
    assert_eq!(plan.subjects[0].label, "baseline");
    assert_eq!(plan.subjects[0].arguments, ["/work.sh", "measure"]);
    assert!(plan.stack_padding);
    assert_eq!(plan.working_directory, PathBuf::from("/"));
    assert_eq!(plan.environment["LC_ALL"], "C");
    assert!(plan.pairs.is_empty());
    assert_eq!(plan.rounds, 3);
}

#[test]
fn rejects_mismatched_or_empty_plans() {
    let mut mismatched = options(&[("baseline", ArtifactRole::Baseline)], &[], Mode::Smoke);
    mismatched.artifacts.binary = "other".to_owned();
    assert!(
        Plan::new(mismatched)
            .unwrap_err()
            .to_string()
            .contains("artifacts were prepared for")
    );
    let mut zero = options(&[("baseline", ArtifactRole::Baseline)], &[], Mode::Smoke);
    zero.rounds = Some(0);
    assert!(Plan::new(zero).is_err());
    let mut instant = options(&[("baseline", ArtifactRole::Baseline)], &[], Mode::Smoke);
    instant.timeout = Duration::ZERO;
    assert!(Plan::new(instant).is_err());
    let mut unknown = options(&[("baseline", ArtifactRole::Baseline)], &[], Mode::Smoke);
    unknown.scenario = "missing".to_owned();
    assert!(Plan::new(unknown).is_err());
}

#[test]
fn recorded_campaigns_report_every_missing_prerequisite() {
    let mut options = options(
        &[
            ("baseline", ArtifactRole::Baseline),
            ("candidate", ArtifactRole::Candidate),
        ],
        &[],
        Mode::Recorded,
    );
    options.rounds = Some(5);
    options.artifacts.artifacts[0].source.dirty = Some(true);
    options.artifacts.artifacts[1].source.dirty = Some(true);
    options.artifacts.artifacts[1].source.path_dependencies = None;
    options.artifacts.artifacts[1].build.toolchain = "other".to_owned();
    options.artifacts.artifacts[1].build.configuration = vec![crate::artifact::ConfigurationFile {
        path: PathBuf::from("/.cargo/config.toml"),
        sha256: "0".repeat(64),
    }];
    let error = Plan::new(options).unwrap_err().to_string();
    for missing in [
        "at least 10 rounds",
        "explicit CPU placement",
        "decision rule",
        "idle-host",
        "unmodified source and path dependencies for baseline,",
        "unmodified source and path dependencies for candidate (Cargo metadata could not be read",
        "layout replica",
        "same toolchain",
    ] {
        assert!(error.contains(missing), "{missing}: {error}");
    }

    let roles = [
        ("baseline", ArtifactRole::Baseline),
        ("candidate", ArtifactRole::Candidate),
        ("baseline-replica", ArtifactRole::LayoutReplica),
    ];
    let mut options = self::options(&roles, &[], Mode::Recorded);
    options.cpus = vec![0];
    options.decision_rule = Some("median instructions improve beyond the replica range".to_owned());
    options.idle_host = true;
    options.seed = None;
    let plan = Plan::new(options.clone()).unwrap();
    assert_eq!(plan.rounds, 10);
    assert!(plan.seed > 0);

    // An overlaid workload must be committed too.
    for artifact in &mut options.artifacts.artifacts {
        artifact.source.overlay = Some(crate::artifact::DependencyIdentity {
            name: "workload".to_owned(),
            path: PathBuf::from("/workspace/bench"),
            revision: Some("0123".to_owned()),
            dirty: Some(true),
        });
    }
    let error = Plan::new(options).unwrap_err().to_string();
    assert!(
        error.contains(
            "unmodified source and path dependencies, and overlaid workload for baseline"
        ),
        "{error}"
    );
}

#[test]
fn shuffles_are_seeded_permutations() {
    let first = Shuffle::new(3).order(10);
    assert_eq!(first, Shuffle::new(3).order(10));
    assert_ne!(first, Shuffle::new(4).order(10));
    let mut sorted = first.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..10).collect::<Vec<_>>());
    assert!(Shuffle::new(1).order(0).is_empty());
}

#[test]
fn launches_use_a_minimal_recorded_environment() {
    let environment = launch_environment(Some("/bin".to_owned()), None);
    assert_eq!(environment["PATH"], "/bin");
    assert_eq!(environment.len(), 2);
    assert_eq!(
        launch_environment(None, None)["PATH"],
        "/usr/local/bin:/usr/bin:/bin"
    );
    let coverage = launch_environment(None, Some("/target/%p.profraw".to_owned()));
    assert_eq!(coverage["LLVM_PROFILE_FILE"], "/target/%p.profraw");
    let mut shuffle = Shuffle::new(9);
    assert!((0..100).all(|_| shuffle.below(STACK_PADDING_LIMIT) < STACK_PADDING_LIMIT));
}
