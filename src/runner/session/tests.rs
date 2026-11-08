// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::plan::Plan;
use crate::role::Role;
use crate::runner::{CampaignStatus, Cancellation, read_entries, read_metadata, run};
use crate::test_support::{record, roles_plan, roles_suite, script, service, shell};
use crate::window::{WINDOW_START, WINDOW_STARTED, WINDOW_STOP};
use std::path::Path;

/// Wait until FILE exists, polling, in a fixture script.
fn until_exists(file: &Path) -> String {
    format!("until [ -e '{}' ]; do sleep 0.01; done", file.display())
}

/// Wait until the process whose ID FILE holds has exited, though it is not yet reaped.
fn until_exited(file: &Path) -> String {
    format!(
        "{}; until [ \"$(cut -d' ' -f3 /proc/$(cat '{}')/stat)\" = Z ]; do sleep 0.01; done",
        until_exists(file),
        file.display()
    )
}

/// A plan of one session: `server` is the subject's script and `client` the driver's.
fn session(directory: &Path, server: &str, client: &str) -> Plan {
    let server = script(directory, "server.sh", server);
    let client = script(directory, "client.sh", client);
    roles_plan(roles_suite(&server, &client, &[]), &directory.join("lock"))
}

/// A driver that marks an empty window and prints its record.
fn valid() -> String {
    format!(
        "echo '{WINDOW_START}'; echo '{WINDOW_STOP}'; echo '{}'",
        record("same", 100)
    )
}

/// A service that prints its ready line and then waits for signals.
fn serving(address: &str) -> String {
    format!("echo 'ready {address}'\nsleep 30 & wait\n")
}

/// Run a campaign that must fail, returning its error and journal.
fn failed(plan: Plan, directory: &Path) -> (String, Vec<SampleEntry>) {
    let output = directory.join("campaign");
    let error = run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap_err();
    let metadata = read_metadata(&output).unwrap();
    assert_eq!(metadata.status, CampaignStatus::Failed, "{error}");
    assert_eq!(metadata.error.as_deref(), Some(error.to_string().as_str()));
    (
        error.to_string(),
        read_entries(&output.join("samples.jsonl")).unwrap(),
    )
}

#[test]
fn sessions_start_services_in_order_pass_captured_values_and_stop_in_reverse() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("log");
    let log = log.display();
    let server = script(
        directory.path(),
        "server.sh",
        &format!(
            "echo \"start server $1\" >> '{log}'\ntrap 'echo stop server >> \"{log}\"; exit 0' TERM\necho warming up\necho 'ready 127.0.0.1:4000'\nsleep 30 & wait\n"
        ),
    );
    let relay = script(
        directory.path(),
        "relay.sh",
        &format!(
            "echo \"start relay $1\" >> '{log}'\ntrap 'echo stop relay >> \"{log}\"; exit 0' TERM\necho 'port 9000'\nsleep 30 & wait\n"
        ),
    );
    let client = script(
        directory.path(),
        "client.sh",
        &format!(
            "echo \"driver $*\" >> '{log}'\nprintf '%s %s %s\\n' \"${{#BASE_BENCHMARK_STACK_PADDING}}\" \"${{HOME-unset}}\" \"$(pwd)\" >&2\n{}\n",
            valid()
        ),
    );
    let mut suite = roles_suite(&server, &client, &["base", "fast"]);
    suite.scenarios[0].roles[0]
        .arguments
        .push("{relay.port}".to_owned());
    let relay_role: Role = service(
        "relay",
        shell(),
        &[&relay.display().to_string(), "{server.address}"],
        "port {port}",
    );
    suite.scenarios[0].roles.push(relay_role);
    let plan = roles_plan(suite, &directory.path().join("lock"));
    let output = directory.path().join("campaign");
    let campaign = run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    assert_eq!(campaign.summary.subjects[0].processes, 1);

    let entries = read_entries(&output.join("samples.jsonl")).unwrap();
    let log = std::fs::read_to_string(directory.path().join("log")).unwrap();
    let mut expected = String::new();
    for entry in &entries {
        let variant = entry.subject.rsplit('/').next().unwrap();
        expected.push_str(&format!(
            "start server {variant}\nstart relay 127.0.0.1:4000\ndriver 127.0.0.1:4000 9000 measure\nstop relay\nstop server\n"
        ));
    }
    assert_eq!(log, expected);
    let shell = Path::new(crate::test_support::SHELL);
    for entry in &entries {
        assert!(entry.error.is_none() && entry.record.is_some());
        assert!(entry.command.is_empty() && entry.executable_sha256.is_empty());
        let names: Vec<_> = entry.roles.iter().map(|role| role.role.as_str()).collect();
        assert_eq!(names, ["server", "relay", "client"]);
        let [server, relay, client] = &entry.roles[..] else {
            unreachable!()
        };
        assert_eq!(server.ready_line.as_deref(), Some("ready 127.0.0.1:4000"));
        assert_eq!(server.captures["address"], "127.0.0.1:4000");
        assert_eq!(server.stdout, "warming up\n");
        assert_eq!(relay.captures["port"], "9000");
        assert_eq!(
            relay.command[2..],
            ["127.0.0.1:4000".to_owned()],
            "captured values replace references"
        );
        assert!(
            server.command[0].ends_with("/bin/0"),
            "the subject is staged"
        );
        assert_eq!(
            client.command[0],
            shell.canonicalize().unwrap().display().to_string(),
            "external programs run from their resolved path"
        );
        assert_eq!(
            client.executable_sha256,
            crate::artifact::sha256_file(shell).unwrap()
        );
        for role in &entry.roles {
            assert!(role.started && role.exit_code == Some(0) && !role.killed_after_grace);
            assert_eq!(role.host, read_metadata(&output).unwrap().host.hostname);
            assert!(role.stack_padding.is_some());
        }
        // Each role has its own stack padding, a cleared environment, and the workspace root as its directory.
        assert_eq!(
            client.stderr,
            format!("{} unset /\n", client.stack_padding.unwrap())
        );
    }
    // Services that do not run the subject are the same for every subject.
    assert_eq!(entries[0].roles[1].command, entries[1].roles[1].command);
    assert_ne!(entries[0].roles[0].command, entries[1].roles[0].command);
}

