// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::artifact::{ArtifactManifest, ArtifactRole, sha256_identified};
use crate::config::{Suite, invalid_input};
use crate::placement::{SharedCore, Topology, describe, shared_cores};
use crate::role::{ReadyLine, RoleKind, RoleProgram, Stop};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

pub const PLAN_SCHEMA: &str = "base-benchmark/plan";
/// Version 2 embeds a version 2 artifact manifest, with features per artifact. Version 3 adds the roles and external programs of multi-process scenarios, which plans of single-process scenarios omit.
pub const PLAN_VERSION: u32 = 3;

/// How results may be used. The mode is recorded with every campaign.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Correctness and lifecycle checks with tiny workloads. Not performance evidence.
    Smoke,
    /// Quick paired comparison with controls, for triage. Not acceptance evidence.
    Screening,
    /// Frozen builds, declared placement, enough rounds, and a predeclared decision rule.
    Recorded,
}

impl Mode {
    pub fn default_rounds(self) -> u32 {
        match self {
            Self::Smoke => 1,
            Self::Screening => 5,
            Self::Recorded => 10,
        }
    }

    /// The evidence classification written into every summary.
    pub fn evidence(self) -> &'static str {
        match self {
            Self::Smoke => "smoke: correctness and lifecycle only; not performance evidence",
            Self::Screening => "screening: paired triage with controls; not acceptance evidence",
            Self::Recorded => {
                "recorded: declared prerequisites were met; interpret only with the predeclared decision rule"
            }
        }
    }
}

impl FromStr for Mode {
    type Err = io::Error;

    fn from_str(value: &str) -> io::Result<Self> {
        match value {
            "smoke" => Ok(Self::Smoke),
            "screening" => Ok(Self::Screening),
            "recorded" => Ok(Self::Recorded),
            _ => Err(invalid_input(format!(
                "invalid mode {value:?}; expected smoke, screening, or recorded"
            ))),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Smoke => "smoke",
            Self::Screening => "screening",
            Self::Recorded => "recorded",
        })
    }
}

/// What to run. [`Plan::new`] expands it into subjects and comparison pairs.
#[derive(Clone, Debug)]
pub struct PlanOptions {
    pub suite: Suite,
    pub scenario: String,
    pub artifacts: ArtifactManifest,
    pub mode: Mode,
    pub rounds: Option<u32>,
    pub seed: Option<u64>,
    /// CPUs every workload process of a single-process scenario is pinned to. Required for recorded runs.
    pub cpus: Vec<usize>,
    /// CPUs each role of a multi-process scenario is pinned to, by role name. Recorded runs require a set for every role.
    pub role_cpus: BTreeMap<String, Vec<usize>>,
    /// External programs by name, as paths on the measuring host; see [`ExternalProgram`].
    pub programs: BTreeMap<String, PathBuf>,
    /// Which CPUs share physical cores, to check roles' placement. Defaults to this host's, as it reports them; plans are made on the host that measures.
    pub topology: Option<Topology>,
    /// `perf stat` events counted for the whole workload process.
    pub perf_events: Vec<String>,
    pub perf_program: PathBuf,
    pub timeout: Duration,
    pub lock: PathBuf,
    pub decision_rule: Option<String>,
    /// The operator's statement that the measuring host is otherwise idle. Software cannot verify this.
    pub idle_host: bool,
    /// The command line that requested the campaign, for provenance.
    pub invocation: Vec<String>,
    /// Vary initial stack alignment with a random-length environment variable in each sample. On by default.
    pub stack_padding: bool,
}

impl PlanOptions {
    /// Defaults for everything except the suite, scenario, artifacts, and mode.
    pub fn new(
        suite: Suite,
        scenario: impl Into<String>,
        artifacts: ArtifactManifest,
        mode: Mode,
    ) -> Self {
        Self {
            suite,
            scenario: scenario.into(),
            artifacts,
            mode,
            rounds: None,
            seed: None,
            cpus: Vec::new(),
            role_cpus: BTreeMap::new(),
            programs: BTreeMap::new(),
            topology: None,
            perf_events: Vec::new(),
            perf_program: PathBuf::from("perf"),
            timeout: Duration::from_secs(300),
            lock: default_lock_path(),
            decision_rule: None,
            idle_host: false,
            invocation: Vec::new(),
            stack_padding: true,
        }
    }
}

