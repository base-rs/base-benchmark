// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! One session of a multi-process scenario: every role of one sample.
//!
//! Every executable is verified first. Services start in order, each waited on until it prints its ready line, whose captured values later roles' arguments receive; then the driver runs to completion, marking its measured window on stdout, where the runner snapshots every service's CPU time at each marker and acknowledges the start on the driver's stdin; then the services are stopped in reverse order. One deadline covers the whole session. On any failure, the deadline, or cancellation, every role's process group still running is killed, in reverse start order, and every role is reaped, also in reverse start order, before the session returns, so before the host lock is released. A session that unwinds from a panic does the same when it is dropped. Helper threads only read output and wait for exits, and report them to the session as events, so the session can always react to the deadline.

use super::process::{self, CpuTime, Launch, OUTPUT_GRACE, STDERR_LIMIT, STDOUT_LIMIT, read_tail};
use super::{Collection, RoleEntry, SampleEntry, cancelled};
use crate::plan::{PlannedRole, Subject};
use crate::record::{Metric, Phase, ProcessRecord, Source, Unit, invalid};
use crate::role::{Pattern, RoleKind, RoleProgram, Stop, substitute};
use crate::window::{WINDOW_START, WINDOW_STARTED, WINDOW_STOP};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// A service's stdout line longer than this cannot be its ready line; the excess is not kept.
const LINE_LIMIT: usize = 64 * 1024;

/// What helper threads report to the session.
enum Event {
    /// The service being started printed its ready line: earlier services are ready already.
    Ready {
        line: String,
        captures: BTreeMap<String, String>,
    },
    /// The service being started closed its stdout without printing its ready line.
    Unready { role: usize },
    /// The role's process exited; it is not reaped yet.
    Exited { role: usize },
    /// The driver marked its window.
    Marker(Marker),
    /// The driver's stdout closed, after every marker on it was reported.
    Closed,
}

/// A line on the driver's stdout that begins like a window marker.
#[derive(Debug, PartialEq, Eq)]
enum Marker {
    Start,
    Stop,
    Invalid(String),
    /// A marker after output of the record had begun.
    Late(String),
    /// A marker that ends the output without a newline.
    Unterminated(String),
}

#[cfg(test)]
thread_local! {
    /// The steps this thread took at a window's start, in order: snapshotting the services and acknowledging the start, an order the driver cannot observe.
    static WINDOW_START_STEPS: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Where the driver's window is, with each service's CPU time at the start once it has begun.
enum Window {
    Before,
    Open(Vec<Option<CpuTime>>),
    Closed,
}

/// A role's output, once its stream has closed.
enum Output {
    Stdout((Vec<u8>, bool)),
    Stderr(Vec<u8>),
}

/// How a service was stopped.
#[derive(Clone, Copy)]
enum Stopping {
    /// By its declared signal, by number.
    Signal(i32),
    /// Its group was killed after the grace period.
    KilledAfterGrace,
    /// It exited by itself after the driver finished.
    Exit,
}

struct Running {
    child: Child,
    group: i32,
    started: Instant,
    outputs: Receiver<Output>,
    exited: bool,
    /// Its stdout closed before it printed its ready line.
    unready: bool,
    stopping: Option<Stopping>,
    stdout: Option<(Vec<u8>, bool)>,
    /// A descendant that left the group kept its output open after it exited.
    escaped: bool,
    /// Reaping it has been attempted, so its process ID may be reused and its group must not be signalled again.
    reaped: bool,
}

struct Session<'a> {
    collection: &'a Collection<'a>,
    roles: &'a [PlannedRole],
    programs: Vec<PathBuf>,
    arguments: Vec<Vec<String>>,
    entries: Vec<RoleEntry>,
    running: Vec<Option<Running>>,
    sender: Sender<Event>,
    events: Receiver<Event>,
    deadline: Instant,
    timed_out: bool,
    captured: BTreeMap<String, BTreeMap<String, String>>,
    /// Every role has been killed if need be and reaped, and its output collected.
    finished: bool,
    /// Where acknowledgements of the window's start go.
    driver_input: Option<ChildStdin>,
    /// Whether the driver's stdout has closed.
    closed: bool,
    window: Window,
}