#[test]
fn services_that_fail_before_or_during_the_driver_stop_the_run() {
    let directory = tempfile::tempdir().unwrap();
    let started = directory.path().join("started");
    let cases = [
        (
            "echo starting; exit 3".to_owned(),
            valid(),
            "role server (service) exited before it was ready",
        ),
        (
            format!("echo 'ready a:1'\n{}\nexit 0\n", until_exists(&started)),
            format!("touch '{}'; sleep 30", started.display()),
            "role server (service) exited before it was stopped",
        ),
        (
            format!("trap 'exit 3' TERM\n{}", serving("a:1")),
            valid(),
            "role server (service) did not stop successfully: exit code Some(3), signal None",
        ),
        (
            serving("a:1"),
            "echo broken; exit 2".to_owned(),
            "role client (driver) failed with exit code Some(2), signal None",
        ),
        (
            serving("a:1"),
            format!("echo '{WINDOW_START}'; echo '{WINDOW_STOP}'; echo not-json"),
            "role client (driver) stdout is not exactly one process record",
        ),
    ];
    for (index, (server, client, message)) in cases.into_iter().enumerate() {
        let directory = directory.path().join(index.to_string());
        std::fs::create_dir(&directory).unwrap();
        let _ = std::fs::remove_file(&started);
        let (error, entries) = failed(session(&directory, &server, &client), &directory);
        assert!(error.contains(message), "{message}: {error}");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert!(entry.record.is_none());
        assert_eq!(entry.roles.len(), 2, "every role is journalled");
        if index == 0 {
            // Its output before the failure is kept: it was read to the end once the service exited.
            assert_eq!(entry.roles[0].stdout, "starting\n");
        }
        // A process that had to be stopped after the failure was killed.
        assert!(
            entry
                .roles
                .iter()
                .all(|role| !role.started || role.exit_code.is_some() || role.signal.is_some())
        );
    }
}

#[test]
fn a_service_that_never_becomes_ready_is_killed_and_later_roles_never_start() {
    let directory = tempfile::tempdir().unwrap();
    // Whether it printed anything before the timeout depends on scheduling, so only what does not is checked.
    let mut plan = session(directory.path(), "echo 'not ready'; sleep 30", &valid());
    plan.roles[0].ready.as_mut().unwrap().timeout_ms = 300;
    let (error, entries) = failed(plan, directory.path());
    assert!(
        error.contains(
            "role server (service) printed no line matching \"ready {address}\" within 300 ms"
        ),
        "{error}"
    );
    let [server, client] = &entries[0].roles[..] else {
        unreachable!()
    };
    assert_eq!((server.started, server.signal), (true, Some(9)));
    assert!(
        "not ready\n".starts_with(&server.stdout),
        "{}",
        server.stdout
    );
    assert!(!client.started && client.exit_code.is_none());
    assert_eq!(
        client.command[2], "{server.address}",
        "an unstarted role is journalled as planned"
    );
}

#[test]
fn an_earlier_service_that_exits_while_a_later_one_starts_fails() {
    let directory = tempfile::tempdir().unwrap();
    let started = directory.path().join("started");
    let first = script(
        directory.path(),
        "first.sh",
        &format!("echo 'ready a:1'\n{}\nexit 0\n", until_exists(&started)),
    );
    let second = script(
        directory.path(),
        "second.sh",
        &format!("touch '{}'; sleep 30", started.display()),
    );
    let client = script(directory.path(), "client.sh", &valid());
    let mut suite = roles_suite(&first, &client, &[]);
    suite.scenarios[0].roles.push(service(
        "second",
        shell(),
        &[&second.display().to_string()],
        "ready",
    ));
    let plan = roles_plan(suite, &directory.path().join("lock"));
    let (error, _) = failed(plan, directory.path());
    assert!(
        error.contains("role server (service) exited before it was stopped"),
        "{error}"
    );
}