/// The host-wide lock shared by every Base project and user on this machine. It is a fixed path rather than one under `$TMPDIR`, so sessions with different temporary directories still exclude each other. A private `/tmp`, as under systemd's `PrivateTmp`, still separates them; pass an explicit shared path there.
pub fn default_lock_path() -> PathBuf {
    PathBuf::from("/tmp/base-benchmark-host.lock")
}

/// A fully expanded, serializable campaign definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    pub version: u32,
    pub mode: Mode,
    pub package: String,
    pub scenario: String,
    pub rounds: u32,
    pub seed: u64,
    pub cpus: Vec<usize>,
    pub perf_events: Vec<String>,
    pub perf_program: PathBuf,
    pub timeout_ms: u64,
    pub lock: PathBuf,
    pub decision_rule: Option<String>,
    pub idle_host: bool,
    pub invocation: Vec<String>,
    /// Every workload starts here, so relative paths and the working directory are the same for all subjects.
    pub working_directory: PathBuf,
    /// The complete workload environment. The collector's own environment is not inherited.
    pub environment: BTreeMap<String, String>,
    /// Whether each sample adds a random-length environment variable, so that initial stack alignment varies between processes instead of being fixed by the environment's size.
    pub stack_padding: bool,
    pub subjects: Vec<Subject>,
    pub pairs: Vec<Pair>,
    pub artifacts: ArtifactManifest,
    /// A multi-process scenario's roles, in the order they start: services in declared order, then the driver. Empty for a single-process scenario.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<PlannedRole>,
    /// The external programs its roles run, identified when the plan was made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<ExternalProgram>,
    /// Whether the scenario declares that its roles may share physical cores.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared_placement: bool,
    /// Pairs of roles whose CPU sets share a physical core on the measuring host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_cores: Vec<SharedCore>,
    /// Roles' CPUs whose SMT siblings the measuring host did not report, so that sharing a physical core through them cannot be ruled out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_topology: Vec<usize>,
}

/// One role of every sample. The role that runs the subject takes each subject's program and arguments; every other role runs the same program with the same arguments for every subject.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedRole {
    pub name: String,
    pub kind: RoleKind,
    pub program: RoleProgram,
    /// Its arguments, with the mode's, for a role that does not run the subject. `{ROLE.NAME}` references are replaced by captured values when it starts.
    pub arguments: Vec<String>,
    /// The CPUs it is pinned to; empty when it is not placed.
    pub cpus: Vec<usize>,
    pub ready: Option<ReadyLine>,
    pub stop: Option<Stop>,
    /// Variables it receives in addition to the plan's environment: for the driver, `BASE_BENCHMARK_CONTROL`, which tells it how to send its window markers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

/// A program outside the workspace that a role runs, identified on the measuring host. Its hash is verified before every sample, so a program changed during the campaign fails it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalProgram {
    /// The name roles declare it by.
    pub name: String,
    /// The path it was given by, made absolute and canonical on the measuring host.
    pub path: PathBuf,
    pub sha256: String,
    /// The first line it printed for `--version`, or `None` when that failed, so its version is unknown.
    pub version: Option<String>,
}

/// How long an external program may take to report its version.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// One executable and argument list, run once per round.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    pub label: String,
    pub artifact: String,
    pub variant: Option<String>,
    pub role: ArtifactRole,
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

/// A baseline/candidate comparison computed from the same rounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    pub baseline: String,
    pub candidate: String,
    pub kind: PairKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairKind {
    /// Two variants of the same artifact.
    Variant,
    /// The same variant built from a candidate tree.
    Build,
    /// The same variant built from the baseline's layout replica: a control, not a change.
    Replica,
}

