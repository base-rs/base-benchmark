// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::artifact::ArtifactRole;
use crate::record::{ProcessRecord, Sample};
use crate::test_support::plan;
use std::path::Path;

fn entry(
    subject: &str,
    round: u32,
    metrics: Vec<Metric>,
    process_metrics: Vec<Metric>,
) -> SampleEntry {
    let mut sample = Sample::new(10);
    for metric in metrics {
        sample.push(metric);
    }
    SampleEntry {
        round,
        position: 0,
        subject: subject.to_owned(),
        command: Vec::new(),
        cpus: Vec::new(),
        executable_sha256: String::new(),
        duration_ns: 0,
        exit_code: Some(0),
        signal: None,
        timed_out: false,
        error: None,
        stderr: String::new(),
        record: Some(ProcessRecord::new("fixture", serde_json::json!({})).with_sample(sample)),
        process_metrics,
    }
}

fn wall(value: u64) -> Metric {
    Metric::total("wall_time", Unit::Nanoseconds, value).lower_is_better()
}

fn build_plan() -> Plan {
    let roles = [
        ("baseline", ArtifactRole::Baseline),
        ("candidate", ArtifactRole::Candidate),
        ("baseline-replica", ArtifactRole::LayoutReplica),
    ];
    plan(Path::new("/work.sh"), &[], &roles, Path::new("/lock"))
}

#[test]
fn distributions_have_medians_and_ranges() {
    assert_eq!(Distribution::of(&mut []), None);
    assert_eq!(
        Distribution::of(&mut [3.0, 1.0, 2.0]),
        Some(Distribution {
            median: 2.0,
            minimum: 1.0,
            maximum: 3.0
        })
    );
    assert_eq!(Distribution::of(&mut [4.0, 1.0]).unwrap().median, 2.5);
}

#[test]
fn build_changes_are_compared_with_the_layout_replica() {
    let plan = build_plan();
    let mut entries = Vec::new();
    for round in 1..=2 {
        entries.push(entry(
            "baseline",
            round,
            vec![wall(1000), Metric::total("bytes", Unit::Bytes, 50)],
            vec![
                Metric::total("instructions:u", Unit::Count, 900)
                    .with_phase(Phase::Process)
                    .lower_is_better(),
            ],
        ));
        entries.push(entry(
            "candidate",
            round,
            vec![wall(990), Metric::total("bytes", Unit::Bytes, 50)],
            vec![
                Metric::total("instructions:u", Unit::Count, 600)
                    .with_phase(Phase::Process)
                    .lower_is_better(),
            ],
        ));
        entries.push(entry(
            "baseline-replica",
            round,
            vec![
                wall(if round == 1 { 1020 } else { 990 }),
                Metric::total("bytes", Unit::Bytes, 50),
            ],
            vec![
                Metric::total("instructions:u", Unit::Count, 900)
                    .with_phase(Phase::Process)
                    .lower_is_better(),
            ],
        ));
    }
    let mut failed = entry("candidate", 3, vec![wall(1)], Vec::new());
    failed.error = Some("ignored".to_owned());
    entries.push(failed);
    let summary = summarize(&plan, &entries);
    let build = summary
        .pairs
        .iter()
        .find(|pair| pair.kind == PairKind::Build)
        .unwrap();
    let instructions = build
        .metrics
        .iter()
        .find(|metric| metric.name == "instructions:u")
        .unwrap();
    assert_eq!(
        (
            instructions.statistic,
            instructions.improved,
            instructions.regressed
        ),
        (Statistic::Total, 2, 0)
    );
    let bytes = build
        .metrics
        .iter()
        .find(|metric| metric.name == "bytes")
        .unwrap();
    assert_eq!(
        (bytes.direction, bytes.improved, bytes.regressed),
        (Direction::Neutral, 0, 0)
    );
    assert_eq!(summary.warnings.len(), 1);
    assert!(
        summary.warnings[0].contains(
            "wall_time median change -1.00% is within the layout replica's range of ±2.00%"
        ),
        "{:?}",
        summary.warnings
    );
    assert_eq!(summary.subjects[1].processes, 2);
    let report = summary.report();
    assert!(report.contains("instructions:u (total)"));
    assert!(report.contains("warning: baseline -> candidate: wall_time"));
    assert!(report.contains("ns/op"));
}

