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
    // The shell's version depends on which shell it is.
    let version = plan.programs[0].version.clone();
    assert_eq!(
        plan.programs,
        [ExternalProgram {
            name: "shell".to_owned(),
            path: shell.clone(),
            sha256: crate::artifact::sha256_file(&shell).unwrap(),
            version
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
fn roles_on_shared_physical_cores_must_be_declared_in_recorded_campaigns() {
    let recorded = |shared_placement: bool, server: Vec<usize>, topology: Topology| {
        let mut options = roles_options(Mode::Recorded);
        options.suite.scenarios[0].shared_placement = shared_placement;
        options.role_cpus = [
            ("server".to_owned(), server),
            ("client".to_owned(), vec![3]),
        ]
        .into();
        options.topology = Some(topology);
        options.rounds = Some(10);
        options.decision_rule = Some("rule".to_owned());
        options.idle_host = true;
        Plan::new(options)
    };
    let siblings = || Topology::new([(1, vec![3]), (3, vec![1]), (2, vec![])].into());
    let error = recorded(false, vec![1, 2], siblings())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("roles on separate physical cores, or a scenario that declares shared-placement (server and client on CPUs 1, 3)"),
        "{error}"
    );
    let plan = recorded(true, vec![1, 2], siblings()).unwrap();
    assert_eq!(plan.shared_cores[0].cpus, [1, 3]);
    assert!(plan.shared_placement);
    recorded(false, vec![2], siblings()).unwrap();
    let error = recorded(false, vec![2], Topology::new([(3, vec![])].into()))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("the SMT siblings of CPUs 2, to rule out roles sharing a physical core"),
        "{error}"
    );
    // A scenario that declares shared placement need not rule it out.
    let plan = recorded(true, vec![2], Topology::new([(3, vec![])].into())).unwrap();
    assert_eq!(plan.unknown_topology, [2]);
    // Screening campaigns are planned, and warned about in their summaries.
    let mut screening = roles_options(Mode::Screening);
    screening.role_cpus = [
        ("server".to_owned(), vec![1]),
        ("client".to_owned(), vec![1]),
    ]
    .into();
    screening.topology = Some(Topology::default());
    let plan = Plan::new(screening).unwrap();
    assert_eq!(
        (plan.shared_cores.len(), plan.unknown_topology.as_slice()),
        (1, &[1][..])
    );
}

#[test]
fn roles_must_be_placed_on_cpus_that_exist_and_are_online() {
    let directory = tempfile::tempdir().unwrap();
    let cpus = directory.path().join("sys/devices/system/cpu");
    for cpu in 0..3 {
        let topology = cpus.join(format!("cpu{cpu}/topology"));
        std::fs::create_dir_all(&topology).unwrap();
        std::fs::write(topology.join("thread_siblings_list"), format!("{cpu}\n")).unwrap();
    }
    std::fs::write(cpus.join("present"), "0-3\n").unwrap();
    std::fs::write(cpus.join("online"), "0-2\n").unwrap();
    let plan = |client: Vec<usize>| {
        let mut options = roles_options(Mode::Screening);
        options.role_cpus = [
            ("server".to_owned(), vec![1]),
            ("client".to_owned(), client),
        ]
        .into();
        options.topology = Some(Topology::read_from(directory.path(), &[0, 1, 2, 3, 9]));
        Plan::new(options)
    };
    plan(vec![2]).unwrap();
    let error = plan(vec![3, 9, 2]).unwrap_err().to_string();
    assert!(
        error.contains("roles are placed on CPUs this host cannot run them on: CPU 3 is offline, CPU 9 does not exist"),
        "{error}"
    );
}

#[test]
fn external_programs_report_their_versions() {
    let directory = tempfile::tempdir().unwrap();
    for (index, (body, version)) in [
        ("echo; echo '  tool 1.2.3 '; echo more", Some("tool 1.2.3")),
        ("echo 'v2' >&2", Some("v2")),
        ("echo 'tool 1.2.3'; exit 1", None),
        (":", None),
    ]
    .into_iter()
    .enumerate()
    {
        let program = crate::test_support::write_executable(
            &directory.path().join(format!("program-{index}")),
            &format!("#!/bin/sh\n[ \"$1\" = --version ] || exit 5\n{body}\n"),
        );
        let identified = ExternalProgram::identify("tool", &program).unwrap();
        assert_eq!(identified.version.as_deref(), version, "{body}");
    }
}

#[test]
fn helper_roles_need_prepared_helpers() {
    let mut options = roles_options(Mode::Screening);
    options.suite.scenarios[0].roles[0].program = crate::role::RoleProgram::Binary {
        package: "fixture".to_owned(),
        binary: "load".to_owned(),
        features: Vec::new(),
    };
    let error = Plan::new(options.clone()).unwrap_err().to_string();
    assert!(
        error.contains("role \"client\" runs fixture's binary load with features [], which these artifacts do not include"),
        "{error}"
    );
    let mut helper = artifact("load", ArtifactRole::Helper, Path::new(SHELL));
    helper.features = Some(Vec::new());
    options.artifacts.helpers = vec![crate::artifact::HelperArtifact {
        package: "fixture".to_owned(),
        binary: "load".to_owned(),
        artifact: helper,
    }];
    let plan = Plan::new(options).unwrap();
    assert_eq!(plan.artifacts.helper_index("fixture", "load", &[]), Some(0));
    assert_eq!(
        plan.artifacts
            .helper_index("fixture", "load", &["x".to_owned()]),
        None
    );
    assert!(plan.programs.is_empty());
}
