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

    // An overlaid build that changed what its tree locks is not the tree as locked.
    let mut changed = options.clone();
    changed.artifacts.artifacts[1].source.lockfile_changes =
        vec!["signal 0.4.5 -> 0.4.6".to_owned()];
    let error = Plan::new(changed).unwrap_err().to_string();
    assert!(
        error.contains("an overlaid build of candidate that keeps the versions its tree locks (signal 0.4.5 -> 0.4.6)"),
        "{error}"
    );

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

fn roles_options(mode: Mode) -> PlanOptions {
    let artifacts = manifest(vec![artifact(
        "baseline",
        ArtifactRole::Baseline,
        Path::new(SHELL),
    )]);
    let suite = crate::test_support::roles_suite(
        Path::new("/server.sh"),
        Path::new("/client.sh"),
        &["base", "fast"],
    );
    let mut options = PlanOptions::new(suite, "work", artifacts, mode);
    options.programs = [("shell".to_owned(), PathBuf::from(SHELL))].into();
    options
}

#[test]
fn roles_start_services_first_and_variants_change_only_the_subject() {
    let mut options = roles_options(Mode::Smoke);
    options.role_cpus = [("server".to_owned(), vec![1, 2])].into();
    let plan = Plan::new(options).unwrap();
    let roles: Vec<_> = plan
        .roles
        .iter()
        .map(|role| (role.name.as_str(), role.cpus.clone()))
        .collect();
    assert_eq!(roles, [("server", vec![1, 2]), ("client", Vec::new())]);
    assert_eq!(plan.unplaced_roles(), ["client"]);
    assert_eq!(plan.placed_cpus(), [1, 2]);
    // The driver takes its smoke arguments; the subject's role takes each subject's.
    assert_eq!(
        plan.roles[1].arguments,
        ["/client.sh", "{server.address}", "smoke"]
    );
    assert!(plan.roles[0].arguments.is_empty());
    // The plan records how the driver is told to mark its window.
    assert!(plan.roles[0].environment.is_empty());
    assert_eq!(
        plan.roles[1].environment,
        BTreeMap::from([("BASE_BENCHMARK_CONTROL".to_owned(), "stdio".to_owned())])
    );
    let arguments: Vec<_> = plan
        .subjects
        .iter()
        .map(|subject| subject.arguments.clone())
        .collect();
    assert_eq!(
        arguments,
        [
            ["/server.sh", "base", "smoke"],
            ["/server.sh", "fast", "smoke"]
        ]
    );
    let shell = Path::new(SHELL).canonicalize().unwrap();
    assert_eq!(
        plan.programs,
        [ExternalProgram {
            name: "shell".to_owned(),
            path: shell.clone(),
            sha256: crate::artifact::sha256_file(&shell).unwrap()
        }]
    );
    plan.programs[0].verify().unwrap();
    // Plans without roles serialize as before.
    let single = Plan::new(options_single()).unwrap();
    let value = serde_json::to_value(&single).unwrap();
    assert!(value.get("roles").is_none() && value.get("programs").is_none());
    assert_eq!(
        serde_json::from_value::<Plan>(serde_json::to_value(&plan).unwrap()).unwrap(),
        plan
    );

    let measured = Plan::new(roles_options(Mode::Screening)).unwrap();
    assert_eq!(
        measured.roles[1].arguments,
        ["/client.sh", "{server.address}", "measure"]
    );
}

fn options_single() -> PlanOptions {
    options(&[("baseline", ArtifactRole::Baseline)], &[], Mode::Smoke)
}

#[test]
fn roles_need_their_programs_and_their_own_placement() {
    type Change = fn(&mut PlanOptions);
    let cases: [(Change, &str); 6] = [
        (
            |options| options.cpus = vec![1],
            "place each with --role-cpus",
        ),
        (
            |options| options.perf_events = vec!["cycles".to_owned()],
            "cannot be counted with perf stat",
        ),
        (
            |options| {
                options.role_cpus.insert("missing".to_owned(), vec![1]);
            },
            "has no role \"missing\"",
        ),
        (
            |options| options.programs.clear(),
            "give its path with --program shell=PATH",
        ),
        (
            |options| {
                options
                    .programs
                    .insert("shell".to_owned(), PathBuf::from("/nonexistent/shell"));
            },
            "external program shell, /nonexistent/shell",
        ),
        (
            |options| options.mode = Mode::Recorded,
            "explicit CPU placement for every role (server, client has none)",
        ),
    ];
    for (change, message) in cases {
        let mut options = roles_options(Mode::Screening);
        change(&mut options);
        let error = Plan::new(options).unwrap_err().to_string();
        assert!(error.contains(message), "{message}: {error}");
    }
    let mut single = options_single();
    single.role_cpus.insert("server".to_owned(), vec![1]);
    assert!(
        Plan::new(single)
            .unwrap_err()
            .to_string()
            .contains("has no roles")
    );
    // An external program that is a directory cannot be hashed.
    assert!(
        ExternalProgram::identify("shell", Path::new("/"))
            .unwrap_err()
            .to_string()
            .contains("external program shell")
    );
}

#[test]
fn changed_external_programs_fail_verification() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("program");
    std::fs::write(&path, "before").unwrap();
    let program = ExternalProgram::identify("tool", &path).unwrap();
    assert_eq!(program.path, path.canonicalize().unwrap());
    std::fs::write(&path, "after, longer").unwrap();
    let error = program.verify().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("external program tool changed during the campaign"),
        "{error}"
    );
    std::fs::remove_file(&path).unwrap();
    assert!(
        program
            .verify()
            .unwrap_err()
            .to_string()
            .contains("external program tool:")
    );
}

#[test]
fn recorded_campaigns_refuse_roles_that_share_a_cpu() {
    let recorded = |server: &[usize], client: &[usize]| {
        let mut options = roles_options(Mode::Recorded);
        options
            .role_cpus
            .insert("server".to_owned(), server.to_vec());
        options
            .role_cpus
            .insert("client".to_owned(), client.to_vec());
        Plan::new(options).unwrap_err().to_string()
    };
    let error = recorded(&[1, 2, 3], &[0, 2, 3]);
    assert!(
        error.contains("roles on separate CPUs (server and client share CPU 2, 3)"),
        "{error}"
    );
    // Other prerequisites are unmet too; separate CPUs meet this one.
    assert!(!recorded(&[1, 2], &[3]).contains("separate CPUs"));
    // Screening allows it.
    let mut options = roles_options(Mode::Screening);
    options.role_cpus.insert("server".to_owned(), vec![0]);
    options.role_cpus.insert("client".to_owned(), vec![0]);
    let plan = Plan::new(options).unwrap();
    assert_eq!(plan.shared_cpus(), ["server and client share CPU 0"]);
}
