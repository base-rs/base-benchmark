// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::plan::Plan;
use crate::role::Role;
use crate::runner::{CampaignStatus, Cancellation, read_entries, read_metadata, run};
use crate::test_support::{record, roles_plan, roles_suite, script, service, shell};
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

fn valid() -> String {
    format!("echo '{}'", record("same", 100))
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
            "echo not-json".to_owned(),
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