impl Plan {
    /// Expand subjects and pairs, then check the mode's prerequisites.
    pub fn new(mut options: PlanOptions) -> io::Result<Self> {
        let scenario = options.suite.scenario(&options.scenario)?.clone();
        // A scenario runs the builds made with its features.
        options.artifacts = options.artifacts.for_features(&scenario.features)?;
        if options.artifacts.package != options.suite.package
            || options.artifacts.binary != options.suite.binary
        {
            return Err(invalid_input(format!(
                "artifacts were prepared for {}/{}, not {}/{}",
                options.artifacts.package,
                options.artifacts.binary,
                options.suite.package,
                options.suite.binary
            )));
        }
        let rounds = options.rounds.unwrap_or(options.mode.default_rounds());
        if rounds == 0 || options.timeout.is_zero() {
            return Err(invalid_input("rounds and timeout must be positive"));
        }
        let (roles, programs) = plan_roles(&scenario, &options)?;
        let placed: Vec<usize> = roles
            .iter()
            .flat_map(|role| role.cpus.iter().copied())
            .collect();
        let topology = options
            .topology
            .clone()
            .unwrap_or_else(|| Topology::read(&placed));
        let unavailable = topology.unavailable(&placed);
        if !unavailable.is_empty() {
            return Err(invalid_input(format!(
                "roles are placed on CPUs this host cannot run them on: {}",
                unavailable.join(", ")
            )));
        }
        let (shared_cores, unknown_topology) = shared_cores(&roles, &topology);
        let variants: Vec<Option<&crate::config::Variant>> = if scenario.variants.is_empty() {
            vec![None]
        } else {
            scenario.variants.iter().map(Some).collect()
        };
        let label = |artifact: &str, variant: Option<&str>| match variant {
            Some(variant) => format!("{artifact}/{variant}"),
            None => artifact.to_owned(),
        };
        let mut subjects = Vec::new();
        for artifact in &options.artifacts.artifacts {
            for variant in &variants {
                let mut arguments = scenario.arguments.clone();
                if let Some(variant) = variant {
                    arguments.extend(variant.arguments.iter().cloned());
                }
                let mode_arguments = if options.mode == Mode::Smoke {
                    &scenario.smoke_arguments
                } else {
                    &scenario.measure_arguments
                };
                arguments.extend(mode_arguments.iter().cloned());
                let variant = variant.map(|variant| variant.name.clone());
                subjects.push(Subject {
                    label: label(&artifact.label, variant.as_deref()),
                    artifact: artifact.label.clone(),
                    variant,
                    role: artifact.role,
                    program: artifact.executable.clone(),
                    arguments,
                });
            }
        }

        let baseline_artifact = options.artifacts.baseline().label.clone();
        let baseline_variant = scenario
            .baseline_variant()
            .map(|variant| variant.name.clone());
        let mut pairs = Vec::new();
        for artifact in &options.artifacts.artifacts {
            for variant in &variants {
                let variant = variant.map(|variant| variant.name.as_str());
                if artifact.label != baseline_artifact {
                    let kind = if artifact.role == ArtifactRole::LayoutReplica {
                        PairKind::Replica
                    } else {
                        PairKind::Build
                    };
                    pairs.push(Pair {
                        baseline: label(&baseline_artifact, variant),
                        candidate: label(&artifact.label, variant),
                        kind,
                    });
                }
                if variant.is_some() && variant != baseline_variant.as_deref() {
                    pairs.push(Pair {
                        baseline: label(&artifact.label, baseline_variant.as_deref()),
                        candidate: label(&artifact.label, variant),
                        kind: PairKind::Variant,
                    });
                }
            }
        }

        let plan = Self {
            schema: PLAN_SCHEMA.to_owned(),
            version: PLAN_VERSION,
            mode: options.mode,
            package: options.suite.package,
            scenario: scenario.name,
            rounds,
            seed: options.seed.unwrap_or_else(seed_from_clock),
            cpus: options.cpus,
            perf_events: options.perf_events,
            perf_program: options.perf_program,
            timeout_ms: u64::try_from(options.timeout.as_millis()).unwrap_or(u64::MAX),
            lock: options.lock,
            decision_rule: options.decision_rule.filter(|rule| !rule.trim().is_empty()),
            idle_host: options.idle_host,
            invocation: options.invocation,
            working_directory: options.suite.workspace_root.clone(),
            environment: launch_environment(
                std::env::var("PATH").ok(),
                // Recorded campaigns keep their environment independent of how the collector was started.
                std::env::var("LLVM_PROFILE_FILE")
                    .ok()
                    .filter(|_| options.mode != Mode::Recorded),
            ),
            stack_padding: options.stack_padding,
            subjects,
            pairs,
            artifacts: options.artifacts,
            roles,
            programs,
            shared_placement: scenario.shared_placement,
            shared_cores,
            unknown_topology,
        };
        plan.check_prerequisites()?;
        Ok(plan)
    }