/// Run one session of `subject`, recording every role in `entry`, and the driver's record when it succeeds.
pub(super) fn collect(
    collection: &Collection,
    subject: &Subject,
    stack_padding: &[Option<usize>],
    entry: &mut SampleEntry,
) -> io::Result<()> {
    let started = Instant::now();
    let mut session = Session::new(collection, started);
    let identified = session.identify(subject, stack_padding);
    let driven = identified.and_then(|()| session.drive());
    session.finish();
    let result = driven.and_then(|()| session.check());
    entry.duration_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    entry.timed_out = session.timed_out;
    entry.roles = std::mem::take(&mut session.entries);
    entry.record = result?;
    Ok(())
}

impl<'a> Session<'a> {
    fn new(collection: &'a Collection<'a>, started: Instant) -> Self {
        let plan = collection.plan;
        let (sender, events) = mpsc::channel();
        Session {
            collection,
            roles: &plan.roles,
            programs: Vec::new(),
            arguments: Vec::new(),
            entries: Vec::new(),
            running: plan.roles.iter().map(|_| None).collect(),
            sender,
            events,
            deadline: started + Duration::from_millis(plan.timeout_ms),
            timed_out: false,
            captured: BTreeMap::new(),
            finished: false,
            driver_input: None,
            closed: false,
            window: Window::Before,
        }
    }

    /// Each role's program, arguments, and identity, journalled before anything starts; then verify every executable.
    fn identify(&mut self, subject: &Subject, stack_padding: &[Option<usize>]) -> io::Result<()> {
        let plan = self.collection.plan;
        let mut verifications = Vec::new();
        for (index, role) in self.roles.iter().enumerate() {
            let (program, arguments, sha256) = match &role.program {
                RoleProgram::Subject => {
                    let artifact = self.collection.artifact(subject);
                    let executable = self.collection.staged[&subject.artifact].clone();
                    verifications
                        .push((index, Verification::Artifact(artifact, executable.clone())));
                    (
                        executable,
                        subject.arguments.clone(),
                        artifact.sha256.clone(),
                    )
                }
                RoleProgram::Binary {
                    package,
                    binary,
                    features,
                } => {
                    let found = plan
                        .artifacts
                        .helper_index(package, binary, features)
                        .expect("plans check that the helpers their roles run were prepared");
                    let executable = self.collection.staged[&super::helper_key(found)].clone();
                    let artifact = &plan.artifacts.helpers[found].artifact;
                    verifications
                        .push((index, Verification::Artifact(artifact, executable.clone())));
                    (executable, role.arguments.clone(), artifact.sha256.clone())
                }
                RoleProgram::External { name } => {
                    let program = plan
                        .programs
                        .iter()
                        .find(|program| &program.name == name)
                        .expect("plans identify the external programs their roles run");
                    verifications.push((index, Verification::External(program)));
                    (
                        program.path.clone(),
                        role.arguments.clone(),
                        program.sha256.clone(),
                    )
                }
            };
            self.entries.push(RoleEntry {
                role: role.name.clone(),
                kind: role.kind,
                host: self.collection.host.map(str::to_owned),
                command: command(&program, &arguments),
                cpus: role.cpus.clone(),
                stack_padding: stack_padding[index],
                executable_sha256: sha256,
                started: false,
                ready_line: None,
                captures: BTreeMap::new(),
                exit_code: None,
                signal: None,
                killed_after_grace: false,
                stdout: String::new(),
                stderr: String::new(),
                window: Vec::new(),
            });
            self.programs.push(program);
            self.arguments.push(arguments);
        }
        for (index, verification) in verifications {
            let verified = match verification {
                Verification::Artifact(artifact, executable) => artifact.verify_at(&executable),
                Verification::External(program) => program.verify(),
            };
            verified.map_err(|error| self.failure(index, &error.to_string()))?;
        }
        Ok(())
    }

    /// Start each service and wait until it is ready, run the driver, then stop the services in reverse order.
    fn drive(&mut self) -> io::Result<()> {
        // Plans list the driver last.
        let driver = self.roles.len() - 1;
        for index in 0..=driver {
            self.start(index)?;
            if index < driver {
                self.wait_ready(index)?;
            }
        }
        // Every marker is reported before the driver's stdout closes, so the window is complete once both the driver has exited and its stdout has closed. Output a descendant holds open past the grace period is reported as escaped after cleanup.
        let mut grace = None;
        loop {
            let exited = self.running[driver]
                .as_ref()
                .expect("the driver started")
                .exited;
            if exited && self.closed {
                break;
            }
            let until = match exited {
                true => *grace.get_or_insert_with(|| Instant::now() + OUTPUT_GRACE),
                false => self.deadline,
            };
            match self.receive(until.min(self.deadline))? {
                Some(Event::Marker(marker)) => self.mark(marker)?,
                Some(_) => {}
                None => break,
            }
            if let Some(role) = self.early_exit() {
                return Err(self.exited_early(role));
            }
        }
        for index in (0..driver).rev() {
            self.stop(index)?;
        }
        Ok(())
    }

