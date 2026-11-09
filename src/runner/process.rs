// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Workload processes. Collecting samples needs Linux: process groups, `waitid` without reaping, and CPU affinity. Elsewhere [`execute`] reports that it is unsupported, and everything else here builds.

#[cfg(target_os = "linux")]
use nix::errno::Errno;
#[cfg(target_os = "linux")]
use nix::sched::{CpuSet, sched_getaffinity, sched_setaffinity};
#[cfg(target_os = "linux")]
use nix::sys::signal::{Signal, killpg};
#[cfg(target_os = "linux")]
use nix::sys::wait::{Id, WaitPidFlag, waitid};
#[cfg(target_os = "linux")]
use nix::unistd::Pid;
use std::collections::BTreeMap;
use std::io;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

/// Workload stdout beyond this size is a malformed record.
#[cfg(target_os = "linux")]
pub(crate) const STDOUT_LIMIT: usize = 16 * 1024 * 1024;
/// Only the end of stderr is retained as diagnostic evidence.
#[cfg(target_os = "linux")]
pub(crate) const STDERR_LIMIT: usize = 64 * 1024;
/// How long to wait for output after the workload and its process group are gone.
#[cfg(target_os = "linux")]
pub(crate) const OUTPUT_GRACE: Duration = Duration::from_secs(2);
/// Attempts to start a program whose file is briefly open for writing elsewhere, such as a just-staged copy.
#[cfg(target_os = "linux")]
const BUSY_ATTEMPTS: usize = 50;

#[cfg(target_os = "linux")]
enum Output {
    Stdout((Vec<u8>, bool)),
    Stderr(Vec<u8>),
}

/// Spawn, retrying `ETXTBSY`: another thread may have forked while the executable was still open for writing, and that descriptor closes when its child execs.
#[cfg(target_os = "linux")]
fn spawn(command: &mut Command) -> io::Result<std::process::Child> {
    let mut attempts = 1;
    loop {
        match command.spawn() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempts < BUSY_ATTEMPTS =>
            {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

/// Stops a running campaign: the active workload process groups are killed, in reverse start order, and no further samples start.
///
/// Every process the runner starts leads its own process group, so wrapper descendants such as a `perf` child are killed with it. The lock is released only after that cleanup. A cancellation tracks the processes of one sample at a time: one workload, or every role of a session.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<CancellationState>);

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    /// The running workload process groups, in start order.
    active: Mutex<Vec<i32>>,
}

/// Cancellations guarded against termination signals, shared with the process-wide handler thread.
static GUARDED: Mutex<Vec<Cancellation>> = Mutex::new(Vec::new());
#[cfg(unix)]
static HANDLER: Once = Once::new();

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        let active = self
            .0
            .active
            .lock()
            .expect("cancellation state is never poisoned");
        active.iter().rev().copied().for_each(kill_group);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Cancel when the process receives SIGINT, SIGTERM, or SIGHUP, until the guard is dropped.
    ///
    /// The first signal cancels every guarded campaign, so workloads are killed before locks are released. A signal while nothing is guarded, or a second signal, exits the process with status 128 plus the signal number. The handler stays installed for the life of the process. On Linux, signals already ignored when it is installed, as under `nohup`, stay ignored. Signals blocked when it is installed, as a launcher's signal mask passes to what it starts, are handled all the same: the handler's own thread, which lives as long as the process, unblocks them, so a signal sent to the process reaches it, though a signal sent to a thread that blocks it, as `raise` does, waits for that thread. Systems other than Unix install no handler.
    pub fn on_termination_signals(&self) -> SignalGuard {
        #[cfg(unix)]
        HANDLER.call_once(|| {
            use nix::sys::signal::{SigSet, Signal};
            let ignored =
                ignored_signals(std::fs::read_to_string("/proc/self/status").ok().as_deref());
            let handled: SigSet = [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP]
                .into_iter()
                .filter(|signal| ignored & (1 << (*signal as i32 - 1)) == 0)
                .collect();
            let mut signals =
                signal_hook::iterator::Signals::new(handled.iter().map(|signal| signal as i32))
                    .expect("termination signals are not forbidden signals");
            std::thread::spawn(move || {
                handled
                    .thread_unblock()
                    .expect("termination signals can be unblocked");
                for signal in signals.forever() {
                    respond(signal);
                }
            });
        });
        guarded().push(self.clone());
        SignalGuard {
            cancellation: self.clone(),
        }
    }

    #[cfg(target_os = "linux")]
    fn register(&self, group: i32) {
        let mut active = self
            .0
            .active
            .lock()
            .expect("cancellation state is never poisoned");
        active.push(group);
        if self.is_cancelled() {
            kill_group(group);
        }
    }

    /// Kill remaining group members, such as daemonized descendants, while the leader is still unreaped and its process ID cannot be reused.
    #[cfg(target_os = "linux")]
    fn finish(&self, group: i32) {
        let mut active = self
            .0
            .active
            .lock()
            .expect("cancellation state is never poisoned");
        kill_group(group);
        active.retain(|active| *active != group);
    }
}

/// The mask of signals ignored by this process, from the `SigIgn` line of `/proc/self/status`, or none where that is unavailable.
#[cfg(unix)]
fn ignored_signals(status: Option<&str>) -> u64 {
    status
        .and_then(|status| status.lines().find_map(|line| line.strip_prefix("SigIgn:")))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        .unwrap_or(0)
}

fn guarded() -> std::sync::MutexGuard<'static, Vec<Cancellation>> {
    GUARDED
        .lock()
        .expect("the signal registry is never poisoned")
}