    pub fn subject(&self, label: &str) -> Option<&Subject> {
        self.subjects.iter().find(|subject| subject.label == label)
    }

    /// The roles without a CPU set.
    pub fn unplaced_roles(&self) -> Vec<&str> {
        self.roles
            .iter()
            .filter(|role| role.cpus.is_empty())
            .map(|role| role.name.as_str())
            .collect()
    }

    /// Every CPU a workload process may be pinned to: the single-process placement, or every role's.
    pub fn placed_cpus(&self) -> Vec<usize> {
        let mut cpus: Vec<usize> = self
            .cpus
            .iter()
            .chain(self.roles.iter().flat_map(|role| &role.cpus))
            .copied()
            .collect();
        cpus.sort_unstable();
        cpus.dedup();
        cpus
    }

    /// Recorded campaigns must declare what makes them acceptance evidence. All unmet prerequisites are reported together.
    pub fn check_prerequisites(&self) -> io::Result<()> {
        if self.mode != Mode::Recorded {
            return Ok(());
        }
        let mut missing = Vec::new();
        if self.rounds < 10 {
            missing.push("at least 10 rounds".to_owned());
        }
        let unplaced = self.unplaced_roles();
        if self.roles.is_empty() && self.cpus.is_empty() {
            missing.push("explicit CPU placement".to_owned());
        } else if !unplaced.is_empty() {
            missing.push(format!(
                "explicit CPU placement for every role ({} has none)",
                unplaced.join(", ")
            ));
        }
        if self.decision_rule.is_none() {
            missing.push("a predeclared decision rule".to_owned());
        }
        if !self.idle_host {
            missing.push("an idle-host declaration".to_owned());
        }
        let artifacts = &self.artifacts.artifacts;
        for artifact in artifacts.iter().filter(|artifact| {
            artifact.role != ArtifactRole::LayoutReplica && !artifact.source.is_frozen()
        }) {
            let reason = if artifact.source.path_dependencies.is_none() {
                " (Cargo metadata could not be read during preparation, so its path dependencies are unknown)"
            } else {
                ""
            };
            let overlay = match artifact.source.overlay {
                Some(_) => ", and overlaid workload",
                None => "",
            };
            missing.push(format!(
                "committed, unmodified source and path dependencies{overlay} for {}{reason}",
                artifact.label
            ));
        }
        for artifact in artifacts
            .iter()
            .filter(|artifact| !artifact.source.lockfile_changes.is_empty())
        {
            missing.push(format!(
                "an overlaid build of {} that keeps the versions its tree locks ({})",
                artifact.label,
                artifact.source.lockfile_changes.join("; ")
            ));
        }
        let trees = artifacts
            .iter()
            .filter(|artifact| artifact.role != ArtifactRole::LayoutReplica)
            .count();
        if trees > 1
            && !artifacts
                .iter()
                .any(|artifact| artifact.role == ArtifactRole::LayoutReplica)
        {
            missing
                .push("a layout replica for the comparison of separately built trees".to_owned());
        }
        if artifacts
            .iter()
            .any(|artifact| artifact.build.settings() != artifacts[0].build.settings())
        {
            missing.push("the same toolchain, Cargo version, build environment, and Cargo configuration for every artifact".to_owned());
        }
        if !self.shared_placement && !self.shared_cores.is_empty() {
            missing.push(format!(
                "roles on separate physical cores, or a scenario that declares shared-placement ({})",
                describe(&self.shared_cores)
            ));
        }
        // Declared sharing allows sharing through siblings that are not reported, too.
        if !self.shared_placement && !self.unknown_topology.is_empty() {
            missing.push(format!(
                "the SMT siblings of CPUs {}, to rule out roles sharing a physical core",
                cpu_list(&self.unknown_topology)
            ));
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(invalid_input(format!(
                "recorded campaigns need {}",
                missing.join(", ")
            )))
        }
    }
}

