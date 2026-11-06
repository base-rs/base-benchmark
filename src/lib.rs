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
//! The same operations are available through the `base-benchmark` command and, separately, the `bake-benchmark` task library.

mod artifact;
#[cfg(unix)]
pub mod cli;
mod config;
mod plan;
mod prepare;
mod record;
#[cfg(unix)]
mod summary;
#[cfg(test)]
mod test_support;
mod usage;

#[cfg(unix)]
mod runner;
#[cfg(unix)]
mod workspace;

pub use artifact::{
    Artifact, ArtifactManifest, ArtifactRole, BuildIdentity, MANIFEST_SCHEMA, MANIFEST_VERSION,
    SourceIdentity, sha256_file,
};
pub use config::{METADATA_KEY, Scenario, Suite, Variant, cargo_program, discover, select};
pub use plan::{
    Mode, PLAN_SCHEMA, PLAN_VERSION, Pair, PairKind, Plan, PlanOptions, Subject, default_lock_path,
};
pub use prepare::{PrepareOptions, Prepared, Tree, prepare};
pub use record::{
    Direction, Measurement, Metric, Phase, ProcessRecord, RECORD_SCHEMA, RECORD_VERSION, Sample,
    Source, Unit,
};
#[cfg(unix)]
pub use summary::{
    Distribution, MetricSummary, PairMetric, PairSummary, SUMMARY_SCHEMA, SUMMARY_VERSION,
    Statistic, SubjectSummary, Summary, summarize,
};
pub use usage::{CurrentProcess, Interval, ResourceSource, ResourceUsage};

#[cfg(unix)]
pub use runner::{
    CAMPAIGN_SCHEMA, CAMPAIGN_VERSION, Campaign, CampaignMetadata, CampaignStatus, Cancellation,
    CpuIdentity, HostIdentity, SampleEntry, SignalGuard, read_entries, read_metadata, run,
    summarize_directory,
};
#[cfg(unix)]
pub use workspace::{RunRequest, Workspace};
