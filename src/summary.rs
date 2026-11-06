// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::artifact::ArtifactRole;
use crate::plan::{Mode, PairKind, Plan};
use crate::record::{Direction, Measurement, Metric, Phase, Source, Unit};
use crate::runner::SampleEntry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub const SUMMARY_SCHEMA: &str = "base-benchmark/summary";
pub const SUMMARY_VERSION: u32 = 1;

/// A ratio is computed only when the baseline is at least this many measurement quanta, so quantization moves it by at most 1%.
pub const RESOLUTION_FACTOR: f64 = 100.0;

/// Descriptive statistics regenerated from raw samples. It contains no acceptance verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub schema: String,
    pub version: u32,
    pub mode: Mode,
    pub evidence: String,
    pub scenario: String,
    pub rounds: u32,
    pub seed: u64,
    pub decision_rule: Option<String>,
    /// How processes, samples, and rounds are combined.
    pub aggregation: String,
    pub subjects: Vec<SubjectSummary>,
    pub pairs: Vec<PairSummary>,
    /// Interpretation hazards found in this campaign. They describe the evidence; they are not verdicts.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectSummary {
    pub label: String,
    pub processes: usize,
    pub metrics: Vec<MetricSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricSummary {
    pub name: String,
    pub unit: Unit,
    pub phase: Phase,
    pub source: Source,
    pub direction: Direction,
    pub statistic: Statistic,
    /// Across processes; absent when no process measured the metric.
    pub distribution: Option<Distribution>,
    pub unavailable: usize,
}

/// What a summarized value means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Statistic {
    /// A measured-phase total divided by operations, or a workload-reported mean.
    PerOperation,
    /// A total over a window wider than the measured operations, such as the whole process. It is not divided by operations.
    Total,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distribution {
    pub median: f64,
    pub minimum: f64,
    pub maximum: f64,
}

impl Distribution {
    fn of(values: &mut [f64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        let median = if values.len().is_multiple_of(2) {
            (values[middle - 1] + values[middle]) / 2.0
        } else {
            values[middle]
        };
        Some(Self {
            median,
            minimum: values[0],
            maximum: values[values.len() - 1],
        })
    }
}

/// Per-round comparisons of a candidate with its baseline in the same round.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairSummary {
    pub baseline: String,
    pub candidate: String,
    pub kind: PairKind,
    pub metrics: Vec<PairMetric>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairMetric {
    pub name: String,
    pub unit: Unit,
    pub statistic: Statistic,
    pub direction: Direction,
    /// Rounds in which both subjects measured the metric.
    pub rounds: usize,
    pub improved: usize,
    pub regressed: usize,
    /// `candidate - baseline`, in the metric's statistic.
    pub difference: Option<Distribution>,
    /// `candidate / baseline - 1`, from rounds whose baseline is resolvable.
    pub change: Option<Distribution>,
    /// Rounds without a ratio: the baseline was zero or within [`RESOLUTION_FACTOR`] measurement quanta.
    pub unresolved: usize,
}

type ProcessValues = BTreeMap<String, ProcessValue>;

/// The value one process contributes for each metric. Unavailable metrics contribute no value.
struct ProcessValue {
    metric: Metric,
    statistic: Statistic,
    value: Option<f64>,
    /// The measurement quantum in the same statistic, such as CPU time resolution divided by operations.
    quantum: f64,
    /// The process reported the name with incompatible definitions, so it contributes no value.
    conflict: bool,
}

fn process_values(entry: &SampleEntry) -> ProcessValues {
    let mut grouped: BTreeMap<String, (Metric, Statistic, Vec<f64>, f64, bool)> = BTreeMap::new();
    let samples = entry.record.iter().flat_map(|record| {
        record
            .samples
            .iter()
            .map(|sample| (sample.operations, &sample.metrics))
    });
    let process = std::iter::once((1, &entry.process_metrics));
    for (operations, metrics) in samples.chain(process) {
        for metric in metrics {
            let (statistic, value) = match (metric.per_operation(operations), &metric.measurement) {
                (Some(value), _) => (Statistic::PerOperation, Some(value)),
                (None, Measurement::Total { value }) => (Statistic::Total, Some(*value as f64)),
                (None, _) => (Statistic::PerOperation, None),
            };
            let divisor = if statistic == Statistic::PerOperation {
                operations as f64
            } else {
                1.0
            };
            let quantum = metric.resolution.unwrap_or(0) as f64 / divisor;
            let slot = grouped
                .entry(metric.name.clone())
                .or_insert_with(|| (metric.clone(), statistic, Vec::new(), 0.0, false));
            // A name reused with another unit, phase, or statistic in one process cannot be combined.
            slot.4 |=
                slot.0.unit != metric.unit || slot.0.phase != metric.phase || slot.1 != statistic;
            slot.2.extend(value);
            slot.3 = slot.3.max(quantum);
        }
    }
    grouped
        .into_iter()
        .map(
            |(name, (metric, statistic, mut values, quantum, conflict))| {
                let value = Distribution::of(&mut values)
                    .filter(|_| !conflict)
                    .map(|distribution| distribution.median);
                (
                    name,
                    ProcessValue {
                        metric,
                        statistic,
                        value,
                        quantum,
                        conflict,
                    },
                )
            },
        )
        .collect()
}

/// Paired values of one metric across rounds.
struct Comparison<'a> {
    base: &'a ProcessValue,
    differences: Vec<f64>,
    ratios: Vec<f64>,
    unresolved: usize,
}