/// Expand a scenario's roles in start order, and identify the external programs they run.
fn plan_roles(
    scenario: &crate::config::Scenario,
    options: &PlanOptions,
) -> io::Result<(Vec<PlannedRole>, Vec<ExternalProgram>)> {
    if scenario.roles.is_empty() {
        if let Some(role) = options.role_cpus.keys().next() {
            return Err(invalid_input(format!(
                "scenario {:?} has no roles, so role {role:?} cannot be placed; use --cpus",
                scenario.name
            )));
        }
        return Ok((Vec::new(), Vec::new()));
    }
    if !options.cpus.is_empty() {
        return Err(invalid_input(format!(
            "scenario {:?} has roles; place each with --role-cpus ROLE=CPUS rather than --cpus",
            scenario.name
        )));
    }
    if !options.perf_events.is_empty() {
        return Err(invalid_input(format!(
            "scenario {:?} has roles, whose processes cannot be counted with perf stat yet",
            scenario.name
        )));
    }
    if let Some(role) = options
        .role_cpus
        .keys()
        .find(|name| !scenario.roles.iter().any(|role| &role.name == *name))
    {
        return Err(invalid_input(format!(
            "scenario {:?} has no role {role:?} to place",
            scenario.name
        )));
    }
    let services = scenario
        .roles
        .iter()
        .filter(|role| role.kind == RoleKind::Service);
    let drivers = scenario
        .roles
        .iter()
        .filter(|role| role.kind == RoleKind::Driver);
    let mut programs: Vec<ExternalProgram> = Vec::new();
    let mut roles = Vec::new();
    for role in services.chain(drivers) {
        let arguments = match role.program {
            RoleProgram::Subject => Vec::new(),
            _ => {
                let mode_arguments = if options.mode == Mode::Smoke {
                    &role.smoke_arguments
                } else {
                    &role.measure_arguments
                };
                role.arguments
                    .iter()
                    .chain(mode_arguments)
                    .cloned()
                    .collect()
            }
        };
        if let RoleProgram::Binary {
            package,
            binary,
            features,
        } = &role.program
        {
            if options
                .artifacts
                .helper_index(package, binary, features)
                .is_none()
            {
                return Err(invalid_input(format!(
                    "role {:?} runs {package}'s binary {binary} with features {features:?}, which these artifacts do not include; prepare the scenario again",
                    role.name
                )));
            }
        }
        if let RoleProgram::External { name } = &role.program {
            if !programs.iter().any(|program| &program.name == name) {
                let path = options.programs.get(name).ok_or_else(|| {
                    invalid_input(format!(
                        "role {:?} runs the external program {name:?}; give its path with --program {name}=PATH",
                        role.name
                    ))
                })?;
                programs.push(ExternalProgram::identify(name, path)?);
            }
        }
        roles.push(PlannedRole {
            name: role.name.clone(),
            kind: role.kind,
            program: role.program.clone(),
            arguments,
            cpus: options
                .role_cpus
                .get(&role.name)
                .cloned()
                .unwrap_or_default(),
            ready: role.ready.clone(),
            stop: role.stop.clone(),
            environment: match role.kind {
                RoleKind::Service => BTreeMap::new(),
                RoleKind::Driver => BTreeMap::from([(
                    crate::window::CONTROL_VARIABLE.to_owned(),
                    crate::window::CONTROL_STDIO.to_owned(),
                )]),
            },
        });
    }
    Ok((roles, programs))
}

impl ExternalProgram {
    /// Resolve `path` on this host, relative to the current directory, and hash it.
    pub fn identify(name: &str, path: &std::path::Path) -> io::Result<Self> {
        let resolved = path.canonicalize().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("external program {name}, {}: {error}", path.display()),
            )
        })?;
        let sha256 = sha256_identified(&resolved).map_err(|error| {
            io::Error::new(error.kind(), format!("external program {name}: {error}"))
        })?;
        Ok(Self {
            name: name.to_owned(),
            version: reported_version(&resolved),
            path: resolved,
            sha256,
        })
    }

    /// Fail if the program changed since the plan identified it.
    pub fn verify(&self) -> io::Result<()> {
        let actual = sha256_identified(&self.path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("external program {}: {error}", self.name),
            )
        })?;
        if actual != self.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "external program {} changed during the campaign: {} no longer has SHA-256 {}",
                    self.name,
                    self.path.display(),
                    self.sha256
                ),
            ));
        }
        Ok(())
    }
}