#[test]
fn services_can_stop_by_exiting_once_the_driver_has_finished() {
    let directory = tempfile::tempdir().unwrap();
    let pid = directory.path().join("driver.pid");
    let exiting = format!("echo 'ready a:1'\n{}\nexit 0\n", until_exited(&pid));
    let mut plan = session(
        directory.path(),
        &exiting,
        &format!("echo $$ > '{}'; {}", pid.display(), valid()),
    );
    plan.roles[0].stop = Some(Stop::Exit);
    let output = directory.path().join("campaign");
    run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    let entries = read_entries(&output.join("samples.jsonl")).unwrap();
    assert_eq!(entries[0].roles[0].exit_code, Some(0));

    // Exiting before the driver has finished fails, as does exiting unsuccessfully.
    let started = directory.path().join("started");
    for (server, client, message) in [
        (
            format!("echo 'ready a:1'\n{}\nexit 0\n", until_exists(&started)),
            format!("touch '{}'; sleep 30", started.display()),
            "role server (service) exited before the driver had finished",
        ),
        (
            exiting.replace("exit 0", "exit 4"),
            format!("echo $$ > '{}'; {}", pid.display(), valid()),
            "role server (service) did not stop successfully: exit code Some(4)",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let _ = std::fs::remove_file(&pid);
        let mut plan = session(directory.path(), &server, &client);
        plan.roles[0].stop = Some(Stop::Exit);
        let (error, _) = failed(plan, directory.path());
        assert!(error.contains(message), "{message}: {error}");
    }
}

#[test]
fn a_signalled_service_that_exits_before_its_turn_to_stop_fails() {
    let directory = tempfile::tempdir().unwrap();
    let pid = directory.path().join("first.pid");
    // When the second service is stopped, it kills the first, which the runner stops after it.
    let first = script(
        directory.path(),
        "first.sh",
        &format!("echo $$ > '{}'\n{}", pid.display(), serving("a:1")),
    );
    let second = script(
        directory.path(),
        "second.sh",
        &format!(
            "stop() {{ kill -9 $(cat '{}'); {}; exit 0; }}\ntrap stop TERM\necho ready\nsleep 30 & wait\n",
            pid.display(),
            until_exited(&pid)
        ),
    );
    let client = script(directory.path(), "client.sh", &valid());
    let mut suite = roles_suite(&first, &client, &[]);
    suite.scenarios[0].roles.push(service(
        "second",
        shell(),
        &[&second.display().to_string()],
        "ready",
    ));
    let plan = roles_plan(suite, &directory.path().join("lock"));
    let (error, entries) = failed(plan, directory.path());
    assert!(
        error.contains("role server (service) exited before it was stopped"),
        "{error}"
    );
    assert_eq!(entries[0].roles[0].signal, Some(9));
    assert_eq!(entries[0].roles[1].exit_code, Some(0));
}

#[test]
fn a_service_that_outlasts_its_grace_period_is_killed_and_journalled() {
    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(
        directory.path(),
        "trap '' TERM\necho 'ready a:1'\nwhile :; do sleep 1; done\n",
        &valid(),
    );
    plan.roles[0].stop = Some(Stop::Signal {
        signal: "SIGTERM".to_owned(),
        grace_ms: 200,
    });
    let output = directory.path().join("campaign");
    run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    let server = &read_entries(&output.join("samples.jsonl")).unwrap()[0].roles[0];
    assert!(server.killed_after_grace);
    assert_eq!(server.signal, Some(9));
}

#[test]
fn services_stopped_by_their_signal_stop_successfully() {
    let directory = tempfile::tempdir().unwrap();
    // The service dies from its stop signal.
    let mut plan = session(
        directory.path(),
        "echo 'ready a:1'\nexec sleep 30\n",
        &valid(),
    );
    plan.roles[0].stop = Some(Stop::Signal {
        signal: "SIGINT".to_owned(),
        grace_ms: 20_000,
    });
    let output = directory.path().join("campaign");
    run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    let server = &read_entries(&output.join("samples.jsonl")).unwrap()[0].roles[0];
    assert_eq!((server.signal, server.killed_after_grace), (Some(2), false));
}

#[test]
fn the_session_deadline_applies_while_services_start() {
    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(directory.path(), "sleep 30", &valid());
    plan.timeout_ms = 300;
    let (error, entries) = failed(plan, directory.path());
    assert!(
        error.contains("the session timed out after 300 ms"),
        "{error}"
    );
    assert!(!entries[0].roles[1].started);
}

#[test]
fn cancellation_while_a_service_stops_kills_it() {
    let directory = tempfile::tempdir().unwrap();
    let stopping = directory.path().join("stopping");
    let plan = session(
        directory.path(),
        &format!(
            "trap \"touch '{}'\" TERM\necho 'ready a:1'\nwhile :; do sleep 1; done\n",
            stopping.display()
        ),
        &valid(),
    );
    let cancellation = Cancellation::new();
    let canceller = cancellation.clone();
    let wait = stopping.clone();
    let stop = std::thread::spawn(move || {
        while !wait.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        canceller.cancel();
    });
    let output = directory.path().join("campaign");
    let error = run(plan, &output, &cancellation, &mut Vec::new()).unwrap_err();
    stop.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let entries = read_entries(&output.join("samples.jsonl")).unwrap();
    assert_eq!(entries[0].roles[0].signal, Some(9));
    assert!(!entries[0].roles[0].killed_after_grace);
    assert_eq!(entries[0].roles[1].exit_code, Some(0));
}

#[test]
fn the_session_deadline_and_cancellation_kill_every_role() {
    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(directory.path(), &serving("a:1"), "sleep 30");
    plan.timeout_ms = 300;
    let (error, entries) = failed(plan, directory.path());
    assert!(
        error.contains("the session timed out after 300 ms"),
        "{error}"
    );
    assert!(entries[0].timed_out);
    // On a slow host the deadline can pass before the driver starts; whatever started was killed.
    assert!(
        entries[0]
            .roles
            .iter()
            .all(|role| !role.started || role.signal == Some(9))
    );

    let directory = tempfile::tempdir().unwrap();
    let started = directory.path().join("started");
    let plan = session(
        directory.path(),
        &serving("a:1"),
        &format!("touch '{}'; sleep 30", started.display()),
    );
    let cancellation = Cancellation::new();
    let canceller = cancellation.clone();
    let wait = started.clone();
    let stop = std::thread::spawn(move || {
        while !wait.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        canceller.cancel();
    });
    let output = directory.path().join("campaign");
    let error = run(plan, &output, &cancellation, &mut Vec::new()).unwrap_err();
    stop.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(
        read_metadata(&output).unwrap().status,
        CampaignStatus::Cancelled
    );
    let entries = read_entries(&output.join("samples.jsonl")).unwrap();
    assert!(!entries[0].timed_out);
    // The session reports the cancellation, not the exits it caused.
    assert_eq!(
        entries[0].error.as_deref(),
        Some("the campaign was cancelled")
    );
    assert!(entries[0].roles.iter().all(|role| role.signal == Some(9)));
}

#[test]
fn executables_are_verified_before_any_role_starts() {
    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(directory.path(), &serving("a:1"), &valid());
    plan.programs[0].sha256 = "0".repeat(64);
    let (error, entries) = failed(plan, directory.path());
    assert!(
        error.contains("role client (driver) external program shell changed during the campaign"),
        "{error}"
    );
    assert!(entries[0].roles.iter().all(|role| !role.started));

    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(directory.path(), &serving("a:1"), &valid());
    plan.artifacts.artifacts[0].sha256 = "0".repeat(64);
    let (error, _) = failed(plan, directory.path());
    assert!(
        error.contains("role server (service) baseline: executable changed after preparation"),
        "{error}"
    );

    // A program that cannot be started fails once the roles before it are ready.
    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(directory.path(), &serving("a:1"), &valid());
    let unexecutable = directory.path().join("unexecutable");
    std::fs::write(&unexecutable, "data").unwrap();
    plan.programs[0].path = unexecutable.clone();
    plan.programs[0].sha256 = crate::artifact::sha256_file(&unexecutable).unwrap();
    let (error, entries) = failed(plan, directory.path());
    assert!(
        error.contains("role client (driver) could not start"),
        "{error}"
    );
    assert_eq!(entries[0].roles[0].signal, Some(9));
}

#[test]
fn escaped_descendants_of_a_service_fail_the_session() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left");
    let server = format!(
        "setsid sh -c 'touch \"$0\"; exec sleep 30' '{0}' &\n{1}\n{2}",
        left.display(),
        until_exists(&left),
        serving("a:1")
    );
    let (error, _) = failed(
        session(directory.path(), &server, &valid()),
        directory.path(),
    );
    assert!(
        error.contains("role server (service) left a descendant outside its process group"),
        "{error}"
    );
}