/// Summarize complete rounds of `entries` according to `plan`.
pub fn summarize(plan: &Plan, entries: &[SampleEntry]) -> Summary {
    let mut by_subject: BTreeMap<&str, Vec<(u32, ProcessValues)>> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.error.is_none()) {
        by_subject
            .entry(entry.subject.as_str())
            .or_default()
            .push((entry.round, process_values(entry)));
    }
    let mut warnings = Vec::new();

    let subjects = plan
        .subjects
        .iter()
        .map(|subject| {
            let processes = by_subject
                .get(subject.label.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut metrics: BTreeMap<&str, (&ProcessValue, Vec<f64>, usize)> = BTreeMap::new();
            for (_, values) in processes {
                for (name, value) in values {
                    let slot = metrics
                        .entry(name.as_str())
                        .or_insert((value, Vec::new(), 0));
                    if value.conflict {
                        warnings.push(format!(
                            "{}: metric {name} has conflicting definitions within a process and is not summarized",
                            subject.label
                        ));
                    } else if slot.0.metric.unit != value.metric.unit
                        || slot.0.metric.phase != value.metric.phase
                        || slot.0.statistic != value.statistic
                    {
                        warnings.push(format!(
                            "{}: metric {name} changes unit, phase, or statistic between processes",
                            subject.label
                        ));
                    }
                    match value.value {
                        Some(number) => slot.1.push(number),
                        None => slot.2 += 1,
                    }
                }
            }
            SubjectSummary {
                label: subject.label.clone(),
                processes: processes.len(),
                metrics: metrics
                    .into_values()
                    .map(|(first, mut values, unavailable)| MetricSummary {
                        name: first.metric.name.clone(),
                        unit: first.metric.unit,
                        phase: first.metric.phase,
                        source: first.metric.source,
                        direction: first.metric.direction,
                        statistic: first.statistic,
                        distribution: Distribution::of(&mut values),
                        unavailable,
                    })
                    .collect(),
            }
        })
        .collect();

    let pairs: Vec<PairSummary> = plan
        .pairs
        .iter()
        .map(|pair| {
            let rounds = |label: &str| -> BTreeMap<u32, &ProcessValues> {
                by_subject
                    .get(label)
                    .into_iter()
                    .flatten()
                    .map(|(round, values)| (*round, values))
                    .collect()
            };
            let baseline = rounds(&pair.baseline);
            let candidate = rounds(&pair.candidate);
            let mut comparisons: BTreeMap<&str, Comparison> = BTreeMap::new();
            for (round, baseline_values) in &baseline {
                let Some(candidate_values) = candidate.get(round) else {
                    continue;
                };
                for (name, base) in baseline_values.iter() {
                    let Some(other) = candidate_values.get(name) else {
                        continue;
                    };
                    let slot = comparisons.entry(name.as_str()).or_insert(Comparison {
                        base,
                        differences: Vec::new(),
                        ratios: Vec::new(),
                        unresolved: 0,
                    });
                    if let (Some(before), Some(after)) = (base.value, other.value) {
                        if base.statistic == other.statistic {
                            slot.differences.push(after - before);
                            if before > 0.0
                                && before >= RESOLUTION_FACTOR * base.quantum.max(other.quantum)
                            {
                                slot.ratios.push(after / before - 1.0);
                            } else {
                                slot.unresolved += 1;
                            }
                        }
                    }
                }
            }
            PairSummary {
                baseline: pair.baseline.clone(),
                candidate: pair.candidate.clone(),
                kind: pair.kind,
                metrics: comparisons
                    .into_values()
                    .map(Comparison::into_metric)
                    .collect(),
            }
        })
        .collect();

    warnings.extend(interpretation_warnings(plan, &pairs));
    Summary {
        schema: SUMMARY_SCHEMA.to_owned(),
        version: SUMMARY_VERSION,
        mode: plan.mode,
        evidence: plan.mode.evidence().to_owned(),
        scenario: plan.scenario.clone(),
        rounds: plan.rounds,
        seed: plan.seed,
        decision_rule: plan.decision_rule.clone(),
        aggregation: "each process contributes the median of its samples; subject distributions are across processes; pair differences and changes are per round".to_owned(),
        subjects,
        pairs,
        warnings,
    }
}