#[test]
fn missing_controls_rounds_and_values_are_visible() {
    let roles = [
        ("baseline", ArtifactRole::Baseline),
        ("candidate", ArtifactRole::Candidate),
    ];
    let mut plan = plan(Path::new("/work.sh"), &[], &roles, Path::new("/lock"));
    plan.decision_rule = Some("predeclared".to_owned());
    let entries = vec![
        entry(
            "baseline",
            1,
            vec![
                wall(0),
                Metric::unavailable("cycles", Unit::Count, "denied").lower_is_better(),
                Metric::mean("latency", Unit::Nanoseconds, 2.0).higher_is_better(),
                Metric::total("total_bytes", Unit::Bytes, 5).with_phase(Phase::Attached),
                Metric::total("total_time", Unit::Nanoseconds, 5).with_phase(Phase::Process),
            ],
            Vec::new(),
        ),
        entry(
            "candidate",
            1,
            vec![
                wall(5),
                Metric::unavailable("cycles", Unit::Count, "denied").lower_is_better(),
                Metric::mean("latency", Unit::Nanoseconds, 1.0).higher_is_better(),
            ],
            Vec::new(),
        ),
        entry(
            "baseline",
            2,
            vec![wall(10), Metric::total("latency", Unit::Count, 1)],
            Vec::new(),
        ),
        entry("candidate", 2, vec![wall(20)], Vec::new()),
        entry(
            "baseline",
            3,
            vec![wall(10), Metric::total("payload", Unit::Bytes, 3)],
            Vec::new(),
        ),
    ];
    let summary = summarize(&plan, &entries);
    assert!(
        summary
            .warnings
            .iter()
            .any(|warning| warning.contains("no layout replica"))
    );
    assert!(
        summary
            .warnings
            .iter()
            .any(|warning| warning.contains("latency changes unit"))
    );
    let pair = &summary.pairs[0];
    let wall = pair
        .metrics
        .iter()
        .find(|metric| metric.name == "wall_time")
        .unwrap();
    assert_eq!((wall.rounds, wall.regressed), (1, 1));
    let latency = pair
        .metrics
        .iter()
        .find(|metric| metric.name == "latency")
        .unwrap();
    assert_eq!((latency.rounds, latency.regressed), (1, 1));
    let cycles = pair
        .metrics
        .iter()
        .find(|metric| metric.name == "cycles")
        .unwrap();
    assert_eq!((cycles.rounds, cycles.change), (0, None));
    let baseline = &summary.subjects[0];
    assert_eq!(
        baseline
            .metrics
            .iter()
            .find(|metric| metric.name == "cycles")
            .unwrap()
            .unavailable,
        1
    );
    let report = summary.report();
    for expected in [
        "decision rule: predeclared",
        "cycles",
        "unavailable",
        "B total",
        "ns total",
        "/op",
        "B/op",
    ] {
        assert!(report.contains(expected), "{expected}: {report}");
    }
    let counts = summarize(
        &plan,
        &[entry(
            "baseline",
            1,
            vec![Metric::total("events", Unit::Count, 3).with_phase(Phase::Process)],
            Vec::new(),
        )],
    );
    assert!(counts.report().contains(" total"));
}

#[test]
fn incomplete_rounds_and_metrics_are_skipped() {
    let plan = build_plan();
    let entries = vec![
        entry(
            "baseline",
            1,
            vec![
                wall(100),
                Metric::total("bytes", Unit::Bytes, 1).lower_is_better(),
            ],
            Vec::new(),
        ),
        entry(
            "candidate",
            1,
            vec![
                wall(90),
                Metric::total("bytes", Unit::Bytes, 1).lower_is_better(),
            ],
            Vec::new(),
        ),
        entry("baseline-replica", 1, vec![wall(100)], Vec::new()),
        entry("baseline", 2, vec![wall(100)], Vec::new()),
    ];
    let summary = summarize(&plan, &entries);
    let build = summary
        .pairs
        .iter()
        .find(|pair| pair.kind == PairKind::Build)
        .unwrap();
    assert_eq!(
        build
            .metrics
            .iter()
            .find(|metric| metric.name == "wall_time")
            .unwrap()
            .rounds,
        1
    );
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
}
