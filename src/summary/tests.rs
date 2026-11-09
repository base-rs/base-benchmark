// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::artifact::{ArtifactRole, DependencyIdentity};
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
        roles: Vec::new(),
    }
}

fn wall(value: u64) -> Metric {
    Metric::total("wall_time", Unit::Nanoseconds, value).lower_is_better()
}

fn pinned(roles: &[(&str, ArtifactRole)], variants: &[&str]) -> Plan {
    let mut plan = plan(Path::new("/work.sh"), variants, roles, Path::new("/lock"));
    plan.cpus = vec![0];
    // Every fixture artifact runs the same shell, so give replicas their own hash, as a salted build has.
    for artifact in &mut plan.artifacts.artifacts {
        if artifact.role == ArtifactRole::LayoutReplica {
            artifact.sha256 = format!("{} salted", artifact.sha256);
        }
    }
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
            "baseline -> candidate: median change within the layout replicas' range: wall_time -1.00% (range ±2.00%)"
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
    assert!(summary.warnings.iter().any(|warning| warning == "current/reference -> current/engine: median change with the opposite sign in the layout replica current-replica, so the conclusion depends on build layout: wall_time -20.00% (replica +10.00%)"), "{:?}", summary.warnings);
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
    // Layout first, then campaign controls, then each pair's metrics that moved both ways, on one line.
    assert_eq!(
        summary.warnings,
        [
            "no layout replica: variant comparisons inside one executable can depend on its build layout; prepare with --replicas 1 to check",
            "no CPU placement: workload processes may run on different cores or core types between rounds",
            "stack padding disabled: every process of a subject starts with the same stack alignment, which can bias comparisons",
            "baseline/slow -> baseline/fast: changed in both directions, so inconclusive: user_cpu_time (better 1/2, worse 1/2), wall_time (better 1/2, worse 1/2)",
        ]
    );
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
        "candidate: conflicting definitions within a process, so not summarized: wall_time",
        "baseline -> candidate: defined differently by baseline and candidate, so those rounds are not compared: mixed (1 round(s))",
        "baseline -> candidate: missing from the candidate: latency (1 round(s))",
        "baseline -> candidate: no resolved layout-replica change to compare with: mean",
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
            .any(|warning| warning == "baseline: definitions change between processes, so differing processes are excluded: latency")
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
            "baseline -> candidate: median change within the layout replicas' range: wall_time -3.00% (range ±4.00%)"
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
    assert!(summary.warnings.contains(&"current/reference -> current/engine: no resolved comparison in the layout replica current-replica: wall_time".to_owned()), "{:?}", summary.warnings);
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
            &"baseline -> candidate: defined differently by baseline and candidate, so those rounds are not compared: tail (1 round(s))".to_owned()
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
    assert_eq!(summary.version, SUMMARY_VERSION);
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
        summary.warnings.contains(
            &"baseline: conflicting definitions within a process, so not summarized: tail"
                .to_owned()
        ),
        "{:?}",
        summary.warnings
    );
}

#[test]
fn replicas_identical_to_their_baseline_are_flagged() {
    let plan = plan(
        Path::new("/work.sh"),
        &[],
        &[
            ("baseline", ArtifactRole::Baseline),
            ("baseline-replica", ArtifactRole::LayoutReplica),
        ],
        Path::new("/lock"),
    );
    let entries = vec![
        entry("baseline", 1, vec![wall(100)], Vec::new()),
        entry("baseline-replica", 1, vec![wall(100)], Vec::new()),
    ];
    let summary = summarize(&plan, &entries);
    assert!(
        summary.warnings.contains(
            &"layout replica baseline-replica is byte-identical to its baseline, so it samples run-to-run variation only, not build layout".to_owned()
        ),
        "{:?}",
        summary.warnings
    );
}

