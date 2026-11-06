// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::record::{Direction, Measurement};

struct Scripted(Vec<io::Result<ResourceUsage>>);

impl ResourceSource for Scripted {
    fn snapshot(&mut self) -> io::Result<ResourceUsage> {
        self.0.remove(0)
    }
    fn resolution_ns(&self) -> u64 {
        7
    }
}

fn usage(value: u64) -> ResourceUsage {
    ResourceUsage {
        user_cpu_ns: value,
        system_cpu_ns: value,
        voluntary_context_switches: value,
        involuntary_context_switches: value,
    }
}

#[test]
fn current_process_intervals_report_the_standard_metrics() {
    let interval = Interval::start().unwrap();
    std::hint::black_box((0..10_000).sum::<u64>());
    let sample = interval.finish(3).unwrap();
    assert_eq!(sample.operations, 3);
    let names: Vec<_> = sample
        .metrics
        .iter()
        .map(|metric| metric.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "wall_time",
            "user_cpu_time",
            "system_cpu_time",
            "voluntary_context_switches",
            "involuntary_context_switches"
        ]
    );
    assert!(
        sample
            .metrics
            .iter()
            .all(|metric| metric.direction == Direction::LowerIsBetter
                && metric.phase == Phase::Measured)
    );
    assert_eq!(
        sample.metric("user_cpu_time").unwrap().resolution,
        Some(1000)
    );
    assert_eq!(CurrentProcess.resolution_ns(), 1000);
}

#[test]
fn deltas_use_the_source_resolution() {
    let sample = Interval::start_with(Scripted(vec![Ok(usage(1)), Ok(usage(5))]))
        .unwrap()
        .finish(2)
        .unwrap();
    assert_eq!(
        sample.metric("user_cpu_time").unwrap().measurement,
        Measurement::Total { value: 4 }
    );
    assert_eq!(
        sample.metric("system_cpu_time").unwrap().resolution,
        Some(7)
    );
    assert_eq!(
        sample
            .metric("involuntary_context_switches")
            .unwrap()
            .measurement,
        Measurement::Total { value: 4 }
    );
}

#[test]
fn snapshot_failures_and_impossible_deltas_are_errors() {
    assert!(Interval::start_with(Scripted(vec![Err(io::Error::other("denied"))])).is_err());
    let interval = Interval::start_with(Scripted(vec![
        Ok(usage(1)),
        Err(io::Error::other("denied")),
    ]))
    .unwrap();
    assert_eq!(interval.finish(1).unwrap_err().to_string(), "denied");
    let zero = Interval::start_with(Scripted(vec![Ok(usage(1)), Ok(usage(1))]))
        .unwrap()
        .finish(0)
        .unwrap_err();
    assert_eq!(zero.kind(), io::ErrorKind::InvalidInput);
    for field in 0..4 {
        let mut later = usage(5);
        match field {
            0 => later.user_cpu_ns = 0,
            1 => later.system_cpu_ns = 0,
            2 => later.voluntary_context_switches = 0,
            _ => later.involuntary_context_switches = 0,
        }
        assert_eq!(
            later.since(usage(1)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn decreasing_snapshots_fail_the_interval() {
    let interval = Interval::start_with(Scripted(vec![Ok(usage(5)), Ok(usage(1))])).unwrap();
    assert_eq!(
        interval.finish(1).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