#[test]
fn ready_lines_are_read_line_by_line_with_bounded_lines() {
    let pattern = Pattern::parse("ready {address}").unwrap();
    let (sender, events) = mpsc::channel();
    let text = "first\r\nready\nready a:1\r\nafter\n";
    let tail = read_ready(text.as_bytes(), &pattern, 0, &sender);
    assert_eq!(tail, b"first\r\nready\n");
    let Ok(Event::Ready { line, captures }) = events.try_recv() else {
        panic!("the ready line is reported")
    };
    assert_eq!(line, "ready a:1");
    assert_eq!(captures["address"], "a:1");
    // An overlong line never matches, the retained output is bounded, and output that ends without a ready line is kept.
    let overlong = format!("ready a:{}\nunfinished", "1".repeat(LINE_LIMIT));
    let tail = read_ready(overlong.as_bytes(), &pattern, 3, &sender);
    assert!(matches!(events.try_recv(), Ok(Event::Unready { role: 3 })));
    assert!(events.try_recv().is_err());
    assert_eq!(tail.len(), STDERR_LIMIT);
    assert!(tail.ends_with(b"111\nunfinished"));
}

/// Each role's process ID, which its script wrote to `NAME.pid`.
fn pids(directory: &Path, names: &[&str]) -> Vec<i32> {
    names
        .iter()
        .map(|name| {
            std::fs::read_to_string(directory.join(format!("{name}.pid")))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        })
        .collect()
}

/// The kills, of `groups` only, that this thread has made since the last call.
fn kills_of(groups: &[i32]) -> Vec<i32> {
    process::KILLED
        .with(|killed| std::mem::take(&mut *killed.borrow_mut()))
        .into_iter()
        .filter(|group| groups.contains(group))
        .collect()
}

/// A plan whose services `server`, `second`, and `third` and driver `client` each record their process ID; `third` runs `third_body` after doing so, and the others serve or drive.
fn recording_pids(directory: &Path, third_body: &str, client_body: &str) -> Plan {
    let record_pid = |name: &str| {
        format!(
            "echo $$ > '{}'\n",
            directory.join(format!("{name}.pid")).display()
        )
    };
    let server = script(
        directory,
        "server.sh",
        &format!("{}{}", record_pid("server"), serving("a:1")),
    );
    let second = script(
        directory,
        "second.sh",
        &format!("{}echo ready\nsleep 30 & wait\n", record_pid("second")),
    );
    let third = script(
        directory,
        "third.sh",
        &format!("{}{third_body}", record_pid("third")),
    );
    let client = script(
        directory,
        "client.sh",
        &format!("{}{client_body}", record_pid("client")),
    );
    let mut suite = roles_suite(&server, &client, &[]);
    for (name, path) in [("second", &second), ("third", &third)] {
        suite.scenarios[0].roles.push(service(
            name,
            shell(),
            &[&path.display().to_string()],
            "ready",
        ));
    }
    roles_plan(suite, &directory.join("lock"))
}

#[test]
fn after_a_failure_roles_are_killed_and_reaped_in_reverse_start_order() {
    let directory = tempfile::tempdir().unwrap();
    let started = directory.path().join("started");
    // The third service exits once the driver has started, while the others still run.
    let plan = recording_pids(
        directory.path(),
        &format!("echo ready\n{}\nexit 0\n", until_exists(&started)),
        &format!("touch '{}'; sleep 30", started.display()),
    );
    kills_of(&[]);
    let (error, _) = failed(plan, directory.path());
    assert!(
        error.contains("role third (service) exited before it was stopped"),
        "{error}"
    );
    let [server, second, third, client] =
        pids(directory.path(), &["server", "second", "third", "client"])[..]
    else {
        unreachable!()
    };
    // Those still running are killed, last started first; then every group's remnants are killed as each is reaped, in the same order.
    assert_eq!(
        kills_of(&[server, second, third, client]),
        [client, second, server, client, third, second, server]
    );
}

#[test]
fn cancellation_kills_roles_in_reverse_start_order() {
    let directory = tempfile::tempdir().unwrap();
    let started = directory.path().join("started");
    let plan = recording_pids(
        directory.path(),
        &serving("a:1").replace("ready a:1", "ready"),
        &format!("touch '{}'; sleep 30", started.display()),
    );
    let cancellation = Cancellation::new();
    let canceller = cancellation.clone();
    let wait = started.clone();
    // The canceller thread's own kills are exactly those of the cancellation.
    let stop = std::thread::spawn(move || {
        while !wait.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        canceller.cancel();
        process::KILLED.with(|killed| killed.take())
    });
    let output = directory.path().join("campaign");
    let error = run(plan, &output, &cancellation, &mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let kills = stop.join().unwrap();
    let pids = pids(directory.path(), &["server", "second", "third", "client"]);
    let reverse: Vec<i32> = pids.into_iter().rev().collect();
    // The driver may mark that it started before the runner registers its group, which is then killed as it is registered, after the services.
    assert!(
        kills == reverse || kills == reverse[1..],
        "{kills:?} is not {reverse:?}"
    );
}

#[test]
fn service_output_after_readiness_is_drained() {
    let directory = tempfile::tempdir().unwrap();
    let written = directory.path().join("written");
    // Far more than a pipe holds: the service would block writing it, and never get to tell the driver, unless the runner keeps reading.
    let server = format!(
        "echo 'ready a:1'\nhead -c 4194304 /dev/zero || exit 1\ntouch '{}'\nsleep 30 & wait\n",
        written.display()
    );
    let client = format!("{}\n{}", until_exists(&written), valid());
    let plan = session(directory.path(), &server, &client);
    let output = directory.path().join("campaign");
    run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    let server = &read_entries(&output.join("samples.jsonl")).unwrap()[0].roles[0];
    assert_eq!(server.signal, Some(15));
}

#[test]
fn a_service_that_prints_its_ready_line_and_exits_exited_before_it_was_stopped() {
    // The ready line arrives only after the service has exited, so its exit is reported first.
    let directory = tempfile::tempdir().unwrap();
    let server = "leader=$$\n(until [ \"$(cut -d' ' -f3 /proc/$leader/stat)\" = Z ]; do sleep 0.01; done; echo 'ready a:1') &\nexit 0\n";
    let (error, entries) = failed(
        session(directory.path(), server, &valid()),
        directory.path(),
    );
    assert!(
        error.contains("role server (service) exited before it was stopped"),
        "{error}"
    );
    assert_eq!(entries[0].roles[0].ready_line.as_deref(), Some("ready a:1"));
    assert!(!entries[0].roles[1].started);
    // One that stops by exiting exited before the driver had finished.
    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(directory.path(), server, &valid());
    plan.roles[0].stop = Some(Stop::Exit);
    let (error, _) = failed(plan, directory.path());
    assert!(
        error.contains("role server (service) exited before the driver had finished"),
        "{error}"
    );
}

#[test]
fn a_service_that_closes_its_output_without_a_ready_line_is_unready_until_it_exits() {
    let directory = tempfile::tempdir().unwrap();
    let closed = directory.path().join("closed");
    // It closes stdout, then exits only once the test has seen that it kept running.
    let server = format!(
        "exec >&-\ntouch '{}'\n{}\nexit 0\n",
        closed.display(),
        until_exists(&directory.path().join("go"))
    );
    let plan = session(directory.path(), &server, &valid());
    let go = directory.path().join("go");
    let wait = closed.clone();
    let release = std::thread::spawn(move || {
        while !wait.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::write(go, "").unwrap();
    });
    let (error, _) = failed(plan, directory.path());
    release.join().unwrap();
    assert!(
        error.contains("role server (service) exited before it was ready"),
        "{error}"
    );
}

#[test]
fn arguments_that_name_no_captured_value_fail_the_role() {
    // Discovered scenarios are checked when planned; a plan built by hand is checked when the role starts.
    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(directory.path(), &serving("a:1"), &valid());
    plan.roles[1].arguments.push("{server.port}".to_owned());
    let (error, entries) = failed(plan, directory.path());
    assert!(
        error.contains(
            "role client (driver) argument \"{server.port}\": {server.port} names no captured value"
        ),
        "{error}"
    );
    assert!(!entries[0].roles[1].started);
    assert_eq!(entries[0].roles[0].signal, Some(9));
}

#[test]
fn a_panicking_session_kills_and_reaps_its_roles() {
    let directory = tempfile::tempdir().unwrap();
    let pid = directory.path().join("server.pid");
    let plan = session(
        directory.path(),
        &format!("echo $$ > '{}'\n{}", pid.display(), serving("a:1")),
        &valid(),
    );
    let staged = plan
        .artifacts
        .artifacts
        .iter()
        .map(|artifact| (artifact.label.clone(), artifact.executable.clone()))
        .collect();
    let cancellation = Cancellation::new();
    let collection = Collection {
        plan: &plan,
        output: directory.path(),
        staged: &staged,
        cancellation: &cancellation,
        host: None,
    };
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut session = Session::new(&collection, Instant::now());
        session.identify(&plan.subjects[0], &[None, None]).unwrap();
        session.start(0).unwrap();
        session.wait_ready(0).unwrap();
        panic!("a defect while the service runs");
    }));
    assert!(unwound.is_err());
    let pid = std::fs::read_to_string(&pid).unwrap();
    assert!(
        !Path::new(&format!("/proc/{}", pid.trim())).exists(),
        "the service was killed and reaped"
    );
}

