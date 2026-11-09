// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::artifact::ArtifactManifest;
use crate::config::{self, Scenario, Suite};
use crate::plan::{Mode, Plan, PlanOptions, default_lock_path};
use crate::prepare::{Helper, Overlay, PrepareOptions, Prepared, Tree, prepare};
use crate::role::RoleProgram;
use crate::runner::{self, Campaign, Cancellation};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The default deadline for one workload process, or for one session of a multi-process scenario.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// The semantic operations shared by the `base-benchmark` command and the Bake tasks.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub manifest_path: PathBuf,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
    /// External programs that roles run, by the name they declare, as paths on the measuring host. Runs and smoke runs identify the programs their scenarios use, and fail if one is missing.
    pub programs: BTreeMap<String, PathBuf>,
}

/// A request to prepare artifacts. Unset options take the core's defaults.
#[derive(Clone, Debug, Default)]
pub struct PrepareRequest {
    pub package: Option<String>,
    /// Labelled trees, the first being the baseline. Without trees, the current workspace is the baseline, labelled `current`.
    pub trees: Vec<Tree>,
    /// Layout replicas of the baseline; defaults to one when comparing two or more trees, otherwise none.
    pub replicas: Option<usize>,
    /// Defaults to a new directory under `target/base-benchmark`.
    pub output: Option<PathBuf>,
    /// Build the feature sets these scenarios need; without scenarios, every scenario's.
    pub scenarios: Vec<String>,
    /// Build every tree with this workspace's workload package in place of the tree's own; see [`Overlay`].
    pub overlay: bool,
}

/// A request to collect one scenario from prepared artifacts. Unset options take the core's defaults.
#[derive(Clone, Debug, Default)]
pub struct RunRequest {
    pub package: Option<String>,
    pub scenario: String,
    pub artifacts: PathBuf,
    /// Defaults to [`Mode::Screening`].
    pub mode: Option<Mode>,
    /// Defaults to the mode's round count.
    pub rounds: Option<u32>,
    pub seed: Option<u64>,
    /// CPUs a single-process scenario's workloads are pinned to.
    pub cpus: Vec<usize>,
    /// CPUs each role of a multi-process scenario is pinned to, by role name.
    pub role_cpus: BTreeMap<String, Vec<usize>>,
    pub perf_events: Vec<String>,
    /// Defaults to [`DEFAULT_TIMEOUT`]: the deadline of one workload process, or of a whole session of a multi-process scenario.
    pub timeout: Option<Duration>,
    /// Defaults to [`default_lock_path`].
    pub lock: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub decision_rule: Option<String>,
    pub idle_host: bool,
    /// Defaults to enabled.
    pub stack_padding: Option<bool>,
    pub invocation: Vec<String>,
}

impl RunRequest {
    pub fn new(scenario: impl Into<String>, artifacts: impl Into<PathBuf>) -> Self {
        Self {
            scenario: scenario.into(),
            artifacts: artifacts.into(),
            ..Self::default()
        }
    }
}

impl Workspace {
    /// Use `$CARGO` and `$RUSTC` when set, otherwise `cargo` and `rustc` from `PATH`.
    pub fn new(manifest_path: impl Into<PathBuf>) -> Self {
        Self {
            manifest_path: manifest_path.into(),
            cargo: config::cargo_program(),
            rustc: config::program(std::env::var_os("RUSTC"), "rustc"),
            programs: BTreeMap::new(),
        }
    }

    /// Give the path of the external program that roles declare as `name`.
    pub fn with_program(mut self, name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.programs.insert(name.into(), path.into());
        self
    }

    pub fn suites(&self) -> io::Result<Vec<Suite>> {
        config::discover(&self.cargo, &self.manifest_path)
    }

    pub fn suite(&self, package: Option<&str>) -> io::Result<Suite> {
        config::select(self.suites()?, package)
    }

    /// Build labelled trees, the first being the baseline, plus layout replicas of the baseline, once for each feature set the requested scenarios need.
    pub fn prepare(
        &self,
        request: PrepareRequest,
        cancellation: &Cancellation,
    ) -> io::Result<Prepared> {
        let suite = self.suite(request.package.as_deref())?;
        let mut trees = request.trees;
        if trees.is_empty() {
            trees.push(Tree {
                label: "current".to_owned(),
                path: suite.workspace_root.clone(),
            });
        }
        let scenarios = if request.scenarios.is_empty() {
            suite.scenarios.iter().collect()
        } else {
            request
                .scenarios
                .iter()
                .map(|name| suite.scenario(name))
                .collect::<io::Result<Vec<_>>>()?
        };
        let mut feature_sets: Vec<Vec<String>> = Vec::new();
        let mut helpers: Vec<Helper> = Vec::new();
        for scenario in scenarios {
            if !feature_sets.contains(&scenario.features) {
                feature_sets.push(scenario.features.clone());
            }
            for helper in scenario
                .roles
                .iter()
                .filter_map(|role| helper(&role.program))
            {
                if !helpers.contains(&helper) {
                    helpers.push(helper);
                }
            }
        }
        let output = request
            .output
            .unwrap_or_else(|| default_output(&suite.workspace_root, "prepared"));
        let overlay = request.overlay.then(|| Overlay::of(&suite)).transpose()?;
        prepare(
            &PrepareOptions {
                suite,
                trees,
                replicas: request.replicas,
                output,
                feature_sets,
                overlay,
                helpers,
                cargo: self.cargo.clone(),
                rustc: self.rustc.clone(),
            },
            cancellation,
        )
    }