/// The first non-empty line `program --version` prints, on stdout or else stderr, run with a minimal environment from the root directory, when it exits successfully within [`VERSION_TIMEOUT`].
fn reported_version(program: &std::path::Path) -> Option<String> {
    let arguments = ["--version".to_owned()];
    let environment = launch_environment(std::env::var("PATH").ok(), None);
    let launch = crate::runner::Launch {
        program,
        arguments: &arguments,
        environment: &environment,
        directory: std::path::Path::new("/"),
        cpus: &[],
    };
    crate::runner::execute(
        &launch,
        VERSION_TIMEOUT,
        &crate::runner::Cancellation::new(),
    )
    .ok()
    .filter(|execution| execution.succeeded())
    .and_then(|execution| {
        [execution.stdout, execution.stderr]
            .iter()
            .find_map(|output| {
                String::from_utf8_lossy(output)
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_owned)
            })
    })
}

/// CPUs as a comma-separated list.
pub(crate) fn cpu_list(cpus: &[usize]) -> String {
    cpus.iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse a CPU list such as `0,2,4-6` into sorted, unique CPU numbers.
pub fn parse_cpus(value: &str) -> io::Result<Vec<usize>> {
    let invalid = || invalid_input(format!("invalid CPU list {value:?}"));
    let mut cpus = Vec::new();
    for part in value.split(',') {
        match part.split_once('-') {
            Some((first, last)) => {
                let (first, last): (usize, usize) = (
                    first.parse().map_err(|_| invalid())?,
                    last.parse().map_err(|_| invalid())?,
                );
                if first > last {
                    return Err(invalid());
                }
                cpus.extend(first..=last);
            }
            None => cpus.push(part.parse().map_err(|_| invalid())?),
        }
    }
    cpus.sort_unstable();
    cpus.dedup();
    Ok(cpus)
}

/// The variable that carries stack padding, and the exclusive bound of its random length in bytes.
pub const STACK_PADDING_VARIABLE: &str = "BASE_BENCHMARK_STACK_PADDING";
pub const STACK_PADDING_LIMIT: usize = 4096;

/// A minimal, recorded environment: `PATH` from the collector and the C locale, and the collector's `LLVM_PROFILE_FILE` when it has one.
///
/// The size of the environment shifts the initial stack, so inheriting a shell's or agent's environment would make layout depend on who ran the campaign. `LLVM_PROFILE_FILE` is kept, outside recorded campaigns, so that a coverage run, such as a workload's test that drives it through the runner under `cargo llvm-cov`, collects the workload's profiles where the coverage tool expects them, rather than leaving `default_*.profraw` files in the working directory. Like any variable its length shifts the initial stack, but every subject of a campaign gets the same value.
pub(crate) fn launch_environment(
    path: Option<String>,
    profile: Option<String>,
) -> BTreeMap<String, String> {
    let path = path.unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_owned());
    let mut environment = BTreeMap::from([
        ("LC_ALL".to_owned(), "C".to_owned()),
        ("PATH".to_owned(), path),
    ]);
    if let Some(profile) = profile {
        environment.insert("LLVM_PROFILE_FILE".to_owned(), profile);
    }
    environment
}

fn seed_from_clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default()
}

/// A small deterministic generator (SplitMix64) for reproducible round orders.
pub(crate) struct Shuffle(u64);

impl Shuffle {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// A value in `0..bound`. The modulo bias is negligible for the small bounds used here.
    pub fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    /// A Fisher-Yates permutation of `0..count`.
    pub fn order(&mut self, count: usize) -> Vec<usize> {
        let mut order: Vec<usize> = (0..count).collect();
        for index in (1..count).rev() {
            let other = (self.next() % (index as u64 + 1)) as usize;
            order.swap(index, other);
        }
        order
    }
}

#[cfg(test)]
mod tests;
