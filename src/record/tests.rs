// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use serde_json::json;

fn valid() -> ProcessRecord {
    ProcessRecord::new("fixture", json!({ "items": 1 }))
        .with_signature("output")
        .with_extension("note", json!("kept"))
        .with_sample(
            Sample::new(4)
                .with(Metric::total("bytes", Unit::Bytes, 8))
                .with(Metric::mean("latency", Unit::Nanoseconds, 2.5).higher_is_better()),
        )
}

fn rejects(record: ProcessRecord, message: &str) {
    let error = record.validate().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains(message), "{error}");
}

#[test]
fn valid_records_round_trip_as_one_line() {
    let record = valid();
    let mut out = Vec::new();
    record.emit(&mut out).unwrap();
    assert_eq!(out.iter().filter(|byte| **byte == b'\n').count(), 1);
    let parsed: ProcessRecord = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed, record);
    assert_eq!(parsed.samples[0].metric("bytes").unwrap().unit, Unit::Bytes);
    assert!(parsed.samples[0].metric("missing").is_none());
}

#[test]
fn structural_errors_are_rejected() {
    rejects(
        ProcessRecord {
            version: 2,
            ..valid()
        },
        "unsupported record schema",
    );
    rejects(
        ProcessRecord {
            schema: "other".to_owned(),
            ..valid()
        },
        "unsupported record schema",
    );
    rejects(
        ProcessRecord {
            workload: " ".to_owned(),
            ..valid()
        },
        "workload is empty",
    );
    rejects(
        ProcessRecord {
            parameters: json!([]),
            ..valid()
        },
        "JSON object",
    );
    rejects(
        ProcessRecord {
            signature: Some(String::new()),
            ..valid()
        },
        "signature is empty",
    );
    rejects(
        ProcessRecord {
            samples: Vec::new(),
            ..valid()
        },
        "no samples",
    );
    rejects(
        ProcessRecord::new("fixture", json!({})).with_sample(Sample::new(0)),
        "operations must be positive",
    );
    let duplicate = Sample::new(1)
        .with(Metric::total("a", Unit::Count, 1))
        .with(Metric::total("a", Unit::Count, 2));
    rejects(
        ProcessRecord::new("fixture", json!({})).with_sample(duplicate),
        "duplicate metric a",
    );
    let mut sample = Sample::new(1);
    sample.push(Metric::total("", Unit::Count, 1));
    rejects(
        ProcessRecord::new("fixture", json!({})).with_sample(sample),
        "name is empty",
    );
    for metric in [
        Metric::total("a", Unit::Count, 1).with_resolution(0),
        Metric::mean("a", Unit::Count, f64::NAN),
        Metric::mean("a", Unit::Count, -1.0),
        Metric::unavailable("a", Unit::Count, " "),
    ] {
        let error = ProcessRecord::new("fixture", json!({}))
            .with_sample(Sample::new(1).with(metric))
            .validate()
            .unwrap_err();
        assert!(error.to_string().starts_with("sample 0: a:"), "{error}");
    }
    let mut out = Vec::new();
    assert!(ProcessRecord::new("", json!({})).emit(&mut out).is_err());
    assert!(out.is_empty());
}

#[test]
fn unknown_fields_are_not_silently_accepted() {
    let mut value = serde_json::to_value(valid()).unwrap();
    value["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ProcessRecord>(value).is_err());
}

#[test]
fn only_measured_totals_are_normalized_by_operations() {
    let measured = Metric::total("a", Unit::Count, 10);
    assert_eq!(measured.per_operation(4), Some(2.5));
    assert_eq!(
        measured.clone().with_phase(Phase::Process).per_operation(4),
        None
    );
    assert_eq!(measured.with_phase(Phase::Attached).per_operation(4), None);
    assert_eq!(
        Metric::mean("b", Unit::Nanoseconds, 3.0)
            .with_phase(Phase::Process)
            .per_operation(4),
        None
    );
    let unavailable = Metric::unavailable("c", Unit::Count, "denied")
        .with_source(Source::PerfStat)
        .lower_is_better();
    assert_eq!(unavailable.per_operation(4), None);
    assert_eq!(unavailable.direction, Direction::LowerIsBetter);
    unavailable.validate().unwrap();
}

struct Rejecting;

impl Write for Rejecting {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("rejected"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn emit_reports_write_failures() {
    assert_eq!(
        valid().emit(&mut Rejecting).unwrap_err().to_string(),
        "rejected"
    );
    assert!(Rejecting.flush().is_ok());
}
