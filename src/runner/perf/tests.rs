// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::record::Measurement;

fn events() -> Vec<String> {
    vec!["instructions:u".to_owned(), "cycles:u".to_owned()]
}

#[test]
fn wraps_the_workload_command() {
    let (program, arguments) = wrap(
        Path::new("perf"),
        &events(),
        Path::new("/out.csv"),
        Path::new("/bin/work"),
        &["--fast".to_owned()],
    );
    assert_eq!(program, PathBuf::from("perf"));
    assert_eq!(
        arguments,
        [
            "stat",
            "--no-big-num",
            "-x,",
            "-e",
            "instructions:u,cycles:u",
            "-o",
            "/out.csv",
            "--",
            "/bin/work",
            "--fast"
        ]
    );
}

#[test]
fn counters_are_whole_process_totals() {
    let metrics = parse(
        "# started\n\n12,,instructions:u,100,100.00,,\n34,,cycles:u,100,100.00,,\n",
        &events(),
    )
    .unwrap();
    assert_eq!(metrics[0].measurement, Measurement::Total { value: 12 });
    assert_eq!(
        (metrics[1].phase, metrics[1].source),
        (Phase::Process, Source::PerfStat)
    );
    assert_eq!(metrics[1].per_operation(10), None);
}

#[test]
fn missing_or_uncounted_events_fail() {
    assert!(
        parse("12,,instructions:u,100\n", &events())
            .unwrap_err()
            .to_string()
            .contains("did not report cycles:u")
    );
    assert!(
        parse("<not counted>,,instructions:u\n34,,cycles:u\n", &events())
            .unwrap_err()
            .to_string()
            .contains("did not count instructions:u")
    );
}