    /// Whether a started role's process has exited, reported or not. Exits are reported by separate threads, in no particular order, so decisions that depend on two processes check them directly.
    fn exited(&self, index: usize) -> bool {
        self.running[index]
            .as_ref()
            .is_some_and(|running| running.exited || process::has_exited(running.group))
    }

    /// A service that exited before it was stopped. One that stops by exiting may exit once the driver has.
    fn early_exit(&self) -> Option<usize> {
        let driver = self.roles.len() - 1;
        (0..driver).find(|&index| {
            self.running[index]
                .as_ref()
                .is_some_and(|running| running.stopping.is_none())
                && self.exited(index)
                && !(self.roles[index].stop == Some(Stop::Exit) && self.exited(driver))
        })
    }

    fn start(&mut self, index: usize) -> io::Result<()> {
        let collection = self.collection;
        let role = &self.roles[index];
        let arguments = self.arguments[index]
            .iter()
            .map(|argument| substitute(argument, &self.captured))
            .collect::<Result<Vec<String>, String>>()
            .map_err(|error| self.failure(index, &error))?;
        self.entries[index].command = command(&self.programs[index], &arguments);
        let mut environment = collection.environment(self.entries[index].stack_padding);
        environment.extend(role.environment.clone());
        let stdin = match role.kind {
            RoleKind::Service => Stdio::null(),
            RoleKind::Driver => Stdio::piped(),
        };
        let launch = Launch {
            program: &self.programs[index],
            arguments: &arguments,
            environment: &environment,
            directory: &collection.plan.working_directory,
            cpus: &role.cpus,
        };
        let (mut child, group) = process::start(&launch, stdin, collection.cancellation)
            .map_err(|error| self.failure(index, &format!("could not start: {error}")))?;
        let started = Instant::now();
        self.entries[index].started = true;
        let (outputs, received) = mpsc::channel();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let input = child.stdin.take();
        // Kept before any thread starts, since starting one can panic, so that the session kills and reaps the role whatever happens next.
        self.running[index] = Some(Running {
            child,
            group,
            started,
            outputs: received,
            exited: false,
            unready: false,
            stopping: None,
            stdout: None,
            escaped: false,
            reaped: false,
        });
        let stdout_sender = outputs.clone();
        match &role.ready {
            Some(ready) => {
                let pattern =
                    Pattern::parse(&ready.pattern).expect("scenarios have valid ready patterns");
                let events = self.sender.clone();
                std::thread::spawn(move || {
                    let tail = read_ready(stdout, &pattern, index, &events);
                    stdout_sender.send(Output::Stdout((tail, false)))
                });
            }
            None => {
                self.driver_input = input;
                let events = self.sender.clone();
                std::thread::spawn(move || {
                    stdout_sender.send(Output::Stdout(read_driver(stdout, &events)))
                });
            }
        }
        std::thread::spawn(move || outputs.send(Output::Stderr(read_tail(stderr, STDERR_LIMIT))));
        let events = self.sender.clone();
        std::thread::spawn(move || {
            process::wait_exit(group);
            events.send(Event::Exited { role: index })
        });
        Ok(())
    }

