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
        stack_padding: None,
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

fn pinned(roles: &[(&str, ArtifactRole)], variants: &[&str]) -> Plan {
    let mut plan = plan(Path::new("/work.sh"), variants, roles, Path::new("/lock"));
    plan.cpus = vec![0];
    plan
}

fn build_plan() -> Plan {
    pinned(
        &[
            ("baseline", ArtifactRole::Baseline),
            ("candidate", ArtifactRole::Candidate),
            ("baseline-replica", ArtifactRole::LayoutReplica),
        ],
        &[],
    )
}

fn pair(summary: &Summary, kind: PairKind) -> &PairSummary {
    summary.pairs.iter().find(|pair| pair.kind == kind).unwrap()
}

fn metric<'a>(pair: &'a PairSummary, name: &str) -> &'a PairMetric {
    pair.metrics
        .iter()
        .find(|metric| metric.name == name)
        .unwrap()
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
fn sign_tests_are_exact_and_two_sided() {
    assert_eq!(sign_test(5, 0), 0.0625);
    assert_eq!(sign_test(0, 0), 1.0);
    assert_eq!(sign_test(3, 3), 1.0);
    assert!((sign_test(9, 1) - 0.021_484_375).abs() < 1e-12);
    assert!(sign_test(2000, 0) < 1e-300);
    // Balanced rounds stay insignificant however many there are.
    assert_eq!(sign_test(538, 538), 1.0);
    assert_eq!(sign_test(100_000, 100_000), 1.0);
    // Exact: 2 * sum(C(1000, k), k <= 470) / 2^1000 = 0.0620231950983631...
    assert!((sign_test(530, 470) - 0.062_023_195_098_363).abs() < 1e-10);
}

#[test]
fn build_changes_are_compared_with_the_layout_replica() {
    let plan = build_plan();
    let instructions = |value| {
        Metric::total("instructions:u", Unit::Count, value)
            .with_phase(Phase::Process)
            .lower_is_better()
    };
    let mut entries = Vec::new();
    for round in 1..=2 {
        entries.push(entry(
            "baseline",
            round,
            vec![wall(1000), Metric::total("bytes", Unit::Bytes, 50)],
            vec![instructions(900)],
        ));
        entries.push(entry(
            "candidate",
            round,
            vec![wall(990), Metric::total("bytes", Unit::Bytes, 50)],
            vec![instructions(600)],
        ));
        entries.push(entry(
            "baseline-replica",
            round,
            vec![
                wall(if round == 1 { 1020 } else { 990 }),
                Metric::total("bytes", Unit::Bytes, 50),
            ],
            vec![instructions(900)],
        ));
    }
    let mut failed = entry("candidate", 3, vec![wall(1)], Vec::new());
    failed.error = Some("ignored".to_owned());
    entries.push(failed);
    let summary = summarize(&plan, &entries);
    assert_eq!(summary.generator, GENERATOR);
    let build = pair(&summary, PairKind::Build);
    let counted = metric(build, "instructions:u");
    assert_eq!(
        (counted.statistic, counted.improved, counted.regressed),
        (Statistic::Total, 2, 0)
    );
    assert_eq!(
        (counted.change, counted.difference.unwrap().median),
        (None, -300.0),
        "counts are compared by difference only"
    );
    assert_eq!(counted.sign_test_p, Some(0.5));
    let bytes = metric(build, "bytes");
    assert_eq!(
        (
            bytes.direction,
            bytes.improved,
            bytes.regressed,
            bytes.sign_test_p
        ),
        (Direction::Neutral, 0, 0, None)
    );
    assert_eq!(
        summary.warnings,
        [
            "baseline -> candidate: wall_time median change -1.00% is within the layout replicas' range of ±2.00%"
        ]
    );
    assert_eq!(summary.subjects[1].processes, 2);
    let report = summary.report();
    assert!(report.contains("instructions:u (total)"));
    assert!(
        report.contains("-300.000 [-300.000, -300.000] total"),
        "{report}"
    );
    assert!(report.contains("sign test p=0.500"), "{report}");
    assert!(report.contains("ns/op"));
}