#[test]
fn services_stop_by_signals_that_base_benchmark_started_with_ignored_or_blocked() {
    use nix::sys::signal::Signal;
    crate::test_support::with_signals(
        "runner::session::tests::services_stop_by_signals_that_base_benchmark_started_with_ignored_or_blocked",
        &[Signal::SIGINT, Signal::SIGQUIT, Signal::SIGHUP],
        &[
            Signal::SIGINT,
            Signal::SIGQUIT,
            Signal::SIGHUP,
            Signal::SIGTERM,
        ],
        || {
            for signal in ["SIGINT", "SIGQUIT", "SIGHUP", "SIGTERM"] {
                let directory = tempfile::tempdir().unwrap();
                // A shell cannot trap a signal ignored when it started, and never receives one that stays blocked, so with that signal ignored or blocked this service would outlast its grace period.
                let server = format!("trap 'exit 0' {}\n{}", &signal[3..], serving("a:1"));
                let mut plan = session(directory.path(), &server, &valid());
                plan.roles[0].stop = Some(Stop::Signal {
                    signal: signal.to_owned(),
                    grace_ms: 20_000,
                });
                // Longer than the grace period, so that a service that outlasts it is killed and journalled rather than timed out.
                plan.timeout_ms = 60_000;
                let output = directory.path().join("campaign");
                run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
                let server = &read_entries(&output.join("samples.jsonl")).unwrap()[0].roles[0];
                assert_eq!(
                    (server.exit_code, server.killed_after_grace),
                    (Some(0), false),
                    "{signal}"
                );
            }
        },
    );
}

#[test]
fn a_session_whose_cleanup_panics_partway_still_reaps_every_other_role() {
    let directory = tempfile::tempdir().unwrap();
    let plan = session(directory.path(), &serving("a:1"), "exec sleep 30");
    let staged = plan
        .artifacts
        .artifacts
        .iter()
        .map(|artifact| (artifact.label.clone(), artifact.executable.clone()))
        .collect();
    let cancellation = Cancellation::new();
    let collection = Collection {
        plan: &plan,
        output: directory.path(),
        staged: &staged,
        cancellation: &cancellation,
        host: None,
    };
    let mut session = Session::new(&collection, Instant::now());
    session.identify(&plan.subjects[0], &[None, None]).unwrap();
    session.start(0).unwrap();
    session.wait_ready(0).unwrap();
    session.start(1).unwrap();
    let group = |index: usize| session.running[index].as_ref().unwrap().group;
    let (server, driver) = (group(0), group(1));
    // Reaped behind the session's back, the driver, reaped first, cannot be reaped again, so cleanup panics there.
    let pid = nix::unistd::Pid::from_raw(driver);
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL).unwrap();
    nix::sys::wait::waitpid(pid, None).unwrap();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.finish()));
    assert!(unwound.is_err());
    let server = format!("/proc/{server}");
    assert!(
        Path::new(&server).exists(),
        "the server was killed but is not reaped yet"
    );
    drop(session);
    assert!(
        !Path::new(&server).exists(),
        "dropping the session reaped the server"
    );
}

/// A service that burns `ticks` clock ticks of CPU before it is ready and again when stopped, and on SIGUSR1 burns 5 ticks and then creates `done`; otherwise it waits. Its ready line carries its process ID.
fn burning_service(done: &Path) -> String {
    format!(
        r#"ticks() {{ read -r stat < /proc/$$/stat; set -- ${{stat##*") "}}; t=$(( ${{12}} + ${{13}} )); }}
burn() {{ n=$1; ticks; s=$t; while ticks; [ $(( t - s )) -lt "$n" ]; do :; done; }}
echo "control ${{BASE_BENCHMARK_CONTROL-unset}}" >&2
burn 3
trap 'burn 3; exit 0' TERM
trap 'burn 5; : > "{}"' USR1
sleep 300 &
echo "ready $$"
while :; do wait; done
"#,
        done.display()
    )
}

