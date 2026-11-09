// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

/// The schema identifier of a [`ProcessRecord`].
pub const RECORD_SCHEMA: &str = "base-benchmark/process-record";
/// The current [`ProcessRecord`] schema version. Version 2 added quantile measurements; version 1 records, defined before them, are still accepted.
pub const RECORD_VERSION: u32 = 2;

/// The single record a workload process writes to stdout on success.
///
/// A record contains one or more ordered samples, each with an explicit operation count. The runner rejects malformed records, so a workload reports failure through a nonzero exit status rather than a partial record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRecord {
    pub schema: String,
    pub version: u32,
    /// A stable workload identity, such as `base-html/rendering`.
    pub workload: String,
    /// The complete parsed workload configuration, as a JSON object.
    pub parameters: Value,
    /// Identifies the verified output, so comparisons can reject variants that did different work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub samples: Vec<Sample>,
    /// Project-owned values that the shared tooling preserves without interpreting.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
}

impl ProcessRecord {
    /// Create a record for `workload` with its complete `parameters` object.
    pub fn new(workload: impl Into<String>, parameters: Value) -> Self {
        Self {
            schema: RECORD_SCHEMA.to_owned(),
            version: RECORD_VERSION,
            workload: workload.into(),
            parameters,
            signature: None,
            samples: Vec::new(),
            extensions: BTreeMap::new(),
        }
    }

    /// Identify the verified output of the measured work.
    pub fn with_signature(mut self, signature: impl Into<String>) -> Self {
        self.signature = Some(signature.into());
        self
    }

    pub fn with_sample(mut self, sample: Sample) -> Self {
        self.samples.push(sample);
        self
    }

    pub fn with_extension(mut self, name: impl Into<String>, value: Value) -> Self {
        self.extensions.insert(name.into(), value);
        self
    }

    /// Check structural invariants. The runner applies the same checks to every record it receives.
    pub fn validate(&self) -> io::Result<()> {
        if self.schema != RECORD_SCHEMA || !(1..=RECORD_VERSION).contains(&self.version) {
            return Err(invalid(format!(
                "unsupported record schema {} version {}",
                self.schema, self.version
            )));
        }
        if self.workload.trim().is_empty() {
            return Err(invalid("record workload is empty"));
        }
        if !self.parameters.is_object() {
            return Err(invalid("record parameters must be a JSON object"));
        }
        if self
            .signature
            .as_deref()
            .is_some_and(|signature| signature.trim().is_empty())
        {
            return Err(invalid("record signature is empty"));
        }
        if self.samples.is_empty() {
            return Err(invalid("record contains no samples"));
        }
        for (index, sample) in self.samples.iter().enumerate() {
            sample
                .validate()
                .map_err(|error| invalid(format!("sample {index}: {error}")))?;
        }
        Ok(())
    }

    /// Validate and write the record as one JSON line.
    pub fn emit(&self, out: &mut impl Write) -> io::Result<()> {
        self.validate()?;
        let mut line = serde_json::to_vec(self).expect("validated records serialize");
        line.push(b'\n');
        out.write_all(&line)?;
        out.flush()
    }
}

/// One measured batch. Totals in the measured phase are normalized by `operations`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub operations: u64,
    pub metrics: Vec<Metric>,
}

impl Sample {
    pub fn new(operations: u64) -> Self {
        Self {
            operations,
            metrics: Vec::new(),
        }
    }

    pub fn with(mut self, metric: Metric) -> Self {
        self.metrics.push(metric);
        self
    }

    pub fn push(&mut self, metric: Metric) {
        self.metrics.push(metric);
    }

    pub fn metric(&self, name: &str) -> Option<&Metric> {
        self.metrics.iter().find(|metric| metric.name == name)
    }

    fn validate(&self) -> io::Result<()> {
        if self.operations == 0 {
            return Err(invalid("operations must be positive"));
        }
        let mut names = BTreeSet::new();
        for metric in &self.metrics {
            if !names.insert(metric.name.as_str()) {
                return Err(invalid(format!("duplicate metric {}", metric.name)));
            }
            metric.validate()?;
        }
        Ok(())
    }
}

/// A named value with explicit unit, capture window, source, and improvement direction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub name: String,
    pub unit: Unit,
    pub phase: Phase,
    pub source: Source,
    pub direction: Direction,
    pub measurement: Measurement,
    /// Sampling resolution in `unit`, when coarser than one unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u64>,
}