#[test]
fn variant_conclusions_are_checked_against_the_replica() {
    let mut plan = pinned(
        &[
            ("current", ArtifactRole::Baseline),
            ("current-replica", ArtifactRole::LayoutReplica),
        ],
        &["reference", "engine"],
    );
    plan.artifacts.artifacts[1].source.replica_of = Some("current".to_owned());
    let entries = vec![
        entry("current/reference", 1, vec![wall(1000)], Vec::new()),
        entry("current/engine", 1, vec![wall(800)], Vec::new()),
        entry("current-replica/reference", 1, vec![wall(1000)], Vec::new()),
        entry("current-replica/engine", 1, vec![wall(1100)], Vec::new()),
    ];
    let summary = summarize(&plan, &entries);
    assert!(summary.warnings.iter().any(|warning| warning.contains("current/reference -> current/engine: wall_time median change -20.00% has the opposite sign in the layout replica (+10.00%)")), "{:?}", summary.warnings);
}

#[test]
fn quantized_counters_and_mixed_directions_are_flagged() {
    let mut plan = pinned(&[("baseline", ArtifactRole::Baseline)], &["slow", "fast"]);
    plan.cpus.clear();
    plan.stack_padding = false;
    let cpu = |value| {
        Metric::total("user_cpu_time", Unit::Nanoseconds, value)
            .with_resolution(1000)
            .lower_is_better()
    };
    let switches = |value| Metric::total("switches", Unit::Count, value).lower_is_better();
    let entries = vec![
        entry(
            "baseline/slow",
            1,
            vec![wall(1000), cpu(1000), switches(1)],
            Vec::new(),
        ),
        entry(
            "baseline/fast",
            1,
            vec![wall(900), cpu(2000), switches(5)],
            Vec::new(),
        ),
        entry(
            "baseline/slow",
            2,
            vec![wall(1000), cpu(2_000_000), switches(1)],
            Vec::new(),
        ),
        entry(
            "baseline/fast",
            2,
            vec![wall(1100), cpu(1_000_000), switches(1)],
            Vec::new(),
        ),
    ];
    let summary = summarize(&plan, &entries);
    let variant = pair(&summary, PairKind::Variant);
    let user = metric(variant, "user_cpu_time");
    assert_eq!(
        (user.rounds, user.unresolved, user.change),
        (2, 1, None),
        "a ratio needs every round resolved"
    );
    assert_eq!((user.improved, user.regressed), (1, 1));
    let switch = metric(variant, "switches");
    assert_eq!(
        (switch.improved, switch.regressed, switch.change),
        (0, 1, None),
        "a difference below one event is a tie"
    );
    for expected in [
        "no CPU placement",
        "stack padding disabled",
        "wall_time changed in both directions (better 1/2, worse 1/2)",
        "no layout replica: variant comparisons",
    ] {
        assert!(
            summary
                .warnings
                .iter()
                .any(|warning| warning.contains(expected)),
            "{expected}: {:?}",
            summary.warnings
        );
    }
    let report = summary.report();
    assert!(report.contains("1 round(s) below resolution"), "{report}");
    assert!(
        report.contains("switches                         +0.200 [+0.000, +0.400] /op"),
        "{report}"
    );
}

#[test]
fn incompatible_and_missing_metrics_are_disclosed() {
    let plan = build_plan();
    let latency = |value| Metric::total("latency", Unit::Nanoseconds, value).lower_is_better();
    let entries = vec![
        entry(
            "baseline",
            1,
            vec![
                wall(100),
                latency(50),
                Metric::total("mixed", Unit::Count, 5),
                Metric::mean("mean", Unit::Nanoseconds, 400.0).lower_is_better(),
            ],
            Vec::new(),
        ),
        entry(
            "candidate",
            1,
            vec![
                wall(90),
                Metric::total("mixed", Unit::Count, 5).with_phase(Phase::Process),
                Metric::mean("mean", Unit::Nanoseconds, 200.0).lower_is_better(),
            ],
            vec![Metric::total("wall_time", Unit::Nanoseconds, 1).with_phase(Phase::Process)],
        ),
        entry(
            "baseline-replica",
            1,
            vec![wall(100), latency(50)],
            Vec::new(),
        ),
    ];
    let summary = summarize(&plan, &entries);
    let build = pair(&summary, PairKind::Build);
    assert_eq!(metric(build, "mixed").rounds, 0);
    let mean = metric(build, "mean");
    assert_eq!(
        (mean.statistic, mean.change.unwrap().median),
        (Statistic::Mean, -0.5)
    );
    for expected in [
        "candidate: metric wall_time has conflicting definitions within a process and is not summarized",
        "baseline -> candidate: mixed is defined differently by baseline and candidate in 1 round(s), which are not compared",
        "baseline -> candidate: latency is missing from the candidate in 1 round(s)",
        "baseline -> candidate: mean has no resolved layout-replica change to compare with",
    ] {
        assert!(
            summary.warnings.contains(&expected.to_owned()),
            "{expected}: {:?}",
            summary.warnings
        );
    }
    assert!(summary.report().contains("ns mean"));
}