    /// Wait until the service just started prints its ready line. Its exit and its stdout closing are reported by separate threads in either order, so it is judged by whether it printed its ready line, not by which report comes first: one that did and has exited exited before it was stopped, and one whose stdout closed without it and has exited exited before it was ready.
    fn wait_ready(&mut self, index: usize) -> io::Result<()> {
        let ready = self.roles[index]
            .ready
            .as_ref()
            .expect("services have a ready line");
        let started = self.running[index]
            .as_ref()
            .expect("the service started")
            .started;
        let by = started + Duration::from_millis(ready.timeout_ms);
        loop {
            let Some(event) = self.receive(by.min(self.deadline))? else {
                return Err(self.failure(
                    index,
                    &format!(
                        "printed no line matching {:?} within {} ms",
                        ready.pattern, ready.timeout_ms
                    ),
                ));
            };
            match event {
                // Earlier services are ready already, so only this one can become ready.
                Event::Ready { line, captures } => {
                    self.entries[index].ready_line = Some(line);
                    self.entries[index].captures = captures.clone();
                    self.captured
                        .insert(self.roles[index].name.clone(), captures);
                    return if self.exited(index) {
                        Err(self.exited_early(index))
                    } else {
                        Ok(())
                    };
                }
                Event::Exited { role } if role != index => return Err(self.exited_early(role)),
                Event::Exited { .. } | Event::Unready { .. } => {}
                Event::Marker(_) | Event::Closed => {
                    unreachable!("only the driver marks a window, and it has not started")
                }
            }
            let unready = self.running[index]
                .as_ref()
                .expect("the service started")
                .unready;
            if unready && self.exited(index) {
                return Err(self.failure(index, "exited before it was ready"));
            }
        }
    }

    fn stop(&mut self, index: usize) -> io::Result<()> {
        // A service that stops by signal and has exited by now failed; one that stops by exiting may have, and is waited for.
        if let Some(role) = self.early_exit() {
            return Err(self.exited_early(role));
        }
        let running = self.running[index]
            .as_mut()
            .expect("services started before the driver");
        let group = running.group;
        let mut by = match self.roles[index]
            .stop
            .as_ref()
            .expect("services have a stop")
        {
            Stop::Signal { signal, grace_ms } => {
                running.stopping = Some(Stopping::Signal(process::signal_number(signal)));
                process::signal_group(group, signal);
                Some(Instant::now() + Duration::from_millis(*grace_ms))
            }
            Stop::Exit => {
                running.stopping = Some(Stopping::Exit);
                None
            }
        };
        while !self.running[index]
            .as_ref()
            .expect("the service started")
            .exited
        {
            let until = by.map_or(self.deadline, |by| by.min(self.deadline));
            if self.receive(until)?.is_none() {
                // The grace period passed: kill the group and wait for the leader.
                process::kill_group(group);
                self.running[index]
                    .as_mut()
                    .expect("the service started")
                    .stopping = Some(Stopping::KilledAfterGrace);
                self.entries[index].killed_after_grace = true;
                by = None;
            }
        }
        Ok(())
    }

    /// Act on a window marker: snapshot every service at the start, acknowledge it, and at the stop snapshot them again and record what each used in the window.
    fn mark(&mut self, marker: Marker) -> io::Result<()> {
        let driver = self.roles.len() - 1;
        match (marker, &self.window) {
            (Marker::Start, Window::Before) => {
                self.window = Window::Open(self.snapshot());
                #[cfg(test)]
                WINDOW_START_STEPS.with(|steps| steps.borrow_mut().push("snapshot"));
                // The acknowledgement is the only message to the driver, so its stdin closes once it is written. A driver that does not wait for it may have closed its stdin already.
                let mut input = self.driver_input.take().expect("the driver started");
                let _ = input.write_all(format!("{WINDOW_STARTED}\n").as_bytes());
                #[cfg(test)]
                WINDOW_START_STEPS.with(|steps| steps.borrow_mut().push("acknowledged"));
                drop(input);
                Ok(())
            }
            (Marker::Stop, Window::Open(starts)) => {
                let ticks = process::ticks_per_second();
                let stops = self.snapshot();
                for (index, (start, stop)) in starts.iter().zip(stops).enumerate() {
                    self.entries[index].window = window_metrics(*start, stop, ticks);
                }
                self.window = Window::Closed;
                Ok(())
            }
            (Marker::Start, _) => Err(self.failure(driver, "sent the window start marker twice")),
            (Marker::Stop, Window::Before) => Err(self.failure(
                driver,
                "sent the window stop marker before the start marker",
            )),
            (Marker::Stop, Window::Closed) => {
                Err(self.failure(driver, "sent the window stop marker twice"))
            }
            (Marker::Invalid(line), _) => {
                Err(self.failure(driver, &format!("sent an invalid window marker {line:?}")))
            }
            (Marker::Late(line), _) => Err(self.failure(
                driver,
                &format!("sent the window marker {line:?} after output of its record; markers are lines before the record"),
            )),
            (Marker::Unterminated(line), _) => Err(self.failure(
                driver,
                &format!("ended its output with the window marker {line:?} without a newline; markers are whole lines"),
            )),
        }
    }