#[test]
fn warnings_name_every_metric_of_a_hazard_on_one_line_with_layout_first() {
    let mut plan = pinned(
        &[
            ("current", ArtifactRole::Baseline),
            ("current-replica", ArtifactRole::LayoutReplica),
        ],
        &["reference", "engine"],
    );
    plan.artifacts.artifacts[1].source.replica_of = Some("current".to_owned());
    let cpu = |value| Metric::total("cpu_time", Unit::Nanoseconds, value).lower_is_better();
    let mut entries = Vec::new();
    for (round, wall_time, cpu_time) in [(1, 700, 700), (2, 1100, 1050)] {
        entries.extend([
            entry(
                "current/reference",
                round,
                vec![wall(1000), cpu(1000)],
                Vec::new(),
            ),
            entry(
                "current/engine",
                round,
                vec![wall(wall_time), cpu(cpu_time)],
                Vec::new(),
            ),
            entry(
                "current-replica/reference",
                round,
                vec![wall(1000), cpu(1000)],
                Vec::new(),
            ),
            entry(
                "current-replica/engine",
                round,
                vec![wall(1100), cpu(1100)],
                Vec::new(),
            ),
        ]);
    }
    let summary = summarize(&plan, &entries);
    // Found after the metrics that moved both ways, the reversal in the replica still comes first.
    assert_eq!(
        summary.warnings,
        [
            "current/reference -> current/engine: median change with the opposite sign in the layout replica current-replica, so the conclusion depends on build layout: cpu_time -12.50% (replica +10.00%), wall_time -10.00% (replica +10.00%)",
            "current/reference -> current/engine: changed in both directions, so inconclusive: cpu_time (better 1/2, worse 1/2), wall_time (better 1/2, worse 1/2)",
        ]
    );
    let report = summary.report();
    let first = report.find("warning: ").unwrap();
    assert!(
        report[first..].starts_with(
            "warning: current/reference -> current/engine: median change with the opposite sign"
        ),
        "{report}"
    );
}

#[test]
fn overlaid_artifacts_and_their_lockfile_changes_are_described() {
    let package = DependencyIdentity {
        name: "fixture-benchmark".to_owned(),
        path: "/workspace/benchmark".into(),
        revision: Some("0123456789abcdef0123".to_owned()),
        dirty: Some(false),
    };
    let mut plan = build_plan();
    let plain = summarize(&plan, &[]);
    assert!(plain.overlays.is_empty());
    let serialized = serde_json::to_value(&plain).unwrap();
    assert!(serialized.get("overlays").is_none(), "{serialized}");
    assert_eq!(
        serde_json::from_value::<Summary>(serialized).unwrap(),
        plain
    );
    assert!(!plain.report().contains("overlaid"));

    for artifact in &mut plan.artifacts.artifacts {
        artifact.source.overlay = Some(package.clone());
    }
    plan.artifacts.artifacts[0].source.lockfile_changes = vec![
        "serde 1.0.1 -> 1.0.2".to_owned(),
        "itoa 1.0.0 -> 1.0.1".to_owned(),
    ];
    plan.artifacts.artifacts[1].source.overlay = Some(DependencyIdentity {
        revision: None,
        dirty: None,
        ..package.clone()
    });
    let summary = summarize(&plan, &[]);
    assert_eq!(
        summary.overlays[0],
        OverlaidArtifact {
            label: "baseline".to_owned(),
            package: package.clone(),
            lockfile_changes: vec![
                "serde 1.0.1 -> 1.0.2".to_owned(),
                "itoa 1.0.0 -> 1.0.1".to_owned()
            ],
        }
    );
    assert_eq!(summary.overlays.len(), 3);
    let serialized = serde_json::to_value(&summary).unwrap();
    assert_eq!(
        serialized["overlays"][0]["lockfile_changes"][0],
        "serde 1.0.1 -> 1.0.2"
    );
    assert_eq!(
        serde_json::from_value::<Summary>(serialized).unwrap(),
        summary
    );
    let report = summary.report();
    for expected in [
        "overlaid workload fixture-benchmark (0123456789ab) onto baseline, baseline-replica\n",
        "overlaid workload fixture-benchmark (unknown revision, state unknown) onto candidate\n",
        "lockfile changes in baseline: serde 1.0.1 -> 1.0.2, itoa 1.0.0 -> 1.0.1\n",
    ] {
        assert!(report.contains(expected), "{expected}: {report}");
    }
    assert!(!report.contains("kept every locked version"), "{report}");

    // Uncommitted changes are stated, and so is a lockfile every build kept.
    for artifact in &mut plan.artifacts.artifacts {
        artifact.source.overlay = Some(DependencyIdentity {
            dirty: Some(true),
            ..package.clone()
        });
        artifact.source.lockfile_changes.clear();
    }
    let report = summarize(&plan, &[]).report();
    for expected in [
        "overlaid workload fixture-benchmark (0123456789ab, modified) onto baseline, candidate, baseline-replica\n",
        "overlaid builds kept every locked version\n",
    ] {
        assert!(report.contains(expected), "{expected}: {report}");
    }
}