impl Metric {
    /// A total accumulated during the measured phase, reported by the workload. Summaries divide it by the sample's operations.
    pub fn total(name: impl Into<String>, unit: Unit, value: u64) -> Self {
        Self::new(name, unit, Measurement::Total { value })
    }

    /// A value that is already per operation or otherwise not divisible, such as a mean latency.
    pub fn mean(name: impl Into<String>, unit: Unit, value: f64) -> Self {
        Self::new(name, unit, Measurement::Mean { value })
    }

    /// The `quantile` (from 0 to 1) of a distribution the workload observed, such as the 99th percentile latency, 0.99. Like a mean, it is reported as it is and never divided by operations.
    pub fn quantile(name: impl Into<String>, unit: Unit, quantile: f64, value: f64) -> Self {
        // Adding zero turns -0.0 into 0.0, so the minimum has one representation.
        Self::new(
            name,
            unit,
            Measurement::Quantile {
                quantile: quantile + 0.0,
                value,
            },
        )
    }

    /// A metric that was requested but could not be measured.
    pub fn unavailable(name: impl Into<String>, unit: Unit, reason: impl Into<String>) -> Self {
        Self::new(
            name,
            unit,
            Measurement::Unavailable {
                reason: reason.into(),
            },
        )
    }

    fn new(name: impl Into<String>, unit: Unit, measurement: Measurement) -> Self {
        Self {
            name: name.into(),
            unit,
            phase: Phase::Measured,
            source: Source::Workload,
            direction: Direction::Neutral,
            measurement,
            resolution: None,
        }
    }

    pub fn with_phase(mut self, phase: Phase) -> Self {
        self.phase = phase;
        self
    }

    pub fn with_source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    pub fn with_resolution(mut self, resolution: u64) -> Self {
        self.resolution = Some(resolution);
        self
    }

    pub fn lower_is_better(mut self) -> Self {
        self.direction = Direction::LowerIsBetter;
        self
    }

    pub fn higher_is_better(mut self) -> Self {
        self.direction = Direction::HigherIsBetter;
        self
    }

    /// The per-operation value of a measured-phase total.
    ///
    /// Whole-process and attached-window totals include work outside the measured operations, so they are never normalized by them. Means and quantiles are reported as they are.
    pub fn per_operation(&self, operations: u64) -> Option<f64> {
        match (&self.measurement, self.phase) {
            (Measurement::Total { value }, Phase::Measured) => {
                Some(*value as f64 / operations as f64)
            }
            _ => None,
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.name.trim().is_empty() {
            return Err(invalid("metric name is empty"));
        }
        if self.phase == Phase::Window {
            return Err(invalid(format!(
                "{}: window metrics are measured by the runner, not reported in records",
                self.name
            )));
        }
        if self.resolution == Some(0) {
            return Err(invalid(format!(
                "{}: resolution must be positive",
                self.name
            )));
        }
        match &self.measurement {
            Measurement::Total { .. } => Ok(()),
            Measurement::Mean { value } if value.is_finite() && *value >= 0.0 => Ok(()),
            Measurement::Mean { .. } => Err(invalid(format!(
                "{}: mean must be finite and nonnegative",
                self.name
            ))),
            Measurement::Quantile { quantile, value }
                if (0.0..=1.0).contains(quantile) && value.is_finite() && *value >= 0.0 =>
            {
                Ok(())
            }
            Measurement::Quantile { .. } => Err(invalid(format!(
                "{}: a quantile must be between 0 and 1, with a finite, nonnegative value",
                self.name
            ))),
            Measurement::Unavailable { reason } if !reason.trim().is_empty() => Ok(()),
            Measurement::Unavailable { .. } => Err(invalid(format!(
                "{}: unavailable metrics need a reason",
                self.name
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Measurement {
    Total {
        value: u64,
    },
    Mean {
        value: f64,
    },
    /// The value at `quantile`, from 0 (the minimum) to 1 (the maximum), of a distribution the workload observed.
    Quantile {
        quantile: f64,
        value: f64,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Nanoseconds,
    Count,
    Bytes,
}

/// The window a metric covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The sample's measured operations only.
    Measured,
    /// A window attached around the measured phase, such as `perf stat -p` for a fixed duration. It is neither the whole process nor exactly the measured operations.
    Attached,
    /// The whole process lifetime, including startup, preparation, warmup, and shutdown.
    Process,
    /// A service of a multi-process scenario, measured by the runner between the driver's window markers. Only the runner records these, never a workload's record.
    Window,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Wall,
    Rusage,
    PerfStat,
    Workload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    LowerIsBetter,
    HigherIsBetter,
    Neutral,
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests;
