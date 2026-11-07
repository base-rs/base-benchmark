// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

mod host;
mod lock;
mod perf;
mod process;

pub use host::{CpuIdentity, HostIdentity};
#[cfg(not(target_os = "linux"))]
pub(crate) use process::check_collection;
pub use process::{Cancellation, SignalGuard};

use crate::artifact::write_json;
use crate::plan::{Plan, STACK_PADDING_LIMIT, STACK_PADDING_VARIABLE, Shuffle, Subject};
use crate::record::{Metric, ProcessRecord, invalid};
use crate::summary::{Summary, summarize};
use lock::HostLock;
use process::Launch;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CAMPAIGN_SCHEMA: &str = "base-benchmark/campaign";
pub const CAMPAIGN_VERSION: u32 = 1;

/// `metadata.json`: the plan, host, and completion state of a campaign directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignMetadata {
    pub schema: String,
    pub version: u32,
    pub status: CampaignStatus,
    pub evidence: String,
    /// The tool version that collected the campaign.
    pub generator: String,
    pub plan: Plan,
    pub host: HostIdentity,
    pub started_unix_ms: u64,
    pub finished_unix_ms: Option<u64>,
    pub error: Option<String>,
}

/// Only complete campaigns have summaries. An `in_progress` campaign whose collector is gone was interrupted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    InProgress,
    Failed,
    Cancelled,
    Complete,
}

/// One line of `samples.jsonl`. Failed samples are retained as evidence and stop the campaign.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleEntry {
    pub round: u32,
    pub position: usize,
    pub subject: String,
    pub command: Vec<String>,
    pub cpus: Vec<usize>,
    /// The length of the random stack-padding variable, when padding is enabled.
    pub stack_padding: Option<usize>,
    pub executable_sha256: String,
    pub duration_ns: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub error: Option<String>,
    /// The end of the workload's stderr.
    pub stderr: String,
    pub record: Option<ProcessRecord>,
    /// Whole-process metrics collected around the workload, such as `perf stat` counters.
    pub process_metrics: Vec<Metric>,
}

/// A finished campaign directory and its summary.
///
/// Serialized results are deliberately compact: the directory, evidence class, and warnings. The complete summary is in `summary.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Campaign {
    pub output: PathBuf,
    pub summary_path: PathBuf,
    pub evidence: String,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub summary: Summary,
    /// The human-readable summary also printed by the CLI.
    #[serde(skip)]
    pub report: String,
}

impl Campaign {
    fn new(output: &Path, summary: Summary) -> Self {
        Self {
            output: output.to_owned(),
            summary_path: output.join("summary.json"),
            evidence: summary.evidence.clone(),
            warnings: summary.warnings.clone(),
            report: summary.report(),
            summary,
        }
    }
}

/// Run every subject once per round, in a seeded random order, under the host lock.
///
/// `output` must be a new directory. It receives `metadata.json`, an append-only `samples.jsonl`, and on success `summary.json`. Any invalid sample stops the campaign and leaves `failed` metadata; partial results never become a summary. Collection needs Linux; elsewhere this returns an `Unsupported` error before creating anything.
pub fn run(
    plan: Plan,
    output: &Path,
    cancellation: &Cancellation,
    progress: &mut dyn Write,
) -> io::Result<Campaign> {
    #[cfg(not(target_os = "linux"))]
    check_collection()?;
    plan.check_prerequisites()?;
    let metadata = CampaignMetadata {
        schema: CAMPAIGN_SCHEMA.to_owned(),
        version: CAMPAIGN_VERSION,
        status: CampaignStatus::InProgress,
        evidence: plan.mode.evidence().to_owned(),
        generator: crate::summary::GENERATOR.to_owned(),
        host: HostIdentity::capture(&plan.cpus),
        plan,
        started_unix_ms: unix_ms(),
        finished_unix_ms: None,
        error: None,
    };
    let journal = Journal::create(output, metadata)?;
    match collect(&journal, cancellation, progress) {
        Ok(summary) => {
            journal.complete(&summary)?;
            Ok(Campaign::new(output, summary))
        }
        Err(error) => {
            let status = if cancellation.is_cancelled() {
                CampaignStatus::Cancelled
            } else {
                CampaignStatus::Failed
            };
            Err(journal.fail(status, error))
        }
    }
}

