// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::record::{Metric, Phase, Source, Unit, invalid};
use std::io;
use std::path::{Path, PathBuf};

/// Wrap a workload in `perf stat`, writing CSV counters to `output`.
pub(crate) fn wrap(
    perf: &Path,
    events: &[String],
    output: &Path,
    program: &Path,
    arguments: &[String],
) -> (PathBuf, Vec<String>) {
    let mut wrapped = vec![
        "stat".to_owned(),
        "--no-big-num".to_owned(),
        "-x,".to_owned(),
        "-e".to_owned(),
        events.join(","),
        "-o".to_owned(),
        output.display().to_string(),
        "--".to_owned(),
        program.display().to_string(),
    ];
    wrapped.extend(arguments.iter().cloned());
    (perf.to_owned(), wrapped)
}

/// Parse `perf stat -x,` output into whole-process totals.
///
/// These counters cover the entire workload process, including startup, preparation, warmup, and serialization. They are recorded in the process phase and are never divided by measured operations. A requested counter that was not counted fails the sample instead of becoming a zero.
pub(crate) fn parse(text: &str, events: &[String]) -> io::Result<Vec<Metric>> {
    let mut metrics = Vec::new();
    for event in events {
        let line = text
            .lines()
            .map(|line| line.split(',').collect::<Vec<_>>())
            .find(|fields| fields.get(2).is_some_and(|name| name.trim() == event))
            .ok_or_else(|| invalid(format!("perf stat did not report {event}")))?;
        let value = line[0].trim().parse::<u64>().map_err(|_| {
            invalid(format!(
                "perf stat did not count {event}: {:?}",
                line[0].trim()
            ))
        })?;
        metrics.push(
            Metric::total(event.clone(), Unit::Count, value)
                .with_phase(Phase::Process)
                .with_source(Source::PerfStat)
                .lower_is_better(),
        );
    }
    Ok(metrics)
}

#[cfg(test)]
mod tests;