/// Cancel guarded campaigns on the first signal; otherwise exit as the default action would.
#[cfg(unix)]
fn respond(signal: i32) {
    let guarded = guarded();
    if guarded
        .iter()
        .any(|cancellation| !cancellation.is_cancelled())
    {
        eprintln!(
            "base-benchmark: received signal {signal}; stopping the campaign (signal again to exit immediately)"
        );
        guarded.iter().for_each(Cancellation::cancel);
    } else {
        eprintln!("base-benchmark: received signal {signal}; exiting");
        std::process::exit(128 + signal);
    }
}

/// Stops guarding a campaign. The process-wide handler remains, and exits on the next signal if nothing else is guarded.
pub struct SignalGuard {
    cancellation: Cancellation,
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        // Remove one registration, so another guard of the same cancellation still holds.
        let mut guarded = guarded();
        let index = guarded
            .iter()
            .position(|cancellation| Arc::ptr_eq(&cancellation.0, &self.cancellation.0))
            .expect("a guard's cancellation is registered until it drops");
        guarded.remove(index);
    }
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    /// The groups this thread has killed, in order, so that tests can check the order of kills, which leave no trace a killed process could record.
    pub(crate) static KILLED: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(target_os = "linux")]
pub(crate) fn kill_group(group: i32) {
    #[cfg(test)]
    KILLED.with(|killed| killed.borrow_mut().push(group));
    // An empty group (ESRCH) is the normal case after a clean exit.
    let _ = killpg(Pid::from_raw(group), Signal::SIGKILL);
}

// Only Linux starts workloads, so elsewhere no group is ever registered to kill.
#[cfg(not(target_os = "linux"))]
fn kill_group(_group: i32) {}

/// Send `signal`, by name, to a process group. A group that is already empty is not an error.
#[cfg(target_os = "linux")]
pub(crate) fn signal_group(group: i32, signal: &str) {
    let signal: Signal = signal
        .parse()
        .expect("plans name only supported stop signals");
    let _ = killpg(Pid::from_raw(group), signal);
}

/// The number of a signal, by name, on this system.
#[cfg(target_os = "linux")]
pub(crate) fn signal_number(signal: &str) -> i32 {
    signal
        .parse::<Signal>()
        .expect("plans name only supported stop signals") as i32
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Execution {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    /// A descendant outside the process group kept the output open after the workload exited.
    pub escaped: bool,
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr: Vec<u8>,
    pub duration_ns: u64,
}

impl Execution {
    pub fn succeeded(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.cancelled && !self.escaped
    }
}

/// What to start: the environment replaces the collector's own completely.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Launch<'a> {
    pub program: &'a Path,
    pub arguments: &'a [String],
    pub environment: &'a BTreeMap<String, String>,
    pub directory: &'a Path,
    pub cpus: &'a [usize],
}

/// Collecting samples needs Linux, so elsewhere campaigns fail with this before creating their directory or building anything.
#[cfg(not(target_os = "linux"))]
pub(crate) fn check_collection() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "collecting samples needs Linux, for process groups, waitid, and CPU affinity",
    ))
}

/// Collecting samples needs Linux; see the module documentation. Campaigns check first, so this is not reached.
#[cfg(not(target_os = "linux"))]
pub(crate) fn execute(
    _launch: &Launch,
    _timeout: Duration,
    _cancellation: &Cancellation,
) -> io::Result<Execution> {
    check_collection().map(|()| unreachable!("collection is unsupported here"))
}

