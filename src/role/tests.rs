// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

fn captures(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn ready_patterns_match_whole_lines_and_capture_values() {
    let address = Pattern::parse("{address}").unwrap();
    assert_eq!(
        address.matches("127.0.0.1:8080"),
        Some(captures(&[("address", "127.0.0.1:8080")]))
    );
    assert_eq!(address.matches(""), None);

    let listening = Pattern::parse("listening on {host}:{port} ({{ok}})").unwrap();
    assert_eq!(listening.captures().collect::<Vec<_>>(), ["host", "port"]);
    // The host contains the separator; each capture takes the longest text that lets the rest match.
    assert_eq!(
        listening.matches("listening on [::1]:80:8080 ({ok})"),
        Some(captures(&[("host", "[::1]:80"), ("port", "8080")]))
    );
    assert_eq!(listening.matches("listening on :8080 ({ok})"), None);
    assert_eq!(listening.matches("listening on a:1 ({ok}) "), None);
    assert_eq!(listening.matches("now listening on a:1 ({ok})"), None);

    // Overlapping occurrences of the literal after a capture are each tried.
    let overlapping = Pattern::parse("{a}aa").unwrap();
    assert_eq!(overlapping.matches("baaa"), Some(captures(&[("a", "ba")])));
    let exact = Pattern::parse("ready").unwrap();
    assert_eq!(exact.matches("ready"), Some(BTreeMap::new()));
    assert_eq!(exact.matches("ready!"), None);
    // Multibyte text is split only at character boundaries.
    let unicode = Pattern::parse("{a}é").unwrap();
    assert_eq!(unicode.matches("ééé"), Some(captures(&[("a", "éé")])));
}

#[test]
fn invalid_ready_patterns_are_rejected() {
    for (pattern, message) in [
        ("", "empty"),
        ("{address", "not closed"),
        ("address}", "not opened"),
        ("{}", "capture names"),
        ("{a b}", "capture names"),
        ("{a}{b}", "separated by text"),
        ("{a}:{a}", "captured twice"),
    ] {
        let error = Pattern::parse(pattern).unwrap_err();
        assert!(error.to_string().contains(message), "{pattern}: {error}");
    }
}

#[test]
fn references_are_substituted_and_other_braces_kept() {
    let roles = BTreeSet::from(["server", "relay"]);
    assert_eq!(
        references("--to={server.address}/{relay.port}{json}{other.x}{", &roles),
        [("server", "address"), ("relay", "port")]
    );
    assert_eq!(
        references("{{server.address}", &roles),
        [("server", "address")]
    );
    let captured = BTreeMap::from([("server".to_owned(), captures(&[("address", "a:1")]))]);
    assert_eq!(
        substitute(
            "{\"to\":\"{server.address}\"} {server.port} {missing.address} {server.address",
            &captured
        ),
        "{\"to\":\"a:1\"} {server.port} {missing.address} {server.address"
    );
    assert_eq!(substitute("{{server.address}}", &captured), "{a:1}");
}

fn configuration(value: serde_json::Value) -> io::Result<Role> {
    let configuration = RoleConfiguration::deserialize(value).unwrap();
    configuration.resolve(&|message| invalid_input(message))
}

#[test]
fn role_declarations_resolve_defaults() {
    let service = configuration(serde_json::json!({
        "name": "server", "kind": "service", "subject": true, "ready": "{address}",
    }))
    .unwrap();
    assert_eq!(service.program, RoleProgram::Subject);
    assert_eq!(
        service.ready,
        Some(ReadyLine {
            pattern: "{address}".to_owned(),
            timeout_ms: DEFAULT_READY_TIMEOUT_MS
        })
    );
    assert_eq!(
        service.stop,
        Some(Stop::Signal {
            signal: "SIGTERM".to_owned(),
            grace_ms: DEFAULT_STOP_GRACE_MS
        })
    );
    let exiting = configuration(serde_json::json!({
        "name": "server", "kind": "service", "external": "nginx", "ready": "up",
        "ready-timeout-seconds": 0.0001, "stop": "exit", "arguments": ["-c", "x"],
    }))
    .unwrap();
    assert_eq!(exiting.stop, Some(Stop::Exit));
    assert_eq!(exiting.ready.unwrap().timeout_ms, 1);
    let signalled = configuration(serde_json::json!({
        "name": "server", "kind": "service", "external": "nginx", "ready": "up",
        "stop": "SIGQUIT", "stop-grace-seconds": 2.5,
    }))
    .unwrap();
    assert_eq!(
        signalled.stop,
        Some(Stop::Signal {
            signal: "SIGQUIT".to_owned(),
            grace_ms: 2500
        })
    );
    let driver = configuration(serde_json::json!({
        "name": "client", "kind": "driver", "external": "load", "measure-arguments": ["--duration", "8s"],
    }))
    .unwrap();
    assert_eq!((driver.ready, driver.stop), (None, None));
    assert_eq!(driver.measure_arguments, ["--duration", "8s"]);
}

#[test]
fn invalid_role_declarations_are_rejected() {
    for (value, message) in [
        (
            serde_json::json!({ "name": "a", "kind": "driver" }),
            "exactly one program",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "driver", "subject": true, "external": "x" }),
            "exactly one program",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "driver", "external": "x/y" }),
            "external program names",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "service", "subject": true }),
            "needs `ready`",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "service", "subject": true, "ready": "{" }),
            "not closed",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "service", "subject": true, "ready": "x", "ready-timeout-seconds": 0 }),
            "positive",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "service", "subject": true, "ready": "x", "stop-grace-seconds": -1 }),
            "positive",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "service", "subject": true, "ready": "x", "stop": "SIGKILL" }),
            "stop must be",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "service", "subject": true, "ready": "x", "stop": "exit", "stop-grace-seconds": 1 }),
            "no stop-grace-seconds",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "driver", "subject": true, "ready": "x" }),
            "no ready line",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "driver", "subject": true, "stop": "exit" }),
            "no ready line",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "driver", "subject": true, "arguments": ["x"] }),
            "scenario's own",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "driver", "subject": true, "smoke-arguments": ["x"] }),
            "scenario's own",
        ),
        (
            serde_json::json!({ "name": "a", "kind": "driver", "subject": true, "measure-arguments": ["x"] }),
            "scenario's own",
        ),
    ] {
        let error = configuration(value.clone()).unwrap_err();
        assert!(error.to_string().contains(message), "{value}: {error}");
        assert!(error.to_string().starts_with("role \"a\""), "{error}");
    }
}