/// A driver that waits until the service whose process ID is its first argument is blocked, marks its window with an acknowledged start, runs `work` inside it, and prints its record.
fn marking_driver(work: &str) -> String {
    format!(
        r#"[ "$BASE_BENCHMARK_CONTROL" = stdio ] || exit 8
until [ "$(cut -d' ' -f3 /proc/$1/stat)" = S ]; do sleep 0.01; done
echo '{WINDOW_START}'
read -r acknowledgement
[ "$acknowledgement" = '{WINDOW_STARTED}' ] || exit 9
{work}
echo '{WINDOW_STOP}'
echo '{}'
"#,
        record("same", 100)
    )
}

fn window_value(entry: &SampleEntry, name: &str) -> u64 {
    let metric = entry.roles[0]
        .window
        .iter()
        .find(|metric| metric.name == name)
        .unwrap();
    assert_eq!(
        (metric.phase, metric.source),
        (Phase::Window, Source::Rusage)
    );
    match metric.measurement {
        crate::record::Measurement::Total { value } => value,
        _ => panic!("{name} is a total"),
    }
}

#[test]
fn service_cpu_is_measured_only_between_the_drivers_markers() {
    let ticks = process::ticks_per_second();
    let tick = 1_000_000_000 / ticks;
    let directory = tempfile::tempdir().unwrap();
    let done = directory.path().join("done");
    // A service that burns CPU only before it is ready and after it is stopped shows none in an empty window.
    let quiet = session(
        directory.path(),
        &burning_service(&done),
        &marking_driver(":"),
    );
    let output = directory.path().join("quiet");
    let campaign = run(quiet, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    let entries = read_entries(&output.join("samples.jsonl")).unwrap();
    assert_eq!(window_value(&entries[0], "cpu_time"), 0);
    assert_eq!(window_value(&entries[0], "user_cpu_time"), 0);
    assert_eq!(entries[0].roles[0].stderr, "control unset\n");
    assert!(entries[0].roles[1].window.is_empty());
    let summary = &campaign.summary.subjects[0];
    let metric = |name: &str| {
        summary
            .metrics
            .iter()
            .find(|metric| metric.name == name)
            .unwrap()
    };
    assert_eq!(
        metric("server/cpu_time").statistic,
        crate::summary::Statistic::Total
    );
    assert_eq!(
        metric("server/cpu_time_per_operation").statistic,
        crate::summary::Statistic::ApproximatePerOperation
    );

    // CPU the service spends inside the window, at the driver's request, is counted.
    let directory = tempfile::tempdir().unwrap();
    let done = directory.path().join("done");
    let busy = session(
        directory.path(),
        &burning_service(&done),
        &marking_driver(&format!("kill -USR1 $1; {}", until_exists(&done))),
    );
    let output = directory.path().join("busy");
    run(busy, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    let entries = read_entries(&output.join("samples.jsonl")).unwrap();
    let split =
        window_value(&entries[0], "user_cpu_time") + window_value(&entries[0], "system_cpu_time");
    assert!(split >= 5 * tick, "{split}");
    // Rounding each of the split's parts down to a tick loses less than two ticks of the precise total.
    let total = window_value(&entries[0], "cpu_time");
    assert!(total >= 3 * tick, "{total}");
}

#[test]
fn drivers_must_mark_one_window() {
    let record = format!("echo '{}'", record("same", 100));
    let cases = [
        (
            record.clone(),
            "role client (driver) sent no window start marker",
        ),
        (
            format!("echo '{WINDOW_START}'; {record}"),
            "sent no window stop marker",
        ),
        (
            format!("echo '{WINDOW_STOP}'; sleep 30"),
            "sent the window stop marker before the start marker",
        ),
        (
            format!("echo '{WINDOW_START}'; echo '{WINDOW_START}'; sleep 30"),
            "sent the window start marker twice",
        ),
        (
            format!("echo '{WINDOW_START}'; echo '{WINDOW_STOP}'; echo '{WINDOW_STOP}'; sleep 30"),
            "sent the window stop marker twice",
        ),
        (
            "echo 'base-benchmark window pause'; sleep 30".to_owned(),
            "sent an invalid window marker \"base-benchmark window pause\"",
        ),
        (
            format!("{record}; echo '{WINDOW_START}'; echo '{WINDOW_STOP}'"),
            "sent the window marker \"base-benchmark window start\" after output of its record",
        ),
        (
            format!("echo '{WINDOW_START}'; printf '%s' '{WINDOW_STOP}'"),
            "ended its output with the window marker \"base-benchmark window stop\" without a newline",
        ),
    ];
    for (index, (client, message)) in cases.into_iter().enumerate() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path().join(index.to_string());
        std::fs::create_dir(&directory).unwrap();
        let (error, entries) = failed(session(&directory, &serving("a:1"), &client), &directory);
        assert!(error.contains(message), "{message}: {error}");
        assert!(entries[0].record.is_none());
    }
}

#[test]
fn escaped_descendants_of_the_driver_fail_the_session() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left");
    let client = format!(
        "setsid sh -c 'touch \"$0\"; exec sleep 30' '{0}' &\n{1}\n{2}",
        left.display(),
        until_exists(&left),
        valid()
    );
    let (error, _) = failed(
        session(directory.path(), &serving("a:1"), &client),
        directory.path(),
    );
    assert!(
        error.contains("role client (driver) left a descendant outside its process group"),
        "{error}"
    );
}

#[test]
fn the_drivers_stdout_carries_markers_then_its_record() {
    let (sender, events) = mpsc::channel();
    let text = format!(
        "{WINDOW_START}\r\n\nbase-benchmark window pause\n{WINDOW_STOP}\nrecord part\n{{\"last\":true}}"
    );
    let (record, truncated) = read_driver(text.as_bytes(), &sender);
    assert_eq!(
        (record.as_slice(), truncated),
        (&b"\nrecord part\n{\"last\":true}"[..], false)
    );
    let reported = |events: &Receiver<Event>| -> Vec<_> {
        events
            .try_iter()
            .map(|event| match event {
                Event::Marker(marker) => Some(marker),
                Event::Closed => None,
                _ => panic!("only markers and the close are reported"),
            })
            .collect()
    };
    // Blank lines before a marker are not the record's output.
    assert_eq!(
        reported(&events),
        [
            Some(Marker::Start),
            Some(Marker::Invalid("base-benchmark window pause".to_owned())),
            Some(Marker::Stop),
            None
        ]
    );
    // Markers are whole lines before the record.
    let text = format!("{{}}\n{WINDOW_STOP}\r\n{WINDOW_START}");
    let (record, _) = read_driver(text.as_bytes(), &sender);
    assert_eq!(record, b"{}\n");
    assert_eq!(
        reported(&events),
        [
            Some(Marker::Late(WINDOW_STOP.to_owned())),
            Some(Marker::Unterminated(WINDOW_START.to_owned())),
            None
        ]
    );
    // The record is bounded, whether or not its lines end.
    let long = vec![b'x'; STDOUT_LIMIT + 5];
    let (record, truncated) = read_driver(&long[..], &sender);
    assert_eq!((record.len(), truncated), (STDOUT_LIMIT, true));
    let mut lines = vec![b'y'; STDOUT_LIMIT - 1];
    lines.extend_from_slice(b"\nmore\n");
    let (record, truncated) = read_driver(&lines[..], &sender);
    assert_eq!((record.len(), truncated), (STDOUT_LIMIT, true));
}