impl Comparison<'_> {
    fn into_metric(mut self) -> PairMetric {
        let direction = self.base.metric.direction;
        let better = |difference: &&f64| match direction {
            Direction::LowerIsBetter => **difference < 0.0,
            Direction::HigherIsBetter => **difference > 0.0,
            Direction::Neutral => false,
        };
        let worse = |difference: &&f64| {
            direction != Direction::Neutral && **difference != 0.0 && !better(difference)
        };
        PairMetric {
            name: self.base.metric.name.clone(),
            unit: self.base.metric.unit,
            statistic: self.base.statistic,
            direction,
            rounds: self.differences.len(),
            improved: self.differences.iter().filter(better).count(),
            regressed: self.differences.iter().filter(worse).count(),
            difference: Distribution::of(&mut self.differences),
            change: Distribution::of(&mut self.ratios),
            unresolved: self.unresolved,
        }
    }
}

/// Hazards that make a comparison weaker than its numbers suggest.
fn interpretation_warnings(plan: &Plan, pairs: &[PairSummary]) -> Vec<String> {
    let mut warnings = Vec::new();
    if plan.mode != Mode::Smoke && plan.cpus.is_empty() {
        warnings.push("no CPU placement: workload processes may run on different cores or core types between rounds".to_owned());
    }
    let timed = |metric: &&PairMetric| {
        metric.direction != Direction::Neutral && metric.unit == Unit::Nanoseconds
    };
    for pair in pairs.iter().filter(|pair| pair.kind != PairKind::Replica) {
        for metric in pair
            .metrics
            .iter()
            .filter(timed)
            .filter(|metric| metric.improved > 0 && metric.regressed > 0)
        {
            warnings.push(format!(
                "{} -> {}: {} changed in both directions (better {}/{}, worse {}/{}); inconclusive",
                pair.baseline,
                pair.candidate,
                metric.name,
                metric.improved,
                metric.rounds,
                metric.regressed,
                metric.rounds
            ));
        }
    }
    for pair in pairs.iter().filter(|pair| pair.kind == PairKind::Build) {
        let Some(replica) = pairs
            .iter()
            .find(|other| other.kind == PairKind::Replica && other.baseline == pair.baseline)
        else {
            warnings.push(format!(
                "{} -> {}: no layout replica, so build-layout variation is not controlled",
                pair.baseline, pair.candidate
            ));
            continue;
        };
        for metric in pair.metrics.iter().filter(timed) {
            let (Some(change), Some(control)) = (
                metric.change,
                replica
                    .metrics
                    .iter()
                    .find(|other| other.name == metric.name)
                    .and_then(|other| other.change),
            ) else {
                continue;
            };
            let bound = control.minimum.abs().max(control.maximum.abs());
            if change.median.abs() <= bound {
                warnings.push(format!(
                    "{} -> {}: {} median change {:+.2}% is within the layout replica's range of ±{:.2}%",
                    pair.baseline,
                    pair.candidate,
                    metric.name,
                    change.median * 100.0,
                    bound * 100.0
                ));
            }
        }
    }
    // A variant comparison inside one executable can also depend on that executable's layout.
    for replica in plan
        .artifacts
        .artifacts
        .iter()
        .filter(|artifact| artifact.role == ArtifactRole::LayoutReplica)
    {
        let original = replica.source.replica_of.as_deref().unwrap_or_default();
        let relabel = |label: &str| {
            label
                .strip_prefix(original)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(|variant| format!("{}/{variant}", replica.label))
        };
        for pair in pairs.iter().filter(|pair| pair.kind == PairKind::Variant) {
            // Pairs of the replica itself do not relabel and have no control.
            let control = relabel(&pair.baseline)
                .zip(relabel(&pair.candidate))
                .and_then(|(baseline, candidate)| {
                    pairs
                        .iter()
                        .find(|other| other.baseline == baseline && other.candidate == candidate)
                });
            let Some(control) = control else {
                continue;
            };
            for metric in pair.metrics.iter().filter(timed) {
                let (Some(change), Some(other)) = (
                    metric.change,
                    control
                        .metrics
                        .iter()
                        .find(|other| other.name == metric.name)
                        .and_then(|other| other.change),
                ) else {
                    continue;
                };
                if change.median * other.median < 0.0 {
                    warnings.push(format!(
                        "{} -> {}: {} median change {:+.2}% has the opposite sign in the layout replica ({:+.2}%); the conclusion depends on build layout",
                        pair.baseline,
                        pair.candidate,
                        metric.name,
                        change.median * 100.0,
                        other.median * 100.0
                    ));
                }
            }
        }
    }
    warnings
}