    pub fn run(
        &self,
        request: RunRequest,
        cancellation: &Cancellation,
        progress: &mut dyn Write,
    ) -> io::Result<Campaign> {
        let suite = self.suite(request.package.as_deref())?;
        let artifacts = ArtifactManifest::read(&request.artifacts)?;
        let mode = request.mode.unwrap_or(Mode::Screening);
        // Absolute, since workloads start in the workspace root, where staged executables must still be found.
        let output = std::path::absolute(request.output.unwrap_or_else(|| {
            default_output(
                &suite.workspace_root,
                &format!("{}-{mode}", request.scenario),
            )
        }))?;
        let plan = Plan::new(PlanOptions {
            rounds: request.rounds,
            seed: request.seed,
            cpus: request.cpus,
            role_cpus: request.role_cpus,
            programs: self.programs.clone(),
            perf_events: request.perf_events,
            timeout: request.timeout.unwrap_or(DEFAULT_TIMEOUT),
            lock: request.lock.unwrap_or_else(default_lock_path),
            decision_rule: request.decision_rule,
            idle_host: request.idle_host,
            stack_padding: request.stack_padding.unwrap_or(true),
            invocation: request.invocation,
            ..PlanOptions::new(suite, request.scenario, artifacts, mode)
        })?;
        runner::run(plan, &output, cancellation, progress)
    }

    /// Build the current workspace and run every scenario once with its smoke arguments.
    pub fn smoke(
        &self,
        package: Option<&str>,
        output: Option<PathBuf>,
        lock: Option<PathBuf>,
        cancellation: &Cancellation,
        progress: &mut dyn Write,
    ) -> io::Result<Vec<Campaign>> {
        // Fail before building when samples cannot be collected here.
        #[cfg(not(target_os = "linux"))]
        runner::check_collection()?;
        let suite = self.suite(package)?;
        let output = output.unwrap_or_else(|| default_output(&suite.workspace_root, "smoke"));
        // Labels cannot start with `.`, so prepared artifacts never collide with a scenario's directory.
        let prepared = self.prepare(
            PrepareRequest {
                package: Some(suite.package.clone()),
                replicas: Some(0),
                output: Some(output.join(".artifacts")),
                ..PrepareRequest::default()
            },
            cancellation,
        )?;
        self.smoke_all(
            &suite,
            suite.scenarios.iter().collect(),
            &prepared.manifest_path,
            &output,
            lock,
            cancellation,
            progress,
        )
    }

    /// Run once, with its smoke arguments, every scenario whose features `features` includes, using an executable built outside preparation with those features, such as a workload's `CARGO_BIN_EXE_*` in its own tests. Scenarios that need other features, or whose roles run helper binaries, are skipped, since the executable cannot run them and no helper was built.
    pub fn smoke_executable(
        &self,
        package: Option<&str>,
        executable: &Path,
        features: &[&str],
        output: &Path,
        lock: Option<PathBuf>,
    ) -> io::Result<Vec<Campaign>> {
        #[cfg(not(target_os = "linux"))]
        runner::check_collection()?;
        let suite = self.suite(package)?;
        let manifest_path = output.join("unprepared.json");
        std::fs::create_dir_all(output)
            .and_then(|()| ArtifactManifest::unprepared(&suite, executable))
            .and_then(|manifest| manifest.write(&manifest_path))?;
        // An executable built outside preparation comes with no helpers.
        let scenarios = suite
            .scenarios
            .iter()
            .filter(|scenario| {
                scenario
                    .features
                    .iter()
                    .all(|feature| features.contains(&feature.as_str()))
                    && scenario
                        .roles
                        .iter()
                        .all(|role| helper(&role.program).is_none())
            })
            .collect::<Vec<_>>();
        if scenarios.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "no scenario of {} can run with features {features:?}",
                    suite.package
                ),
            ));
        }
        self.smoke_all(
            &suite,
            scenarios,
            &manifest_path,
            output,
            lock,
            &Cancellation::new(),
            &mut io::sink(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn smoke_all(
        &self,
        suite: &Suite,
        scenarios: Vec<&Scenario>,
        artifacts: &Path,
        output: &Path,
        lock: Option<PathBuf>,
        cancellation: &Cancellation,
        progress: &mut dyn Write,
    ) -> io::Result<Vec<Campaign>> {
        scenarios
            .into_iter()
            .map(|scenario| {
                let request = RunRequest {
                    package: Some(suite.package.clone()),
                    mode: Some(Mode::Smoke),
                    lock: lock.clone(),
                    output: Some(output.join(&scenario.name)),
                    ..RunRequest::new(&scenario.name, artifacts)
                };
                self.run(request, cancellation, progress)
            })
            .collect()
    }
}

/// The helper a role's program needs built, if any.
fn helper(program: &RoleProgram) -> Option<Helper> {
    match program {
        RoleProgram::Binary {
            package,
            binary,
            features,
        } => Some(Helper {
            package: package.clone(),
            binary: binary.clone(),
            features: features.clone(),
        }),
        _ => None,
    }
}

/// A new directory under the workspace's Cargo target directory.
pub(crate) fn default_output(workspace_root: &Path, name: &str) -> PathBuf {
    workspace_root
        .join("target")
        .join("base-benchmark")
        .join(format!("{name}-{}", runner::unix_ms()))
}