/// Start a process leading its own group, with the launch's environment, directory, and CPUs, piped stdout and stderr, `stdin`, and every stop signal at its default disposition and unblocked, and register its group with `cancellation`. Returns the child and its group.
///
/// Signals this process ignores, such as `SIGINT` and `SIGQUIT` after `&` in a non-interactive shell or `SIGHUP` under `nohup`, would otherwise stay ignored in the child, and signals blocked in the spawning thread, as a launcher's signal mask passes to what it starts, would stay blocked, so that a service would outlast its stop signal and be killed after its grace period. The stop signals include the termination signals.
#[cfg(target_os = "linux")]
pub(crate) fn start(
    launch: &Launch,
    stdin: Stdio,
    cancellation: &Cancellation,
) -> io::Result<(std::process::Child, i32)> {
    let (program, cpus) = (launch.program, launch.cpus);
    let mut command = Command::new(program);
    command
        .args(launch.arguments)
        .env_clear()
        .envs(launch.environment)
        .current_dir(launch.directory)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // SAFETY: `reset_stop_signals` is async-signal-safe, so it can run between fork and exec.
    unsafe {
        command.pre_exec(reset_stop_signals);
    }
    let child = with_affinity(cpus, || spawn(&mut command))
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", program.display())))?;
    let group = i32::try_from(child.id()).expect("process IDs fit in pid_t");
    cancellation.register(group);
    Ok((child, group))
}

/// The signals a service may be stopped with, [`crate::role::STOP_SIGNALS`], which include the termination signals.
#[cfg(target_os = "linux")]
const STOP_SIGNALS: [Signal; 6] = [
    Signal::SIGTERM,
    Signal::SIGINT,
    Signal::SIGHUP,
    Signal::SIGQUIT,
    Signal::SIGUSR1,
    Signal::SIGUSR2,
];

/// Reset every stop signal to its default disposition and unblock it, leaving other signals' dispositions and the rest of the signal mask as inherited. It runs between fork and exec, so it only calls `signal`, `sigemptyset`, `sigaddset`, and `pthread_sigmask`, which are async-signal-safe, and allocates nothing. Resetting a signal other than `SIGKILL` or `SIGSTOP`, and unblocking a valid signal, cannot fail.
#[cfg(target_os = "linux")]
fn reset_stop_signals() -> io::Result<()> {
    use nix::sys::signal::{SigHandler, SigSet, signal};
    let mut stopping = SigSet::empty();
    for stop in STOP_SIGNALS {
        // SAFETY: the default disposition runs no handler.
        let _ = unsafe { signal(stop, SigHandler::SigDfl) };
        stopping.add(stop);
    }
    let _ = stopping.thread_unblock();
    Ok(())
}

/// Wait for a group's leader to exit without reaping it, so its process ID, and therefore the group ID, stays reserved during cleanup.
#[cfg(target_os = "linux")]
pub(crate) fn wait_exit(group: i32) {
    let _ = std::iter::repeat_with(|| {
        waitid(
            Id::Pid(Pid::from_raw(group)),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT,
        )
    })
    .find(|result| *result != Err(Errno::EINTR));
}

/// Whether a group's leader has exited, without reaping it.
#[cfg(target_os = "linux")]
pub(crate) fn has_exited(group: i32) -> bool {
    use nix::sys::wait::WaitStatus;
    let status = std::iter::repeat_with(|| {
        waitid(
            Id::Pid(Pid::from_raw(group)),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT | WaitPidFlag::WNOHANG,
        )
    })
    .find(|result| *result != Err(Errno::EINTR));
    status != Some(Ok(WaitStatus::StillAlive))
}

/// Kill what remains of an exited leader's group, then reap the leader.
#[cfg(target_os = "linux")]
pub(crate) fn reap(
    child: &mut std::process::Child,
    group: i32,
    cancellation: &Cancellation,
) -> std::process::ExitStatus {
    cancellation.finish(group);
    // After `waitid` reported the exit, reaping our own child cannot fail.
    child.wait().expect("an exited child can be reaped")
}

/// A started workload, killed and reaped when dropped before [`Unreaped::reap`] has been called, as when [`execute`] unwinds from a panic or a thread that fails to spawn, so that it never outlives the host lock, which outlives it.
#[cfg(target_os = "linux")]
struct Unreaped<'a> {
    child: std::process::Child,
    group: i32,
    cancellation: &'a Cancellation,
    reaped: bool,
}

#[cfg(target_os = "linux")]
impl Unreaped<'_> {
    /// Kill what remains of the exited leader's group, then reap the leader.
    fn reap(&mut self) -> std::process::ExitStatus {
        // Marked first: a workload whose reaping panicked is not reaped again, nor its group, which may then be another's, killed again.
        self.reaped = true;
        reap(&mut self.child, self.group, self.cancellation)
    }
}