    /// Every service's CPU time.
    fn snapshot(&self) -> Vec<Option<CpuTime>> {
        let driver = self.roles.len() - 1;
        self.running[..driver]
            .iter()
            .map(|running| {
                process::cpu_time(
                    running
                        .as_ref()
                        .expect("services started before the driver")
                        .group,
                )
            })
            .collect()
    }

    fn exited_early(&self, role: usize) -> io::Error {
        match self.roles[role].stop {
            Some(Stop::Exit) => self.failure(role, "exited before the driver had finished"),
            _ => self.failure(role, "exited before it was stopped"),
        }
    }

    /// The next event, or `None` once `until` has passed. Reaching the session's deadline, and cancellation, are errors.
    fn receive(&mut self, until: Instant) -> io::Result<Option<Event>> {
        let cancellation = self.collection.cancellation;
        let session = until >= self.deadline;
        let event = self
            .events
            .recv_timeout(until.saturating_duration_since(Instant::now()));
        // Every exit is recorded, also one that ends the wait for a cancellation, since cleanup waits for those not yet seen.
        match event {
            Ok(Event::Exited { role }) => {
                self.running[role]
                    .as_mut()
                    .expect("only started roles exit")
                    .exited = true;
            }
            Ok(Event::Unready { role }) => {
                self.running[role]
                    .as_mut()
                    .expect("only started roles print")
                    .unready = true;
            }
            Ok(Event::Closed) => self.closed = true,
            _ => {}
        }
        // A cancellation kills every group, so it ends any wait with an exit.
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        match event {
            Ok(event) => Ok(Some(event)),
            // The session holds a sender, so the channel never disconnects: the wait ended at `until`.
            Err(_) if session => {
                self.timed_out = true;
                Err(invalid(format!(
                    "the session timed out after {} ms",
                    self.collection.plan.timeout_ms
                )))
            }
            Err(_) => Ok(None),
        }
    }

    /// Kill every group still running, in reverse start order, then reap every role, also in reverse start order, killing what remains of its group, and collect its output. Once this has completed, later calls do nothing; a call after one that panicked partway, as when the session is dropped, does what remains, reaping each role at most once.
    fn finish(&mut self) {
        if self.finished {
            return;
        }
        for running in self.running.iter().rev().flatten() {
            if !running.exited {
                process::kill_group(running.group);
            }
        }
        for running in self.running.iter_mut().flatten() {
            if !running.exited {
                process::wait_exit(running.group);
                running.exited = true;
            }
        }
        let cancellation = self.collection.cancellation;
        for (index, running) in self.running.iter_mut().enumerate().rev() {
            let Some(running) = running.as_mut().filter(|running| !running.reaped) else {
                continue;
            };
            // Marked first: a role whose reaping panicked is not reaped again, nor its group, which may then be another's, killed again.
            running.reaped = true;
            let status = process::reap(&mut running.child, running.group, cancellation);
            let entry = &mut self.entries[index];
            entry.exit_code = status.code();
            entry.signal = status.signal();
        }
        // A descendant that left a group survives cleanup and may hold its pipes open. Stop waiting for it rather than hold the lock indefinitely.
        let deadline = Instant::now() + OUTPUT_GRACE;
        for (index, running) in self.running.iter_mut().enumerate() {
            let Some(running) = running else { continue };
            let (mut stdout, mut stderr) = (None, None);
            while stdout.is_none() || stderr.is_none() {
                match running
                    .outputs
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(Output::Stdout(output)) => stdout = Some(output),
                    Ok(Output::Stderr(output)) => stderr = Some(output),
                    Err(_) => break,
                }
            }
            running.escaped = stdout.is_none() || stderr.is_none();
            let entry = &mut self.entries[index];
            entry.stderr = String::from_utf8_lossy(&stderr.unwrap_or_default()).into_owned();
            match self.roles[index].kind {
                RoleKind::Service => {
                    let (tail, _) = stdout.unwrap_or_default();
                    entry.stdout = String::from_utf8_lossy(&tail).into_owned();
                }
                RoleKind::Driver => running.stdout = stdout,
            }
        }
        self.finished = true;
    }

    /// After a session that ran to the end: no escaped descendants, a successful driver and successfully stopped services, and the driver's valid record.
    fn check(&mut self) -> io::Result<Option<ProcessRecord>> {
        if let Some(index) = self
            .running
            .iter()
            .position(|running| running.as_ref().is_some_and(|running| running.escaped))
        {
            return Err(self.failure(
                index,
                "left a descendant outside its process group that kept its output open; it was not killed and may still be running",
            ));
        }
        let driver = self.roles.len() - 1;
        let entry = &self.entries[driver];
        if entry.exit_code != Some(0) {
            return Err(self.failure(
                driver,
                &format!(
                    "failed with exit code {:?}, signal {:?}",
                    entry.exit_code, entry.signal
                ),
            ));
        }
        for index in 0..driver {
            let entry = &self.entries[index];
            let running = self.running[index]
                .as_ref()
                .expect("services started before the driver");
            let stopped = match running.stopping.expect("every service was stopped") {
                Stopping::Signal(signal) => {
                    entry.exit_code == Some(0) || entry.signal == Some(signal)
                }
                Stopping::KilledAfterGrace => true,
                Stopping::Exit => entry.exit_code == Some(0),
            };
            if !stopped {
                return Err(self.failure(
                    index,
                    &format!(
                        "did not stop successfully: exit code {:?}, signal {:?}",
                        entry.exit_code, entry.signal
                    ),
                ));
            }
        }
        match self.window {
            Window::Closed => {}
            Window::Before => return Err(self.failure(driver, "sent no window start marker")),
            Window::Open(_) => return Err(self.failure(driver, "sent no window stop marker")),
        }
        let (stdout, truncated) = self.running[driver]
            .as_mut()
            .expect("the driver started")
            .stdout
            .take()
            .expect("the driver's output was collected");
        Collection::record(&stdout, truncated)
            .map(Some)
            .map_err(|error| self.failure(driver, &error.to_string()))
    }

    /// An error naming the role and what it did.
    fn failure(&self, index: usize, message: &str) -> io::Error {
        let role = &self.roles[index];
        let kind = match role.kind {
            RoleKind::Service => "service",
            RoleKind::Driver => "driver",
        };
        invalid(format!("role {} ({kind}) {message}", role.name))
    }
}

