// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Bake-independent benchmark measurement and experiment tooling for Base projects.
//!
//! Workloads stay project-owned. A workload binary measures its own operations with [`Interval`] and writes one [`ProcessRecord`] to stdout. This crate prepares frozen executables from labelled source trees, runs them in seeded random rounds under a host-wide lock, rejects malformed or incomplete evidence, and summarizes paired changes with explicit metric scopes.
//!
//! ```no_run
//! use base_benchmark::{Interval, Metric, ProcessRecord, Unit};
//!
//! fn main() -> std::io::Result<()> {
//!     let iterations = 1000;
//!     let interval = Interval::start()?;
//!     let mut bytes = 0;
//!     for _ in 0..iterations {
//!         bytes += std::hint::black_box("work").len() as u64;
//!     }
//!     let sample = interval.finish(iterations)?.with(Metric::total("output_bytes", Unit::Bytes, bytes));
//!     ProcessRecord::new("example/work", serde_json::json!({ "iterations": iterations }))
//!         .with_sample(sample)
//!         .emit(&mut std::io::stdout())
//! }
//! ```
//!
//! The same operations are available through the `base-benchmark` command and, separately, the `bake-benchmark` task library. Records and `Interval` are always available; preparation, collection, summaries, and the command need the default `runner` feature, so workloads can depend on the crate with `default-features = false`. The runner builds on every platform, so a project's task package can depend on it everywhere. Collecting samples needs Linux, the only platform it is tested on; the host lock and replica copies of symbolic links need Unix. Elsewhere those operations return [`std::io::ErrorKind::Unsupported`] errors. `Interval` measures through `getrusage`, so it needs Unix.
mod record;
mod usage;

#[cfg(feature = "runner")]
mod artifact;
#[cfg(feature = "runner")]
pub mod cli;
#[cfg(feature = "runner")]
mod config;
#[cfg(feature = "runner")]
mod plan;
#[cfg(feature = "runner")]
mod prepare;
#[cfg(feature = "runner")]
mod runner;
#[cfg(feature = "runner")]
mod summary;
#[cfg(all(test, feature = "runner"))]
mod test_support;
#[cfg(feature = "runner")]
mod workspace;

pub use record::{
    Direction, Measurement, Metric, Phase, ProcessRecord, RECORD_SCHEMA, RECORD_VERSION, Sample,
    Source, Unit,
};
pub use usage::{CurrentProcess, Interval, ResourceSource, ResourceUsage};

#[cfg(feature = "runner")]
pub use artifact::{
    Artifact, ArtifactManifest, ArtifactRole, BuildIdentity, ConfigurationFile, DependencyIdentity,
    MANIFEST_SCHEMA, MANIFEST_VERSION, SourceIdentity, sha256_file,
};
#[cfg(feature = "runner")]
pub use config::{METADATA_KEY, Scenario, Suite, Variant, cargo_program, discover, select};
#[cfg(feature = "runner")]
pub use plan::{
    Mode, PLAN_SCHEMA, PLAN_VERSION, Pair, PairKind, Plan, PlanOptions, STACK_PADDING_LIMIT,
    STACK_PADDING_VARIABLE, Subject, default_lock_path, parse_cpus,
};
#[cfg(feature = "runner")]
pub use prepare::{PrepareOptions, Prepared, Tree, prepare};
#[cfg(feature = "runner")]
pub use runner::{
    CAMPAIGN_SCHEMA, CAMPAIGN_VERSION, Campaign, CampaignMetadata, CampaignStatus, Cancellation,
    CpuIdentity, HostIdentity, SampleEntry, SignalGuard, read_entries, read_metadata, run,
    summarize_directory,
};
#[cfg(feature = "runner")]
pub use summary::{
    Distribution, GENERATOR, MetricSummary, PairMetric, PairSummary, RESOLUTION_FACTOR,
    SUMMARY_SCHEMA, SUMMARY_VERSION, Statistic, SubjectSummary, Summary, sign_test, summarize,
};
#[cfg(feature = "runner")]
pub use workspace::{DEFAULT_TIMEOUT, RunRequest, Workspace};