/// Regenerate `summary.json` from the raw samples of a complete campaign.
pub fn summarize_directory(output: &Path) -> io::Result<Campaign> {
    let metadata = read_metadata(output)?;
    if metadata.status != CampaignStatus::Complete {
        return Err(invalid(format!(
            "{}: campaign status is {:?}; only complete campaigns are summarized",
            output.display(),
            metadata.status
        )));
    }
    let entries = read_entries(&output.join("samples.jsonl"))?;
    let summary = summarize(&metadata.plan, &entries);
    write_json(&output.join("summary.json"), &summary)?;
    Ok(Campaign::new(output, summary))
}

pub fn read_metadata(output: &Path) -> io::Result<CampaignMetadata> {
    let path = output.join("metadata.json");
    let text = std::fs::read(&path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
    serde_json::from_slice(&text).map_err(|error| invalid(format!("{}: {error}", path.display())))
}

pub fn read_entries(path: &Path) -> io::Result<Vec<SampleEntry>> {
    let file = File::open(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
    let mut entries = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        entries.push(
            serde_json::from_str(&line).map_err(|error| {
                invalid(format!("{} line {}: {error}", path.display(), index + 1))
            })?,
        );
    }
    Ok(entries)
}

fn collect(
    journal: &Journal,
    cancellation: &Cancellation,
    progress: &mut dyn Write,
) -> io::Result<Summary> {
    let plan = journal.metadata.plan.clone();
    let staged = stage(&plan, &journal.output)?;
    let _lock = HostLock::acquire(&plan.lock, cancellation, progress)?;
    let collection = Collection {
        plan: &plan,
        output: &journal.output,
        staged: &staged,
        cancellation,
    };
    let mut shuffle = Shuffle::new(plan.seed);
    // A separate stream, so padding does not change the round order of a given seed.
    let mut padding = Shuffle::new(plan.seed ^ 0x5851_f42d_4c95_7f2d);
    let mut expected = Expectations::default();
    let mut entries = Vec::new();
    let count = plan.subjects.len();
    for round in 1..=plan.rounds {
        for (position, index) in shuffle.order(count).into_iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err(cancelled());
            }
            let subject = &plan.subjects[index];
            let _ = writeln!(
                progress,
                "base-benchmark: round {round}/{} [{}/{count}] {}",
                plan.rounds,
                position + 1,
                subject.label
            );
            let stack_padding = plan
                .stack_padding
                .then(|| padding.below(STACK_PADDING_LIMIT));
            let entry = collection.sample(subject, round, position, stack_padding, &mut expected);
            journal.append(&entry)?;
            if let Some(error) = &entry.error {
                if cancellation.is_cancelled() {
                    return Err(cancelled());
                }
                return Err(invalid(format!(
                    "{} in round {round}: {error}",
                    subject.label
                )));
            }
            entries.push(entry);
        }
    }
    Ok(summarize(&plan, &entries))
}

/// Link every frozen executable into `bin/` under equal-length names, falling back to a copy.
///
/// The executable path is copied onto each process's initial stack, so artifacts with labels of different lengths would otherwise start with differently aligned stacks.
fn stage(plan: &Plan, output: &Path) -> io::Result<BTreeMap<String, PathBuf>> {
    let directory = output.join("bin");
    let width = plan.artifacts.artifacts.len().to_string().len();
    std::fs::create_dir(&directory).and_then(|()| {
        plan.artifacts
            .artifacts
            .iter()
            .enumerate()
            .map(|(index, artifact)| {
                let path = directory.join(format!("{index:0width$}"));
                std::fs::hard_link(&artifact.executable, &path)
                    .or_else(|_| std::fs::copy(&artifact.executable, &path).map(drop))
                    .map_err(|error| {
                        io::Error::new(
                            error.kind(),
                            format!(
                                "{}: cannot stage {}: {error}",
                                artifact.label,
                                artifact.executable.display()
                            ),
                        )
                    })?;
                Ok((artifact.label.clone(), path))
            })
            .collect()
    })
}