#[test]
fn window_metrics_are_differences_or_unavailable() {
    let at = |total_ns, user_ticks, system_ticks| CpuTime {
        total_ns,
        user_ticks,
        system_ticks,
    };
    let metrics = window_metrics(Some(at(100, 2, 3)), Some(at(350, 5, 3)), 100);
    let values: Vec<_> = metrics
        .iter()
        .map(|metric| {
            (
                metric.name.as_str(),
                metric.measurement.clone(),
                metric.resolution,
            )
        })
        .collect();
    use crate::record::Measurement::Total;
    assert_eq!(
        values,
        [
            ("cpu_time", Total { value: 250 }, None),
            (
                "user_cpu_time",
                Total { value: 30_000_000 },
                Some(10_000_000)
            ),
            ("system_cpu_time", Total { value: 0 }, Some(10_000_000)),
        ]
    );
    for missing in [
        window_metrics(None, Some(at(1, 1, 1)), 100),
        window_metrics(Some(at(1, 1, 1)), None, 100),
    ] {
        assert_eq!(missing.len(), 3);
        assert!(missing.iter().all(|metric| metric.phase == Phase::Window
            && matches!(
                metric.measurement,
                crate::record::Measurement::Unavailable { .. }
            )));
    }
}

#[test]
fn the_drivers_stdin_closes_once_the_snapshotted_start_is_acknowledged() {
    // The driver reads its stdin to the end after the acknowledgement, which it reaches only if the runner closes it.
    let directory = tempfile::tempdir().unwrap();
    let client = format!(
        "echo '{WINDOW_START}'\nread -r acknowledgement\n[ \"$acknowledgement\" = '{WINDOW_STARTED}' ] || exit 9\ncat > /dev/null\necho '{WINDOW_STOP}'\necho '{}'\n",
        record("same", 100)
    );
    let plan = session(directory.path(), &serving("a:1"), &client);
    WINDOW_START_STEPS.with(|steps| steps.take());
    let output = directory.path().join("campaign");
    run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    // The services were snapshotted before the acknowledgement was written, so before the driver's first counted operation.
    assert_eq!(
        WINDOW_START_STEPS.with(|steps| steps.take()),
        ["snapshot", "acknowledged"]
    );
}

#[test]
fn undeclared_shared_placement_is_reported_when_the_campaign_starts() {
    // A screening campaign that fails has no summary to warn in, so the warning is printed as it starts.
    let directory = tempfile::tempdir().unwrap();
    let mut plan = session(directory.path(), "echo starting; exit 3", &valid());
    plan.shared_cores = vec![crate::placement::SharedCore {
        roles: ["server".to_owned(), "client".to_owned()],
        cpus: vec![1, 5],
    }];
    let mut progress = Vec::new();
    let output = directory.path().join("campaign");
    run(plan, &output, &Cancellation::new(), &mut progress).unwrap_err();
    let progress = String::from_utf8(progress).unwrap();
    assert!(
        progress.starts_with("base-benchmark: warning: roles share physical cores, which the scenario does not declare, so they contend for them: server and client on CPUs 1, 5\n"),
        "{progress}"
    );
}

/// `plan` with its external program replaced by a shell that answers `--version` by running `version`.
fn with_version(directory: &Path, mut plan: Plan, version: &str) -> Plan {
    let program = crate::test_support::write_executable(
        &directory.join("program"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then\n{version}\nfi\nexec /bin/sh \"$@\"\n"
        ),
    );
    plan.programs[0] = crate::plan::ExternalProgram::identify("shell", &program).unwrap();
    plan
}

/// The version a campaign recorded for its external program.
fn recorded_version(output: &Path) -> Option<String> {
    read_metadata(output).unwrap().plan.programs[0]
        .version
        .clone()
}

#[test]
fn external_programs_report_their_versions_when_the_campaign_starts() {
    let long = "x".repeat(STDERR_LIMIT);
    for (index, (body, version)) in [
        (
            "echo; echo '  tool 1.2.3 '; echo more; exit 0",
            Some("tool 1.2.3"),
        ),
        ("echo 'v2' >&2; exit 0", Some("v2")),
        // The first line of stderr, however much follows it.
        (
            &*format!("echo 'v3' >&2; for i in 1 2 3; do echo {long} >&2; done; exit 0"),
            Some("v3"),
        ),
        ("echo 'tool 1.2.3'; exit 1", None),
        ("exit 0", None),
    ]
    .into_iter()
    .enumerate()
    {
        let directory = tempfile::tempdir().unwrap();
        let plan = session(directory.path(), &serving("a:1"), &valid());
        assert_eq!(plan.programs[0].version, None, "plans do not run programs");
        let plan = with_version(directory.path(), plan, body);
        let output = directory.path().join(format!("campaign-{index}"));
        run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
        assert_eq!(recorded_version(&output).as_deref(), version, "{body}");
    }
}

#[test]
fn versions_are_asked_under_the_host_lock_on_the_roles_cpus() {
    let directory = tempfile::tempdir().unwrap();
    let lock = directory.path().join("lock");
    let plan = session(directory.path(), &serving("a:1"), &valid());
    let mut plan = with_version(
        directory.path(),
        plan,
        &format!(
            "echo \"$(grep Cpus_allowed_list /proc/self/status | cut -f2) $(flock -n '{}' true && echo unlocked || echo locked)\"; exit 0",
            lock.display()
        ),
    );
    plan.roles[1].cpus = vec![0];
    let output = directory.path().join("campaign");
    run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    assert_eq!(recorded_version(&output).as_deref(), Some("0 locked"));
}

