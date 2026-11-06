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

/// The semantic operations shared by the `base-benchmark` command and the Bake tasks.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub manifest_path: PathBuf,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
}

/// A request to collect one scenario from prepared artifacts.
#[derive(Clone, Debug)]
pub struct RunRequest {
    pub package: Option<String>,
    pub scenario: String,
    pub artifacts: PathBuf,
    pub mode: Mode,
    pub rounds: Option<u32>,
    pub seed: Option<u64>,
    pub cpus: Vec<usize>,
    pub perf_events: Vec<String>,
    pub timeout: Duration,
    pub lock: PathBuf,
    pub output: Option<PathBuf>,
    pub decision_rule: Option<String>,
    pub idle_host: bool,
    pub invocation: Vec<String>,
}

impl RunRequest {
    pub fn new(scenario: impl Into<String>, artifacts: impl Into<PathBuf>, mode: Mode) -> Self {
        Self {
            package: None,
            scenario: scenario.into(),
            artifacts: artifacts.into(),
            mode,
            rounds: None,
            seed: None,
            cpus: Vec::new(),
            perf_events: Vec::new(),
            timeout: Duration::from_secs(300),
            lock: default_lock_path(),
            output: None,
            decision_rule: None,
            idle_host: false,
            invocation: Vec::new(),
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

    /// Build labelled trees, the first being the baseline. Without trees, the current workspace is the baseline, labelled `current`.
    pub fn prepare(
        &self,
        package: Option<&str>,
        mut trees: Vec<Tree>,
        replica: Option<bool>,
        output: Option<PathBuf>,
    ) -> io::Result<Prepared> {
        let suite = self.suite(package)?;
        if trees.is_empty() {
            trees.push(Tree {
                label: "current".to_owned(),
                path: suite.workspace_root.clone(),
            });
        }
        let output = output.unwrap_or_else(|| default_output(&suite.workspace_root, "prepared"));
        prepare(&PrepareOptions {
            suite,
            trees,
            replica,
            output,
            cargo: self.cargo.clone(),
            rustc: self.rustc.clone(),
        })
    }

    pub fn run(
        &self,
        request: RunRequest,
        cancellation: &Cancellation,
        progress: &mut dyn Write,
    ) -> io::Result<Campaign> {
        let suite = self.suite(request.package.as_deref())?;
        let artifacts = ArtifactManifest::read(&request.artifacts)?;
        let output = request.output.unwrap_or_else(|| {
            default_output(
                &suite.workspace_root,
                &format!("{}-{}", request.scenario, request.mode),
            )
        });
        let plan = Plan::new(PlanOptions {
            rounds: request.rounds,
            seed: request.seed,
            cpus: request.cpus,
            perf_events: request.perf_events,
            timeout: request.timeout,
            lock: request.lock,
            decision_rule: request.decision_rule,
            idle_host: request.idle_host,
            invocation: request.invocation,
            ..PlanOptions::new(suite, request.scenario, artifacts, request.mode)
        })?;
        runner::run(plan, &output, cancellation, progress)
    }

    /// Build the current workspace and run every scenario once with its smoke arguments.
    pub fn smoke(
        &self,
        package: Option<&str>,
        output: Option<PathBuf>,
        lock: &Path,
        cancellation: &Cancellation,
        progress: &mut dyn Write,
    ) -> io::Result<Vec<Campaign>> {
        let suite = self.suite(package)?;
        let output = output.unwrap_or_else(|| default_output(&suite.workspace_root, "smoke"));
        let prepared = self.prepare(
            Some(&suite.package),
            Vec::new(),
            Some(false),
            Some(output.join("artifacts")),
        )?;
        let mut campaigns = Vec::new();
        for scenario in &suite.scenarios {
            let mut request = RunRequest::new(&scenario.name, &prepared.manifest_path, Mode::Smoke);
            request.package = Some(suite.package.clone());
            request.lock = lock.to_owned();
            request.output = Some(output.join(&scenario.name));
            campaigns.push(self.run(request, cancellation, progress)?);
        }
        Ok(campaigns)
    }
}

/// A new directory under the workspace's Cargo target directory.
fn default_output(workspace_root: &Path, name: &str) -> PathBuf {
    workspace_root
        .join("target")
        .join("base-benchmark")
        .join(format!("{name}-{}", runner::unix_ms()))
}