#[test]
fn missing_controls_rounds_and_values_are_visible() {
    let mut plan = pinned(
        &[
            ("baseline", ArtifactRole::Baseline),
            ("candidate", ArtifactRole::Candidate),
        ],
        &[],
    );
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
                Metric::mean("mean_count", Unit::Count, 1.0),
                Metric::mean("mean_bytes", Unit::Bytes, 1.0),
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
            .any(|warning| warning.contains("no layout replica, so build-layout"))
    );
    assert!(
        summary
            .warnings
            .iter()
            .any(|warning| warning.contains("latency changes definition between processes"))
    );
    let build = pair(&summary, PairKind::Build);
    let wall_time = metric(build, "wall_time");
    assert_eq!(
        (
            wall_time.rounds,
            wall_time.regressed,
            wall_time.unresolved,
            wall_time.change
        ),
        (2, 2, 2, None)
    );
    let latency = metric(build, "latency");
    assert_eq!((latency.rounds, latency.regressed), (1, 1));
    let cycles = metric(build, "cycles");
    assert_eq!(
        (cycles.rounds, cycles.change, cycles.difference),
        (0, None, None)
    );
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
        " mean",
        "B mean",
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
fn several_replicas_widen_the_control_range() {
    let plan = pinned(
        &[
            ("baseline", ArtifactRole::Baseline),
            ("candidate", ArtifactRole::Candidate),
            ("baseline-replica-1", ArtifactRole::LayoutReplica),
            ("baseline-replica-2", ArtifactRole::LayoutReplica),
        ],
        &[],
    );
    let entries = vec![
        entry("baseline", 1, vec![wall(1000)], Vec::new()),
        entry("candidate", 1, vec![wall(970)], Vec::new()),
        entry("baseline-replica-1", 1, vec![wall(1010)], Vec::new()),
        entry("baseline-replica-2", 1, vec![wall(960)], Vec::new()),
    ];
    let summary = summarize(&plan, &entries);
    assert_eq!(
        summary.warnings,
        [
            "baseline -> candidate: wall_time median change -3.00% is within the layout replicas' range of ±4.00%"
        ]
    );
}

#[test]
fn unresolved_replica_comparisons_are_disclosed() {
    let mut plan = pinned(
        &[
            ("current", ArtifactRole::Baseline),
            ("current-replica", ArtifactRole::LayoutReplica),
        ],
        &["reference", "engine"],
    );
    plan.artifacts.artifacts[1].source.replica_of = Some("current".to_owned());
    let entries = vec![
        entry(
            "current/reference",
            1,
            vec![
                wall(1000),
                Metric::total("cpu", Unit::Nanoseconds, 10)
                    .with_resolution(1000)
                    .lower_is_better(),
            ],
            Vec::new(),
        ),
        entry(
            "current/engine",
            1,
            vec![
                wall(800),
                Metric::total("cpu", Unit::Nanoseconds, 20)
                    .with_resolution(1000)
                    .lower_is_better(),
            ],
            Vec::new(),
        ),
        entry("current-replica/reference", 1, vec![wall(0)], Vec::new()),
        entry("current-replica/engine", 1, vec![wall(5)], Vec::new()),
    ];
    let summary = summarize(&plan, &entries);
    assert!(summary.warnings.contains(&"current/reference -> current/engine: wall_time has no resolved comparison in the layout replica current-replica".to_owned()), "{:?}", summary.warnings);
}

