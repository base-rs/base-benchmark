// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::plan::{Mode, PairKind, Plan};
use crate::record::{Direction, Measurement, Metric, Phase, Source, Unit};
use crate::runner::SampleEntry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub const SUMMARY_SCHEMA: &str = "base-benchmark/summary";
pub const SUMMARY_VERSION: u32 = 1;

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
    /// How a process with several samples is reduced to one value.
    pub aggregation: String,
    pub subjects: Vec<SubjectSummary>,
    pub pairs: Vec<PairSummary>,
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

/// Per-round changes of a candidate relative to its baseline: `candidate / baseline - 1`.
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
    pub statistic: Statistic,
    pub direction: Direction,
    /// Rounds in which both subjects measured the metric and the baseline was nonzero.
    pub rounds: usize,
    pub improved: usize,
    pub regressed: usize,
    pub change: Option<Distribution>,
}

type ProcessValues = BTreeMap<String, ProcessValue>;

/// The value one process contributes for each metric. Unavailable metrics contribute no value.
struct ProcessValue {
    metric: Metric,
    statistic: Statistic,
    value: Option<f64>,
}

fn process_values(entry: &SampleEntry) -> ProcessValues {
    let mut grouped: BTreeMap<String, (Metric, Statistic, Vec<f64>)> = BTreeMap::new();
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
            let slot = grouped
                .entry(metric.name.clone())
                .or_insert_with(|| (metric.clone(), statistic, Vec::new()));
            slot.2.extend(value);
        }
    }
    grouped
        .into_iter()
        .map(|(name, (metric, statistic, mut values))| {
            let value = Distribution::of(&mut values).map(|distribution| distribution.median);
            (
                name,
                ProcessValue {
                    metric,
                    statistic,
                    value,
                },
            )
        })
        .collect()
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
                    if slot.0.metric.unit != value.metric.unit
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
            let mut changes: BTreeMap<&str, (&ProcessValue, Vec<f64>)> = BTreeMap::new();
            for (round, baseline_values) in &baseline {
                let Some(candidate_values) = candidate.get(round) else {
                    continue;
                };
                for (name, base) in baseline_values.iter() {
                    let Some(other) = candidate_values.get(name) else {
                        continue;
                    };
                    let slot = changes.entry(name.as_str()).or_insert((base, Vec::new()));
                    if let (Some(before), Some(after)) = (base.value, other.value) {
                        if before != 0.0 && base.statistic == other.statistic {
                            slot.1.push(after / before - 1.0);
                        }
                    }
                }
            }
            PairSummary {
                baseline: pair.baseline.clone(),
                candidate: pair.candidate.clone(),
                kind: pair.kind,
                metrics: changes
                    .into_values()
                    .map(|(base, mut values)| {
                        let direction = base.metric.direction;
                        let better = |change: &f64| match direction {
                            Direction::LowerIsBetter => *change < 0.0,
                            Direction::HigherIsBetter => *change > 0.0,
                            Direction::Neutral => false,
                        };
                        let worse = |change: &f64| {
                            direction != Direction::Neutral && *change != 0.0 && !better(change)
                        };
                        PairMetric {
                            name: base.metric.name.clone(),
                            statistic: base.statistic,
                            direction,
                            rounds: values.len(),
                            improved: values.iter().filter(|change| better(change)).count(),
                            regressed: values.iter().filter(|change| worse(change)).count(),
                            change: Distribution::of(&mut values),
                        }
                    })
                    .collect(),
            }
        })
        .collect();

    warnings.extend(replica_warnings(&pairs));
    Summary {
        schema: SUMMARY_SCHEMA.to_owned(),
        version: SUMMARY_VERSION,
        mode: plan.mode,
        evidence: plan.mode.evidence().to_owned(),
        scenario: plan.scenario.clone(),
        rounds: plan.rounds,
        seed: plan.seed,
        decision_rule: plan.decision_rule.clone(),
        aggregation: "each process contributes the median of its samples; distributions are across processes; pair changes are per round".to_owned(),
        subjects,
        pairs,
        warnings,
    }
}

/// Compare every build change with the layout replica of the same baseline subject.
fn replica_warnings(pairs: &[PairSummary]) -> Vec<String> {
    let mut warnings = Vec::new();
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
        for metric in pair
            .metrics
            .iter()
            .filter(|metric| metric.direction != Direction::Neutral)
        {
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
    warnings
}

impl Summary {
    /// A compact text report: evidence class, pairs, subject medians, and warnings.
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
                let Some(change) = metric.change else {
                    continue;
                };
                let _ = writeln!(
                    text,
                    "  {:<32} {:+8.2}% [{:+.2}%, {:+.2}%]  better {}/{} worse {}/{}",
                    format!(
                        "{}{}",
                        metric.name,
                        if metric.statistic == Statistic::Total {
                            " (total)"
                        } else {
                            ""
                        }
                    ),
                    change.median * 100.0,
                    change.minimum * 100.0,
                    change.maximum * 100.0,
                    metric.improved,
                    metric.rounds,
                    metric.regressed,
                    metric.rounds
                );
            }
        }
        for subject in &self.subjects {
            let _ = writeln!(
                text,
                "\n{} ({} process(es))",
                subject.label, subject.processes
            );
            for metric in &subject.metrics {
                let suffix = match (metric.statistic, metric.unit) {
                    (Statistic::PerOperation, Unit::Nanoseconds) => "ns/op",
                    (Statistic::PerOperation, Unit::Count) => "/op",
                    (Statistic::PerOperation, Unit::Bytes) => "B/op",
                    (Statistic::Total, Unit::Nanoseconds) => "ns total",
                    (Statistic::Total, Unit::Count) => "total",
                    (Statistic::Total, Unit::Bytes) => "B total",
                };
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

#[cfg(test)]
mod tests;
