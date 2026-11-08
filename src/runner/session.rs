// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! One session of a multi-process scenario: every role of one sample.
//!
//! Every executable is verified first. Services start in order, each waited on until it prints its ready line, whose captured values later roles' arguments receive; then the driver runs to completion; then the services are stopped in reverse order. One deadline covers the whole session. On any failure, the deadline, or cancellation, every role's process group still running is killed, in reverse start order, and every role is reaped before the session returns, so before the host lock is released. Helper threads only read output and wait for exits, and report them to the session as events, so the session can always react to the deadline.

use super::process::{
    self, Launch, OUTPUT_GRACE, STDERR_LIMIT, STDOUT_LIMIT, read_head, read_tail,
};
use super::{Collection, RoleEntry, SampleEntry, cancelled};
use crate::plan::{PlannedRole, Subject};
use crate::record::invalid;
use crate::role::{Pattern, RoleKind, RoleProgram, Stop, substitute};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Stdio};
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
    /// The role's process exited; it is not reaped yet.
    Exited { role: usize },
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
    stopping: Option<Stopping>,
    stdout: Option<(Vec<u8>, bool)>,
    /// A descendant that left the group kept its output open after it exited.
    escaped: bool,
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
}

/// Run one session of `subject`, recording every role in `entry`, and the driver's record when it succeeds.
pub(super) fn collect(
    collection: &Collection,
    subject: &Subject,
    stack_padding: &[Option<usize>],
    entry: &mut SampleEntry,
) -> io::Result<()> {
    let plan = collection.plan;
    let started = Instant::now();
    let (sender, events) = mpsc::channel();
    let mut session = Session {
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
    };
    let identified = session.identify(subject, stack_padding);
    let driven = identified.and_then(|()| session.drive());
    session.finish();
    let result = driven.and_then(|()| session.check());
    entry.duration_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    entry.timed_out = session.timed_out;
    entry.roles = session.entries;
    entry.record = result?;
    Ok(())
}

impl Session<'_> {
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
        while !self.running[driver]
            .as_ref()
            .expect("the driver started")
            .exited
        {
            self.receive(self.deadline)?;
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
        let arguments: Vec<String> = self.arguments[index]
            .iter()
            .map(|argument| substitute(argument, &self.captured))
            .collect();
        self.entries[index].command = command(&self.programs[index], &arguments);
        let environment = collection.environment(self.entries[index].stack_padding);
        let launch = Launch {
            program: &self.programs[index],
            arguments: &arguments,
            environment: &environment,
            directory: &collection.plan.working_directory,
            cpus: &role.cpus,
        };
        let (mut child, group) = process::start(&launch, Stdio::null(), collection.cancellation)
            .map_err(|error| self.failure(index, &format!("could not start: {error}")))?;
        let started = Instant::now();
        self.entries[index].started = true;
        let (outputs, received) = mpsc::channel();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let stdout_sender = outputs.clone();
        match &role.ready {
            Some(ready) => {
                let pattern =
                    Pattern::parse(&ready.pattern).expect("scenarios have valid ready patterns");
                let events = self.sender.clone();
                std::thread::spawn(move || {
                    let tail = read_ready(stdout, &pattern, &events);
                    stdout_sender.send(Output::Stdout((tail, false)))
                });
            }
            None => {
                std::thread::spawn(move || {
                    stdout_sender.send(Output::Stdout(read_head(stdout, STDOUT_LIMIT)))
                });
            }
        }
        std::thread::spawn(move || outputs.send(Output::Stderr(read_tail(stderr, STDERR_LIMIT))));
        let events = self.sender.clone();
        std::thread::spawn(move || {
            process::wait_exit(group);
            events.send(Event::Exited { role: index })
        });
        self.running[index] = Some(Running {
            child,
            group,
            started,
            outputs: received,
            exited: false,
            stopping: None,
            stdout: None,
            escaped: false,
        });
        Ok(())
    }

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
        match self.receive(by.min(self.deadline))? {
            None => Err(self.failure(
                index,
                &format!(
                    "printed no line matching {:?} within {} ms",
                    ready.pattern, ready.timeout_ms
                ),
            )),
            // Earlier services are ready already, so only this one can become ready.
            Some(Event::Ready { line, captures }) => {
                self.entries[index].ready_line = Some(line);
                self.entries[index].captures = captures.clone();
                self.captured
                    .insert(self.roles[index].name.clone(), captures);
                Ok(())
            }
            Some(Event::Exited { role }) if role == index => {
                Err(self.failure(index, "exited before it was ready"))
            }
            Some(Event::Exited { role }) => Err(self.exited_early(role)),
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
        if let Ok(Event::Exited { role }) = event {
            self.running[role]
                .as_mut()
                .expect("only started roles exit")
                .exited = true;
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

    /// Kill every group still running, in reverse start order, then reap every role and collect its output.
    fn finish(&mut self) {
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
        for (index, running) in self.running.iter_mut().enumerate() {
            let Some(running) = running else { continue };
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
    }

    /// After a session that ran to the end: no escaped descendants, a successful driver and successfully stopped services, and the driver's valid record.
    fn check(&mut self) -> io::Result<Option<crate::record::ProcessRecord>> {
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

/// Read a service's stdout line by line until a line matches `pattern`, report it, and drain the rest. Returns the end of the output before the ready line, for diagnostics.
fn read_ready(mut reader: impl Read, pattern: &Pattern, events: &Sender<Event>) -> Vec<u8> {
    let mut tail = Vec::new();
    let mut line = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).unwrap_or(0);
        if count == 0 {
            keep(&mut tail, &line);
            return tail;
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
                return tail;
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

/// Append to the retained end of the output.
fn keep(tail: &mut Vec<u8>, bytes: &[u8]) {
    tail.extend_from_slice(bytes);
    if tail.len() > STDERR_LIMIT {
        tail.drain(..tail.len() - STDERR_LIMIT);
    }
}

#[cfg(test)]
mod tests;