fn role(
    name: &str,
    kind: RoleKind,
    program: RoleProgram,
    arguments: &[&str],
    ready: Option<&str>,
) -> Role {
    Role {
        name: name.to_owned(),
        kind,
        program,
        arguments: arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        smoke_arguments: Vec::new(),
        measure_arguments: Vec::new(),
        ready: ready.map(|pattern| ReadyLine {
            pattern: pattern.to_owned(),
            timeout_ms: 1,
        }),
        stop: None,
    }
}

fn external(name: &str) -> RoleProgram {
    RoleProgram::External {
        name: name.to_owned(),
    }
}

#[test]
fn roles_are_checked_together() {
    let valid = [
        role(
            "client",
            RoleKind::Driver,
            external("load"),
            &["{server.address}", "{relay.port}"],
            None,
        ),
        role(
            "server",
            RoleKind::Service,
            RoleProgram::Subject,
            &[],
            Some("{address}"),
        ),
        role(
            "relay",
            RoleKind::Service,
            external("relay"),
            &["{server.address}"],
            Some("port {port}"),
        ),
    ];
    let context = |message: String| invalid_input(message);
    validate_roles(&valid, &[], &context).unwrap();
    validate_roles(&[], &[], &context).unwrap();
    // The subject's arguments may refer to earlier services too.
    let subject = "{relay.port}".to_owned();
    assert!(
        validate_roles(&valid, &[&subject], &context)
            .unwrap_err()
            .to_string()
            .contains("role \"server\": {relay.port} names no value")
    );

    let mut late = valid.clone();
    late.swap(1, 2);
    assert!(
        validate_roles(&late, &[], &context)
            .unwrap_err()
            .to_string()
            .contains("role \"relay\": {server.address}")
    );
    let mut unknown = valid.clone();
    unknown[0].arguments = vec!["{server.port}".to_owned()];
    assert!(validate_roles(&unknown, &[], &context).is_err());
    let mut mode = valid.clone();
    mode[0].arguments.clear();
    mode[0].smoke_arguments = vec!["{relay.address}".to_owned()];
    assert!(validate_roles(&mode, &[], &context).is_err());
    mode[0].smoke_arguments.clear();
    mode[0].measure_arguments = vec!["{client.x}".to_owned()];
    assert!(validate_roles(&mode, &[], &context).is_err());

    for (roles, message) in [
        (
            vec![valid[0].clone(), valid[0].clone()],
            "duplicate role name",
        ),
        (
            vec![role(
                "a.b",
                RoleKind::Driver,
                RoleProgram::Subject,
                &[],
                None,
            )],
            "invalid or duplicate",
        ),
        (
            vec![role(
                "a b",
                RoleKind::Driver,
                RoleProgram::Subject,
                &[],
                None,
            )],
            "invalid or duplicate",
        ),
        (vec![valid[1].clone()], "exactly one driver, not 0"),
        (
            vec![
                valid[0].clone(),
                valid[1].clone(),
                role("other", RoleKind::Driver, external("x"), &[], None),
            ],
            "exactly one driver, not 2",
        ),
        (
            vec![role("client", RoleKind::Driver, external("x"), &[], None)],
            "exactly one role runs the subject, not 0",
        ),
        (
            vec![
                valid[1].clone(),
                role("client", RoleKind::Driver, RoleProgram::Subject, &[], None),
            ],
            "not 2",
        ),
    ] {
        let error = validate_roles(&roles, &[], &context).unwrap_err();
        assert!(error.to_string().contains(message), "{message}: {error}");
    }
}