impl Summary {
    /// A compact text report: evidence class, pair comparisons, subject medians, and warnings. Smoke reports omit pair comparisons.
    pub fn report(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "{}", self.evidence);
        let _ = writeln!(
            text,
            "scenario {}: {} round(s), seed {}",
            self.scenario, self.rounds, self.seed
        );
        if let Some(rule) = &self.decision_rule {
            let _ = writeln!(text, "decision rule: {rule}");
        }
        if self.mode == Mode::Smoke {
            let _ = writeln!(text, "pair comparisons are omitted from smoke reports");
        } else {
            for pair in &self.pairs {
                let _ = writeln!(
                    text,
                    "\n{:?} {} -> {}",
                    pair.kind, pair.baseline, pair.candidate
                );
                for metric in pair
                    .metrics
                    .iter()
                    .filter(|metric| metric.direction != Direction::Neutral)
                {
                    report_pair_metric(&mut text, metric);
                }
            }
        }
        for subject in &self.subjects {
            let _ = writeln!(
                text,
                "\n{} ({} process(es))",
                subject.label, subject.processes
            );
            for metric in &subject.metrics {
                let suffix = suffix(metric.statistic, metric.unit);
                match metric.distribution {
                    Some(value) => {
                        let _ = writeln!(
                            text,
                            "  {:<32} {:.3} {suffix} [{:.3}, {:.3}]",
                            metric.name, value.median, value.minimum, value.maximum
                        );
                    }
                    None => {
                        let _ = writeln!(text, "  {:<32} unavailable", metric.name);
                    }
                }
            }
        }
        for warning in &self.warnings {
            let _ = writeln!(text, "\nwarning: {warning}");
        }
        text
    }
}

/// Counts are shown as absolute differences: ratios of small counts are dominated by single events.
fn report_pair_metric(text: &mut String, metric: &PairMetric) {
    let name = format!(
        "{}{}",
        metric.name,
        if metric.statistic == Statistic::Total {
            " (total)"
        } else {
            ""
        }
    );
    let value = match (metric.unit, metric.change, metric.difference) {
        (Unit::Count, _, Some(difference)) | (_, None, Some(difference)) => {
            let suffix = suffix(metric.statistic, metric.unit);
            format!(
                "{:+.3} [{:+.3}, {:+.3}] {suffix}",
                difference.median, difference.minimum, difference.maximum
            )
        }
        (_, Some(change), _) => format!(
            "{:+.2}% [{:+.2}%, {:+.2}%]",
            change.median * 100.0,
            change.minimum * 100.0,
            change.maximum * 100.0
        ),
        (_, None, None) => return,
    };
    let unresolved = if metric.unresolved > 0 && metric.unit != Unit::Count {
        format!(", {} round(s) below resolution", metric.unresolved)
    } else {
        String::new()
    };
    let _ = writeln!(
        text,
        "  {name:<32} {value}  better {}/{} worse {}/{}{unresolved}",
        metric.improved, metric.rounds, metric.regressed, metric.rounds
    );
}

fn suffix(statistic: Statistic, unit: Unit) -> &'static str {
    match (statistic, unit) {
        (Statistic::PerOperation, Unit::Nanoseconds) => "ns/op",
        (Statistic::PerOperation, Unit::Count) => "/op",
        (Statistic::PerOperation, Unit::Bytes) => "B/op",
        (Statistic::Total, Unit::Nanoseconds) => "ns total",
        (Statistic::Total, Unit::Count) => "total",
        (Statistic::Total, Unit::Bytes) => "B total",
    }
}

#[cfg(test)]
mod tests;
