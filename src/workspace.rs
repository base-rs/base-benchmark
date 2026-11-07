// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::artifact::ArtifactManifest;
use crate::config::{self, Suite};
use crate::plan::{Mode, Plan, PlanOptions, default_lock_path};
use crate::prepare::{PrepareOptions, Prepared, Tree, prepare};
use crate::runner::{self, Campaign, Cancellation};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The default deadline for one workload process.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// The semantic operations shared by the `base-benchmark` command and the Bake tasks.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub manifest_path: PathBuf,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
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
    pub cpus: Vec<usize>,
    pub perf_events: Vec<String>,
    /// Defaults to [`DEFAULT_TIMEOUT`].
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
        }
    }

    pub fn suites(&self) -> io::Result<Vec<Suite>> {
        config::discover(&self.cargo, &self.manifest_path)
    }

    pub fn suite(&self, package: Option<&str>) -> io::Result<Suite> {
        config::select(self.suites()?, package)
    }

    /// Build labelled trees, the first being the baseline, plus `replicas` layout replicas. Without trees, the current workspace is the baseline, labelled `current`.
    pub fn prepare(
        &self,
        package: Option<&str>,
        mut trees: Vec<Tree>,
        replicas: Option<usize>,
        output: Option<PathBuf>,
        cancellation: &Cancellation,
    ) -> io::Result<Prepared> {
        let suite = self.suite(package)?;
        if trees.is_empty() {
            trees.push(Tree {
                label: "current".to_owned(),
                path: suite.workspace_root.clone(),
            });
        }
        let output = output.unwrap_or_else(|| default_output(&suite.workspace_root, "prepared"));
        prepare(
            &PrepareOptions {
                suite,
                trees,
                replicas,
                output,
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
        let output = request.output.unwrap_or_else(|| {
            default_output(
                &suite.workspace_root,
                &format!("{}-{mode}", request.scenario),
            )
        });
        let plan = Plan::new(PlanOptions {
            rounds: request.rounds,
            seed: request.seed,
            cpus: request.cpus,
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
            Some(&suite.package),
            Vec::new(),
            Some(0),
            Some(output.join(".artifacts")),
            cancellation,
        )?;
        self.smoke_all(
            &suite,
            &prepared.manifest_path,
            &output,
            lock,
            cancellation,
            progress,
        )
    }

    /// Run every scenario once, with its smoke arguments, using an executable built outside preparation, such as a workload's `CARGO_BIN_EXE_*` in its own tests.
    pub fn smoke_executable(
        &self,
        package: Option<&str>,
        executable: &Path,
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
        self.smoke_all(
            &suite,
            &manifest_path,
            output,
            lock,
            &Cancellation::new(),
            &mut io::sink(),
        )
    }

    fn smoke_all(
        &self,
        suite: &Suite,
        artifacts: &Path,
        output: &Path,
        lock: Option<PathBuf>,
        cancellation: &Cancellation,
        progress: &mut dyn Write,
    ) -> io::Result<Vec<Campaign>> {
        suite
            .scenarios
            .iter()
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

/// A new directory under the workspace's Cargo target directory.
fn default_output(workspace_root: &Path, name: &str) -> PathBuf {
    workspace_root
        .join("target")
        .join("base-benchmark")
        .join(format!("{name}-{}", runner::unix_ms()))
}