#[test]
fn incomplete_rounds_and_metrics_are_skipped() {
    let plan = build_plan();
    let entries = vec![
        entry("baseline", 1, vec![wall(100)], Vec::new()),
        entry("candidate", 1, vec![wall(90)], Vec::new()),
        entry("baseline-replica", 1, vec![wall(100)], Vec::new()),
        entry("baseline", 2, vec![wall(100)], Vec::new()),
    ];
    let summary = summarize(&plan, &entries);
    assert_eq!(
        metric(pair(&summary, PairKind::Build), "wall_time").rounds,
        1
    );
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
}

#[test]
fn smoke_summaries_contain_no_comparisons() {
    let mut plan = pinned(&[("baseline", ArtifactRole::Baseline)], &["slow", "fast"]);
    plan.mode = Mode::Smoke;
    plan.cpus.clear();
    let entries = vec![
        entry("baseline/slow", 1, vec![wall(1000)], Vec::new()),
        entry("baseline/fast", 1, vec![wall(500)], Vec::new()),
    ];
    let summary = summarize(&plan, &entries);
    assert!(summary.pairs.is_empty());
    assert!(summary.warnings.is_empty());
    let report = summary.report();
    assert!(report.contains("pair comparisons are omitted from smoke reports"));
    assert!(!report.contains("-50.00%"));
}

#[test]
fn sign_tests_are_reported_only_when_rounds_moved() {
    let plan = pinned(&[("baseline", ArtifactRole::Baseline)], &["slow", "fast"]);
    let entries = vec![
        entry(
            "baseline/slow",
            1,
            vec![
                wall(1000),
                Metric::total("switches", Unit::Count, 1).lower_is_better(),
            ],
            Vec::new(),
        ),
        entry(
            "baseline/fast",
            1,
            vec![
                wall(900),
                Metric::total("switches", Unit::Count, 1).lower_is_better(),
            ],
            Vec::new(),
        ),
    ];
    let summary = summarize(&plan, &entries);
    let report = summary.report();
    let line = |name: &str| {
        report
            .lines()
            .find(|line| line.trim_start().starts_with(name))
            .unwrap()
            .to_owned()
    };
    assert!(line("wall_time").contains("sign test p=1.000"), "{report}");
    assert!(!line("switches").contains("sign test"), "{report}");
    assert_eq!(
        metric(pair(&summary, PairKind::Variant), "switches").sign_test_p,
        Some(1.0)
    );
}

#[test]
fn unavailable_values_are_not_redefinitions() {
    let plan = pinned(&[("baseline", ArtifactRole::Baseline)], &["slow", "fast"]);
    let unavailable = || {
        Metric::unavailable("latency", Unit::Nanoseconds, "no samples")
            .with_phase(Phase::Process)
            .lower_is_better()
    };
    let latency = |value| {
        Metric::mean("latency", Unit::Nanoseconds, value)
            .with_phase(Phase::Process)
            .lower_is_better()
    };
    let entries = vec![
        entry(
            "baseline/slow",
            1,
            vec![wall(1000), unavailable()],
            Vec::new(),
        ),
        entry(
            "baseline/fast",
            1,
            vec![wall(900), latency(500.0)],
            Vec::new(),
        ),
        entry(
            "baseline/slow",
            2,
            vec![wall(1000), latency(600.0)],
            Vec::new(),
        ),
        entry(
            "baseline/fast",
            2,
            vec![wall(900), latency(500.0)],
            Vec::new(),
        ),
    ];
    let summary = summarize(&plan, &entries);
    assert!(
        !summary
            .warnings
            .iter()
            .any(|warning| warning.contains("latency")),
        "{:?}",
        summary.warnings
    );
    let slow = summary
        .subjects
        .iter()
        .find(|subject| subject.label == "baseline/slow")
        .unwrap();
    let described = slow
        .metrics
        .iter()
        .find(|metric| metric.name == "latency")
        .unwrap();
    assert_eq!(
        (
            described.statistic,
            described.unavailable,
            described.distribution.unwrap().median
        ),
        (Statistic::Mean, 1, 600.0)
    );
    let compared = metric(pair(&summary, PairKind::Variant), "latency");
    assert_eq!(
        (compared.rounds, compared.statistic),
        (1, Statistic::Mean),
        "pairs are described by a measured round"
    );
}