#[cfg(target_os = "linux")]
impl Drop for Unreaped<'_> {
    fn drop(&mut self) {
        if !self.reaped {
            // The leader is still unreaped, so its group is still this workload's.
            self.cancellation.finish(self.group);
            let _ = self.child.wait();
        }
    }
}

/// Run one workload process to completion with bounded output, a deadline, CPU placement, and process-group cleanup.
#[cfg(target_os = "linux")]
pub(crate) fn execute(
    launch: &Launch,
    timeout: Duration,
    cancellation: &Cancellation,
) -> io::Result<Execution> {
    let started = Instant::now();
    let (child, group) = start(launch, Stdio::null(), cancellation)?;
    let mut workload = Unreaped {
        child,
        group,
        cancellation,
        reaped: false,
    };

    let (sender, outputs) = mpsc::channel();
    let stdout = workload.child.stdout.take().expect("stdout is piped");
    let stderr = workload.child.stderr.take().expect("stderr is piped");
    let stdout_sender = sender.clone();
    std::thread::spawn(move || stdout_sender.send(Output::Stdout(read_head(stdout, STDOUT_LIMIT))));
    std::thread::spawn(move || sender.send(Output::Stderr(read_tail(stderr, STDERR_LIMIT))));

    let timed_out = Arc::new(AtomicBool::new(false));
    let (done, finished) = mpsc::channel::<()>();
    let watchdog = {
        let timed_out = timed_out.clone();
        std::thread::spawn(move || {
            if finished.recv_timeout(timeout) == Err(RecvTimeoutError::Timeout) {
                timed_out.store(true, Ordering::SeqCst);
                kill_group(group);
            }
        })
    };

    wait_exit(group);
    drop(done);
    watchdog.join().expect("the watchdog does not panic");
    let status = workload.reap();
    // A descendant that left the group survives cleanup and may hold the pipes open. Stop waiting for it rather than hold the lock indefinitely.
    let deadline = Instant::now() + OUTPUT_GRACE;
    let (mut stdout, mut stderr) = (None, None);
    while stdout.is_none() || stderr.is_none() {
        match outputs.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Output::Stdout(output)) => stdout = Some(output),
            Ok(Output::Stderr(output)) => stderr = Some(output),
            Err(_) => break,
        }
    }
    let escaped = stdout.is_none() || stderr.is_none();
    let (stdout, stdout_truncated) = stdout.unwrap_or_default();
    Ok(Execution {
        code: status.code(),
        signal: status.signal(),
        timed_out: timed_out.load(Ordering::SeqCst),
        cancelled: cancellation.is_cancelled(),
        escaped,
        stdout,
        stdout_truncated,
        stderr: stderr.unwrap_or_default(),
        duration_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
    })
}

/// Keep at most `limit` bytes, then drain the rest so the writer never blocks.
///
/// A read error ends the stream; the record parser then rejects incomplete output.
#[cfg(target_os = "linux")]
pub(crate) fn read_head(mut reader: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut head = Vec::new();
    let _ = (&mut reader).take(limit as u64 + 1).read_to_end(&mut head);
    let truncated = head.len() > limit;
    head.truncate(limit);
    let _ = io::copy(&mut reader, &mut io::sink());
    (head, truncated)
}

/// Keep the last `limit` bytes.
#[cfg(target_os = "linux")]
pub(crate) fn read_tail(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut tail = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).unwrap_or(0);
        if count == 0 {
            return tail;
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > limit {
            tail.drain(..tail.len() - limit);
        }
    }
}

/// Spawn with the calling thread temporarily pinned, so the child inherits exactly `cpus` from its first instruction.
#[cfg(target_os = "linux")]
fn with_affinity<T>(cpus: &[usize], spawn: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    if cpus.is_empty() {
        return spawn();
    }
    let this_thread = Pid::from_raw(0);
    let original =
        sched_getaffinity(this_thread).expect("the calling thread can read its own affinity");
    let mut selected = CpuSet::new();
    for &cpu in cpus {
        selected.set(cpu).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("CPU {cpu} exceeds the supported CPU set size"),
            )
        })?;
    }
    sched_setaffinity(this_thread, &selected).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot place workloads on CPUs {cpus:?}: {error}"),
        )
    })?;
    let result = spawn();
    sched_setaffinity(this_thread, &original)
        .expect("the calling thread can restore its previous affinity");
    result
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