#[test]
fn cancellation_stops_a_version_probe() {
    let directory = tempfile::tempdir().unwrap();
    let pid = directory.path().join("probe.pid");
    let plan = session(directory.path(), &serving("a:1"), &valid());
    let plan = with_version(
        directory.path(),
        plan,
        &format!("echo $$ > '{}'; exec sleep 30", pid.display()),
    );
    let cancellation = Cancellation::new();
    let canceller = cancellation.clone();
    let wait = pid.clone();
    let stop = std::thread::spawn(move || {
        while !wait.exists() || std::fs::read_to_string(&wait).unwrap().is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        canceller.cancel();
        process::KILLED.with(|killed| killed.take())
    });
    let output = directory.path().join("campaign");
    kills_of(&[]);
    let error = run(plan, &output, &cancellation, &mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    // The probe ran under the campaign's cancellation, which killed it: from the canceller, or, if it cancelled before the probe's group was registered, when it was registered. Either way its group is also killed once more as it is reaped. Under any other cancellation, only that last kill happens on these threads.
    let probe: i32 = std::fs::read_to_string(&pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let cancelled = stop.join().unwrap();
    assert!(
        cancelled.iter().all(|group| *group == probe),
        "{cancelled:?}"
    );
    assert_eq!(cancelled.len() + kills_of(&[probe]).len(), 2);
    assert_eq!(recorded_version(&output), None);
    assert!(
        read_entries(&output.join("samples.jsonl"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_version_that_cannot_be_recorded_fails_the_campaign() {
    // The probe blocks the metadata's rewrite by occupying the name it is written through.
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("campaign");
    let plan = session(directory.path(), &serving("a:1"), &valid());
    let plan = with_version(
        directory.path(),
        plan,
        &format!(
            "mkdir '{}'; echo 'tool 1'; exit 0",
            output.join("metadata.json.tmp").display()
        ),
    );
    let error = run(plan, &output, &Cancellation::new(), &mut Vec::new())
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with(&format!("{}:", output.join("metadata.json").display())),
        "{error}"
    );
    assert!(
        read_entries(&output.join("samples.jsonl"))
            .unwrap()
            .is_empty()
    );
}

/// A record whose `responses_invalid` total is `count`.
fn counted(count: u64) -> String {
    record("same", 100).replace(
        "\"metrics\":[",
        &format!("\"metrics\":[{{\"name\":\"responses_invalid\",\"unit\":\"count\",\"phase\":\"measured\",\"source\":\"workload\",\"direction\":\"lower_is_better\",\"measurement\":{{\"kind\":\"total\",\"value\":{count}}}}},"),
    )
}

/// A campaign of two variants whose driver counts invalid responses only from the server of the `fast` variant, which its ready line names.
fn invalid_plan(directory: &Path, fast: &str) -> Plan {
    let server = script(
        directory,
        "server.sh",
        "echo \"ready $1\"\nsleep 30 & wait\n",
    );
    let client = script(
        directory,
        "client.sh",
        &format!(
            "echo '{WINDOW_START}'; echo '{WINDOW_STOP}'\nif [ \"$1\" = fast ]; then echo '{fast}'; else echo '{}'; fi\n",
            counted(0)
        ),
    );
    let mut suite = roles_suite(&server, &client, &["slow", "fast"]);
    suite.scenarios[0].roles[0].invalid_responses = Some("responses_invalid".to_owned());
    roles_plan(suite, &directory.join("lock"))
}

#[test]
fn invalid_responses_make_comparisons_invalid_in_screening() {
    let directory = tempfile::tempdir().unwrap();
    let plan = invalid_plan(directory.path(), &counted(3));
    let output = directory.path().join("campaign");
    let campaign = run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    let pair = &campaign.summary.pairs[0];
    assert_eq!(
        (pair.invalid.as_deref(), pair.metrics.len()),
        (Some("baseline/fast received invalid responses"), 0)
    );
    assert!(
        campaign.warnings[0].starts_with("baseline/fast: 3 invalid response(s) in 1 round(s)"),
        "{:?}",
        campaign.warnings
    );
    assert!(
        campaign
            .report
            .contains("  invalid: baseline/fast received invalid responses\n")
    );
    // The samples' numbers are kept and presented.
    let fast = &campaign.summary.subjects[1];
    assert!(
        fast.metrics
            .iter()
            .any(|metric| metric.name == "responses_invalid")
    );
    assert!(
        campaign
            .summary
            .aggregation
            .contains("per successful operation")
    );

    // Without invalid responses, the comparison is made.
    let directory = tempfile::tempdir().unwrap();
    let plan = invalid_plan(directory.path(), &counted(0));
    let campaign = run(
        plan,
        &directory.path().join("campaign"),
        &Cancellation::new(),
        &mut Vec::new(),
    )
    .unwrap();
    assert!(
        campaign.summary.pairs[0].invalid.is_none()
            && !campaign.summary.pairs[0].metrics.is_empty()
    );

    // The driver's records are checked as any workload's: one workload and signature across the campaign.
    let directory = tempfile::tempdir().unwrap();
    let plan = invalid_plan(
        directory.path(),
        &counted(0).replace("\"same\"", "\"other\""),
    );
    let (error, _) = failed(plan, directory.path());
    assert!(error.contains("did not do equivalent work"), "{error}");

    // A driver whose record lacks the declared total has failed.
    let directory = tempfile::tempdir().unwrap();
    let plan = invalid_plan(directory.path(), &record("same", 100));
    let (error, _) = failed(plan, directory.path());
    assert!(
        error.contains("role client (driver) reports no responses_invalid"),
        "{error}"
    );
}

#[test]
fn invalid_responses_fail_recorded_campaigns() {
    let directory = tempfile::tempdir().unwrap();
    let mut plan = invalid_plan(directory.path(), &counted(1));
    let allowed = nix::sched::sched_getaffinity(nix::unistd::Pid::from_raw(0)).unwrap();
    let cpu = (0..nix::sched::CpuSet::count())
        .find(|cpu| allowed.is_set(*cpu).unwrap())
        .unwrap();
    plan.mode = crate::plan::Mode::Recorded;
    plan.rounds = 10;
    plan.decision_rule = Some("rule".to_owned());
    plan.idle_host = true;
    plan.shared_placement = true;
    for role in &mut plan.roles {
        role.cpus = vec![cpu];
    }
    let (error, entries) = failed(plan, directory.path());
    assert!(
        error.contains(
            "role client (driver) received 1 invalid response(s), so the comparison is invalid"
        ),
        "{error}"
    );
    // The failed sample keeps the record that counted them.
    assert!(entries.last().unwrap().record.is_some());
}