#[test]
fn quantiles_are_compared_only_at_the_same_quantile() {
    let plan = build_plan();
    let p99 =
        |value| Metric::quantile("latency_p99", Unit::Nanoseconds, 0.99, value).lower_is_better();
    let entries = vec![
        entry(
            "baseline",
            1,
            vec![
                wall(100),
                p99(400.0),
                Metric::quantile("tail", Unit::Nanoseconds, 0.999, 9.0),
                Metric::quantile("bytes", Unit::Bytes, 0.5, 3.0),
                Metric::quantile("count", Unit::Count, 1.0, 2.0),
            ],
            Vec::new(),
        ),
        entry(
            "candidate",
            1,
            vec![
                wall(100),
                p99(200.0),
                Metric::quantile("tail", Unit::Nanoseconds, 0.99, 9.0),
            ],
            Vec::new(),
        ),
        entry(
            "baseline-replica",
            1,
            vec![wall(100), p99(400.0)],
            Vec::new(),
        ),
    ];
    let summary = summarize(&plan, &entries);
    let build = pair(&summary, PairKind::Build);
    let latency = metric(build, "latency_p99");
    assert_eq!(
        (
            latency.statistic,
            latency.improved,
            latency.change.unwrap().median
        ),
        (Statistic::Quantile(0.99), 1, -0.5)
    );
    assert_eq!(metric(build, "tail").rounds, 0);
    assert!(
        summary.warnings.contains(
            &"baseline -> candidate: tail is defined differently by baseline and candidate in 1 round(s), which are not compared".to_owned()
        ),
        "{:?}",
        summary.warnings
    );
    let report = summary.report();
    for expected in ["ns p99", "ns p99.9", "B p50", "2.000 p100 ["] {
        assert!(report.contains(expected), "{expected}: {report}");
    }
    let serialized = serde_json::to_value(latency.statistic).unwrap();
    assert_eq!(serialized, serde_json::json!({"quantile": 0.99}));
    let parsed: Statistic = serde_json::from_value(serialized).unwrap();
    assert_eq!(parsed, Statistic::Quantile(0.99));
    assert_eq!(summary.version, 2);
}

#[test]
fn quantile_labels_keep_their_precision() {
    for (quantile, label) in [
        (0.0, "ns p0"),
        (0.001, "ns p0.1"),
        (0.5, "ns p50"),
        (0.99, "ns p99"),
        (0.999, "ns p99.9"),
        (0.999999, "ns p99.9999"),
        (0.000001, "ns p0.0001"),
        (1.0, "ns p100"),
    ] {
        assert_eq!(
            suffix(Statistic::Quantile(quantile), Unit::Nanoseconds),
            label
        );
    }
}

#[test]
fn quantiles_from_records_normalize_negative_zero_and_conflict_within_a_process() {
    let plan = build_plan();
    let minimum: Metric = serde_json::from_value(serde_json::json!({
        "name": "minimum", "unit": "count", "phase": "measured", "source": "workload",
        "direction": "neutral", "measurement": {"kind": "quantile", "quantile": -0.0, "value": 1.0}
    }))
    .unwrap();
    let mut sample = Sample::new(10)
        .with(Metric::quantile("tail", Unit::Nanoseconds, 0.99, 1.0))
        .with(minimum);
    let mut entries = vec![entry("baseline", 1, vec![wall(100)], Vec::new())];
    sample.push(wall(100));
    let record = entries[0].record.as_mut().unwrap();
    record.samples.push(
        Sample::new(10)
            .with(Metric::quantile("tail", Unit::Nanoseconds, 0.5, 1.0))
            .with(wall(100)),
    );
    record.samples.push(sample);
    let summary = summarize(&plan, &entries);
    let baseline = &summary.subjects[0];
    let statistic = |name: &str| {
        baseline
            .metrics
            .iter()
            .find(|metric| metric.name == name)
            .unwrap()
            .statistic
    };
    assert_eq!(statistic("minimum"), Statistic::Quantile(0.0));
    assert!(
        summary
            .report()
            .contains("minimum                          1.000 p0 [")
    );
    assert!(
        summary
            .warnings
            .contains(&"baseline: metric tail has conflicting definitions within a process and is not summarized".to_owned()),
        "{:?}",
        summary.warnings
    );
}
