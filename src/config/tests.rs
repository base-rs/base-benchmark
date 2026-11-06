// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use serde_json::json;

fn metadata(configuration: Value, targets: &[&str]) -> Value {
    json!({
        "workspace_root": "/workspace",
        "packages": [
            { "name": "library", "metadata": null, "targets": [] },
            {
                "name": "workload",
                "manifest_path": "/workspace/benchmarks/work/Cargo.toml",
                "metadata": { "base-benchmark": configuration },
                "targets": targets.iter().map(|name| json!({ "name": name, "kind": ["bin"] })).chain([json!({ "name": "library", "kind": ["lib"] })]).collect::<Vec<_>>(),
            },
        ],
    })
}

fn scenario(value: Value) -> Value {
    json!({ "scenario": [value] })
}

#[test]
fn reads_unpublished_workload_packages() {
    let configuration = json!({
        "profile": "bench",
        "features": ["extra"],
        "scenario": [{
            "name": "list",
            "arguments": ["--items", "4"],
            "smoke-arguments": ["--iterations", "1"],
            "variants": [{ "name": "reference", "arguments": ["--variant", "reference"] }, { "name": "engine" }],
            "baseline": "engine",
        }, { "name": "empty" }],
    });
    let suites = parse_metadata(&metadata(configuration, &["workload"])).unwrap();
    assert_eq!(suites.len(), 1);
    let suite = &suites[0];
    assert_eq!(
        (
            suite.package.as_str(),
            suite.binary.as_str(),
            suite.profile.as_str()
        ),
        ("workload", "workload", "bench")
    );
    assert_eq!(suite.workspace_root, PathBuf::from("/workspace"));
    assert_eq!(suite.features, ["extra"]);
    let list = suite.scenario("list").unwrap();
    assert_eq!(list.smoke_arguments, ["--iterations", "1"]);
    assert_eq!(list.baseline_variant().unwrap().name, "engine");
    assert!(
        suite
            .scenario("empty")
            .unwrap()
            .baseline_variant()
            .is_none()
    );
    assert!(
        suite
            .scenario("missing")
            .unwrap_err()
            .to_string()
            .contains("available: list, empty")
    );

    let defaults = parse_metadata(&metadata(json!({ "binary": "second", "scenario": [{ "name": "a", "variants": [{ "name": "x" }, { "name": "y" }] }] }), &["first", "second"])).unwrap();
    assert_eq!(
        (defaults[0].binary.as_str(), defaults[0].profile.as_str()),
        ("second", "release")
    );
    assert_eq!(
        defaults[0].scenarios[0].baseline_variant().unwrap().name,
        "x"
    );
}

#[test]
fn invalid_configuration_is_reported_with_its_package() {
    let cases = [
        (
            metadata(json!({ "unknown": true }), &["workload"]),
            "invalid [package.metadata.base-benchmark]",
        ),
        (
            metadata(json!({ "binary": "missing" }), &["workload"]),
            "is not a binary target",
        ),
        (
            metadata(json!({}), &["one", "two"]),
            "found 2 binary targets",
        ),
        (
            metadata(scenario(json!({ "name": "bad name" })), &["workload"]),
            "names may contain only",
        ),
        (
            metadata(
                scenario(json!({ "name": "a", "variants": [{ "name": "x" }, { "name": "x" }] })),
                &["workload"],
            ),
            "duplicate variant",
        ),
        (
            metadata(
                scenario(json!({ "name": "a", "baseline": "x" })),
                &["workload"],
            ),
            "is not a variant",
        ),
        (
            metadata(
                json!({ "scenario": [{ "name": "a" }, { "name": "a" }] }),
                &["workload"],
            ),
            "duplicate scenario",
        ),
        (json!({ "packages": [] }), "no workspace_root"),
    ];
    for (value, message) in cases {
        let error = parse_metadata(&value).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    assert!(
        parse_metadata(&json!({ "workspace_root": "/w" }))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn selects_one_suite() {
    let suites = parse_metadata(&metadata(json!({}), &["workload"])).unwrap();
    let mut other = suites[0].clone();
    other.package = "other".to_owned();
    assert_eq!(select(suites.clone(), None).unwrap().package, "workload");
    assert_eq!(
        select(vec![suites[0].clone(), other.clone()], Some("other"))
            .unwrap()
            .package,
        "other"
    );
    assert!(
        select(vec![suites[0].clone(), other.clone()], None)
            .unwrap_err()
            .to_string()
            .contains("choose one with --package")
    );
    assert!(
        select(suites, Some("missing"))
            .unwrap_err()
            .to_string()
            .contains("available: workload")
    );
    assert!(
        select(Vec::new(), None)
            .unwrap_err()
            .to_string()
            .contains("no workspace package")
    );
}

#[test]
fn discovery_reports_cargo_failures() {
    let directory = tempfile::tempdir().unwrap();
    let error = discover(&cargo_program(), &directory.path().join("Cargo.toml")).unwrap_err();
    assert!(
        error.to_string().contains("cargo metadata failed"),
        "{error}"
    );
    let error = discover(Path::new("/bin/echo"), &directory.path().join("Cargo.toml")).unwrap_err();
    assert!(error.to_string().contains("expected"), "{error}");
    assert!(discover(Path::new("/nonexistent/cargo"), Path::new("Cargo.toml")).is_err());
}

#[test]
fn labels_are_portable_directory_names() {
    assert!(valid_label("a-1.b_c"));
    assert!(!valid_label(""));
    assert!(!valid_label(".hidden"));
    assert!(!valid_label("a/b"));
}

#[test]
fn tools_come_from_the_environment_or_path() {
    assert_eq!(program(None, "rustc"), PathBuf::from("rustc"));
    assert_eq!(
        program(Some(OsString::from("/opt/rustc")), "rustc"),
        PathBuf::from("/opt/rustc")
    );
}

#[test]
fn package_defaults_apply_to_scenarios_that_do_not_replace_them() {
    let configuration = json!({
        "arguments": ["--common"],
        "smoke-arguments": ["--iterations", "1"],
        "variants": [{ "name": "reference" }, { "name": "engine", "arguments": ["--engine"] }],
        "baseline": "engine",
        "scenario": [
            { "name": "inherited", "arguments": ["--own"] },
            { "name": "replaced", "smoke-arguments": [], "variants": [{ "name": "only" }] },
            { "name": "rebased", "baseline": "reference" },
        ],
    });
    let suite = parse_metadata(&metadata(configuration, &["workload"]))
        .unwrap()
        .remove(0);
    let inherited = suite.scenario("inherited").unwrap();
    assert_eq!(inherited.arguments, ["--common", "--own"]);
    assert_eq!(inherited.smoke_arguments, ["--iterations", "1"]);
    assert_eq!(inherited.baseline_variant().unwrap().name, "engine");
    let replaced = suite.scenario("replaced").unwrap();
    assert!(replaced.smoke_arguments.is_empty());
    assert_eq!(
        (
            replaced.variants.len(),
            replaced.baseline_variant().unwrap().name.as_str()
        ),
        (1, "only")
    );
    assert_eq!(
        suite
            .scenario("rebased")
            .unwrap()
            .baseline_variant()
            .unwrap()
            .name,
        "reference"
    );
}