/// The fixed context of one campaign's samples.
struct Collection<'a> {
    plan: &'a Plan,
    output: &'a Path,
    staged: &'a BTreeMap<String, PathBuf>,
    cancellation: &'a Cancellation,
}

impl Collection<'_> {
    fn sample(
        &self,
        subject: &Subject,
        round: u32,
        position: usize,
        stack_padding: Option<usize>,
        expected: &mut Expectations,
    ) -> SampleEntry {
        let mut entry = SampleEntry {
            round,
            position,
            subject: subject.label.clone(),
            command: Vec::new(),
            cpus: self.plan.cpus.clone(),
            stack_padding,
            executable_sha256: String::new(),
            duration_ns: 0,
            exit_code: None,
            signal: None,
            timed_out: false,
            error: None,
            stderr: String::new(),
            record: None,
            process_metrics: Vec::new(),
        };
        if let Err(error) = self.collect_sample(subject, expected, &mut entry) {
            entry.error = Some(error.to_string());
        }
        entry
    }

    fn collect_sample(
        &self,
        subject: &Subject,
        expected: &mut Expectations,
        entry: &mut SampleEntry,
    ) -> io::Result<()> {
        let plan = self.plan;
        let artifact = plan
            .artifacts
            .artifacts
            .iter()
            .find(|artifact| artifact.label == subject.artifact)
            .expect("plans only name their own artifacts");
        let executable = &self.staged[&subject.artifact];
        entry.executable_sha256 = artifact.sha256.clone();
        artifact.verify_at(executable)?;
        let counters = self
            .output
            .join(format!("perf-{}-{}.csv", entry.round, entry.position));
        let (program, arguments) = if plan.perf_events.is_empty() {
            (executable.clone(), subject.arguments.clone())
        } else {
            perf::wrap(
                &plan.perf_program,
                &plan.perf_events,
                &counters,
                executable,
                &subject.arguments,
            )
        };
        entry.command = std::iter::once(program.display().to_string())
            .chain(arguments.iter().cloned())
            .collect();
        let mut environment = plan.environment.clone();
        if let Some(length) = entry.stack_padding {
            environment.insert(STACK_PADDING_VARIABLE.to_owned(), "x".repeat(length));
        }
        let launch = Launch {
            program: &program,
            arguments: &arguments,
            environment: &environment,
            directory: &plan.working_directory,
            cpus: &plan.cpus,
        };
        let execution = process::execute(
            &launch,
            Duration::from_millis(plan.timeout_ms),
            self.cancellation,
        )?;
        entry.duration_ns = execution.duration_ns;
        entry.exit_code = execution.code;
        entry.signal = execution.signal;
        entry.timed_out = execution.timed_out;
        entry.stderr = String::from_utf8_lossy(&execution.stderr).into_owned();
        if execution.cancelled {
            return Err(cancelled());
        }
        if execution.timed_out {
            return Err(invalid(format!("timed out after {} ms", plan.timeout_ms)));
        }
        if execution.escaped {
            return Err(invalid(
                "a descendant that left the workload's process group kept its output open; it was not killed and may still be running",
            ));
        }
        if !execution.succeeded() {
            return Err(invalid(format!(
                "workload failed with exit code {:?}, signal {:?}",
                execution.code, execution.signal
            )));
        }
        if execution.stdout_truncated {
            return Err(invalid("workload stdout exceeded the record size limit"));
        }
        let record: ProcessRecord = serde_json::from_slice(&execution.stdout).map_err(|error| {
            invalid(format!("stdout is not exactly one process record: {error}"))
        })?;
        record.validate()?;
        entry.record = Some(record);
        let record = entry.record.as_ref().expect("the record was just stored");
        expected.check(subject, record)?;
        if !plan.perf_events.is_empty() {
            let text = std::fs::read_to_string(&counters)
                .map_err(|error| invalid(format!("perf stat wrote no counters: {error}")))?;
            entry.process_metrics = perf::parse(&text, &plan.perf_events)?;
        }
        Ok(())
    }
}