#[test]
fn service_cpu_over_the_window_is_divided_only_into_approximate_cpu_per_request() {
    use crate::role::RoleKind;
    use crate::runner::RoleEntry;
    let plan = plan(
        Path::new("/work.sh"),
        &[],
        &[("baseline", ArtifactRole::Baseline)],
        Path::new("/lock"),
    );
    let window = |metric: Metric| metric.with_phase(Phase::Window);
    let mut sample = entry(
        "baseline",
        1,
        vec![wall(1000)],
        vec![Metric::total("cycles", Unit::Count, 500).with_phase(Phase::Process)],
    );
    sample.roles = vec![RoleEntry {
        role: "server".to_owned(),
        kind: RoleKind::Service,
        host: None,
        command: Vec::new(),
        cpus: Vec::new(),
        stack_padding: None,
        executable_sha256: String::new(),
        started: true,
        ready_line: None,
        captures: Default::default(),
        exit_code: Some(0),
        signal: None,
        killed_after_grace: false,
        stdout: String::new(),
        stderr: String::new(),
        window: vec![
            window(Metric::total("cpu_time", Unit::Nanoseconds, 1000).with_source(Source::Rusage)),
            window(
                Metric::unavailable("user_cpu_time", Unit::Nanoseconds, "gone")
                    .with_source(Source::Rusage),
            ),
            window(Metric::total("elapsed", Unit::Nanoseconds, 7000).with_source(Source::Wall)),
        ],
    }];
    let summary = summarize(&plan, &[sample]);
    let subject = &summary.subjects[0];
    let metric = |name: &str| {
        subject
            .metrics
            .iter()
            .find(|metric| metric.name == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    let total = metric("server/cpu_time");
    assert_eq!(
        (total.phase, total.statistic, total.direction),
        (Phase::Window, Statistic::Total, Direction::Neutral)
    );
    assert_eq!(total.distribution.unwrap().median, 1000.0);
    // The driver's record has 10 operations.
    let per_request = metric("server/cpu_time_per_operation");
    assert_eq!(
        (per_request.statistic, per_request.direction),
        (Statistic::ApproximatePerOperation, Direction::LowerIsBetter)
    );
    assert_eq!(per_request.distribution.unwrap().median, 100.0);
    assert_eq!(metric("server/user_cpu_time_per_operation").unavailable, 1);
    // Only CPU times are divided, and process totals never are.
    assert_eq!(metric("server/elapsed").statistic, Statistic::Total);
    assert!(
        subject
            .metrics
            .iter()
            .all(|metric| metric.name != "server/elapsed_per_operation")
    );
    assert_eq!(metric("cycles").statistic, Statistic::Total);
    assert!(
        summary
            .report()
            .contains("server/cpu_time_per_operation    100.000 ns/op, approximate"),
        "{}",
        summary.report()
    );
}