impl Drop for Session<'_> {
    /// A session that unwinds from a panic still kills and reaps every role before the host lock, which outlives it, is released.
    fn drop(&mut self) {
        self.finish();
    }
}

/// How a role's executable is verified before the session.
enum Verification<'a> {
    Artifact(&'a crate::artifact::Artifact, PathBuf),
    External(&'a crate::plan::ExternalProgram),
}

fn command(program: &std::path::Path, arguments: &[String]) -> Vec<String> {
    std::iter::once(program.display().to_string())
        .chain(arguments.iter().cloned())
        .collect()
}

/// Read a service's stdout line by line until a line matches `pattern`, report it, and drain the rest, so that the service never blocks writing output after its ready line. Reports output that ends without a ready line as the service `role` being unready. Returns the end of the output before the ready line, for diagnostics.
fn read_ready(
    mut reader: impl Read,
    pattern: &Pattern,
    role: usize,
    events: &Sender<Event>,
) -> Vec<u8> {
    let mut tail = VecDeque::new();
    let mut line = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).unwrap_or(0);
        if count == 0 {
            keep(&mut tail, &line);
            let _ = events.send(Event::Unready { role });
            return tail.into();
        }
        let mut chunk = &buffer[..count];
        while let Some(newline) = chunk.iter().position(|byte| *byte == b'\n') {
            extend(&mut line, &chunk[..newline]);
            chunk = &chunk[newline + 1..];
            let text = String::from_utf8_lossy(&line);
            let text = text.strip_suffix('\r').unwrap_or(&text);
            if let Some(captures) = (line.len() <= LINE_LIMIT)
                .then(|| pattern.matches(text))
                .flatten()
            {
                let _ = events.send(Event::Ready {
                    line: text.to_owned(),
                    captures,
                });
                let _ = io::copy(&mut reader, &mut io::sink());
                return tail.into();
            }
            line.push(b'\n');
            keep(&mut tail, &line);
            line.clear();
        }
        extend(&mut line, chunk);
    }
}

/// Append to a line, keeping one byte beyond the limit so that an overlong line never matches.
fn extend(line: &mut Vec<u8>, bytes: &[u8]) {
    let room = (LINE_LIMIT + 1).saturating_sub(line.len());
    line.extend_from_slice(&bytes[..bytes.len().min(room)]);
}