/// What every later record of the campaign must match: the first record's workload and signature, and each variant's parameters.
#[derive(Default)]
struct Expectations {
    identity: Option<(String, Option<String>)>,
    parameters: BTreeMap<Option<String>, serde_json::Value>,
}

impl Expectations {
    fn check(&mut self, subject: &Subject, record: &ProcessRecord) -> io::Result<()> {
        let identity = (record.workload.clone(), record.signature.clone());
        match &self.identity {
            Some(expected) if *expected != identity => {
                return Err(invalid(format!(
                    "workload {:?} with signature {:?} differs from {:?} with {:?}: the subjects did not do equivalent work",
                    identity.0, identity.1, expected.0, expected.1
                )));
            }
            Some(_) => {}
            None => self.identity = Some(identity),
        }
        // Every artifact must run a variant with the same parameters, or a changed default would be compared silently.
        let parameters = self
            .parameters
            .entry(subject.variant.clone())
            .or_insert_with(|| record.parameters.clone());
        if *parameters != record.parameters {
            return Err(invalid(format!(
                "{}: parameters differ from another run of the same variant: {} versus {parameters}",
                subject.label, record.parameters
            )));
        }
        Ok(())
    }
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "the campaign was cancelled")
}

/// The campaign directory: metadata rewritten at each state change, and samples appended as they are collected.
struct Journal {
    output: PathBuf,
    metadata: CampaignMetadata,
}

impl Journal {
    /// Create a new campaign directory with `in_progress` metadata and an empty sample journal.
    fn create(output: &Path, metadata: CampaignMetadata) -> io::Result<Self> {
        let journal = Self {
            output: output.to_owned(),
            metadata,
        };
        std::fs::create_dir_all(output.parent().unwrap_or(output))
            .and_then(|()| std::fs::create_dir(output))
            .and_then(|()| journal.write_metadata())
            .and_then(|()| File::create_new(output.join("samples.jsonl")).map(drop))
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "{}: {error}; campaigns need a new directory",
                        output.display()
                    ),
                )
            })?;
        Ok(journal)
    }

    /// Append one entry, reopening the journal so each line is complete on disk before the next sample starts.
    fn append(&self, entry: &SampleEntry) -> io::Result<()> {
        let mut line = serde_json::to_vec(entry).expect("sample entries serialize");
        line.push(b'\n');
        let path = self.output.join("samples.jsonl");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(&line))
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
    }

    fn complete(mut self, summary: &Summary) -> io::Result<()> {
        self.metadata.status = CampaignStatus::Complete;
        self.metadata.finished_unix_ms = Some(unix_ms());
        write_json(&self.output.join("summary.json"), summary).and_then(|()| self.write_metadata())
    }

    /// Record the failure. The original error is returned, extended if the failure itself could not be recorded.
    fn fail(mut self, status: CampaignStatus, error: io::Error) -> io::Error {
        self.metadata.status = status;
        self.metadata.error = Some(error.to_string());
        self.metadata.finished_unix_ms = Some(unix_ms());
        match self.write_metadata() {
            Ok(()) => error,
            Err(secondary) => io::Error::new(
                error.kind(),
                format!("{error}; recording the failure also failed: {secondary}"),
            ),
        }
    }

    fn write_metadata(&self) -> io::Result<()> {
        write_json(&self.output.join("metadata.json"), &self.metadata)
    }
}

pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