/// Append to the retained end of the output, in time proportional to the bytes appended once the limit is reached.
fn keep(tail: &mut VecDeque<u8>, bytes: &[u8]) {
    tail.extend(bytes);
    if tail.len() > STDERR_LIMIT {
        tail.drain(..tail.len() - STDERR_LIMIT);
    }
}

/// The metrics the runner records for each service over the window.
const WINDOW_CPU_METRICS: [&str; 3] = ["cpu_time", "user_cpu_time", "system_cpu_time"];

/// A service's CPU time between its snapshots at the window's start and stop, or unavailable metrics when either could not be read. Total CPU time is precise to the nanosecond; the user and system split is the kernel's, in clock ticks of `1 / ticks_per_second`.
fn window_metrics(
    start: Option<CpuTime>,
    stop: Option<CpuTime>,
    ticks_per_second: u64,
) -> Vec<Metric> {
    let window = |metric: Metric| metric.with_phase(Phase::Window).with_source(Source::Rusage);
    match start.zip(stop) {
        Some((start, stop)) => {
            let tick = 1_000_000_000 / ticks_per_second;
            let ticks = |before: u64, after: u64| after.saturating_sub(before) * tick;
            vec![
                window(Metric::total(
                    WINDOW_CPU_METRICS[0],
                    Unit::Nanoseconds,
                    stop.total_ns.saturating_sub(start.total_ns),
                )),
                window(
                    Metric::total(
                        WINDOW_CPU_METRICS[1],
                        Unit::Nanoseconds,
                        ticks(start.user_ticks, stop.user_ticks),
                    )
                    .with_resolution(tick),
                ),
                window(
                    Metric::total(
                        WINDOW_CPU_METRICS[2],
                        Unit::Nanoseconds,
                        ticks(start.system_ticks, stop.system_ticks),
                    )
                    .with_resolution(tick),
                ),
            ]
        }
        None => WINDOW_CPU_METRICS
            .iter()
            .map(|name| {
                window(Metric::unavailable(
                    *name,
                    Unit::Nanoseconds,
                    "the service's CPU time could not be read at a window marker",
                ))
            })
            .collect(),
    }
}

/// Read the driver's stdout: report each line that begins `base-benchmark window` as a marker, keep the rest, at most [`STDOUT_LIMIT`] bytes, as its record, and report when it closes. Markers are whole lines before the record, so one after output of the record, or one that ends the output without a newline, is reported as misplaced. Returns the record and whether it was truncated.
fn read_driver(mut reader: impl Read, events: &Sender<Event>) -> (Vec<u8>, bool) {
    let mut record = Vec::new();
    let mut truncated = false;
    let mut line = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).unwrap_or(0);
        let mut chunk = &buffer[..count];
        while let Some(newline) = chunk.iter().position(|byte| *byte == b'\n') {
            bounded(&mut line, &chunk[..=newline], &mut truncated);
            chunk = &chunk[newline + 1..];
            match marker(&line) {
                Some(marker) if record.iter().all(u8::is_ascii_whitespace) => {
                    let _ = events.send(Event::Marker(marker));
                }
                Some(_) => {
                    let _ = events.send(Event::Marker(Marker::Late(text(&line))));
                }
                None => bounded(&mut record, &line, &mut truncated),
            }
            line.clear();
        }
        bounded(&mut line, chunk, &mut truncated);
        if count == 0 {
            match marker(&line) {
                Some(_) => {
                    let _ = events.send(Event::Marker(Marker::Unterminated(text(&line))));
                }
                None => bounded(&mut record, &line, &mut truncated),
            }
            let _ = events.send(Event::Closed);
            return (record, truncated);
        }
    }
}

/// Append to `buffer`, keeping at most [`STDOUT_LIMIT`] bytes.
fn bounded(buffer: &mut Vec<u8>, bytes: &[u8], truncated: &mut bool) {
    let room = STDOUT_LIMIT.saturating_sub(buffer.len());
    *truncated |= bytes.len() > room;
    buffer.extend_from_slice(&bytes[..bytes.len().min(room)]);
}

/// A line of the driver's stdout without its line ending.
fn text(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}

/// The marker a line of the driver's stdout is, if it begins like one.
fn marker(line: &[u8]) -> Option<Marker> {
    let line = text(line);
    match line.as_str() {
        WINDOW_START => Some(Marker::Start),
        WINDOW_STOP => Some(Marker::Stop),
        _ if line.starts_with("base-benchmark window") => Some(Marker::Invalid(line)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
