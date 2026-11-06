// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use nix::errno::Errno;
use nix::sched::{CpuSet, sched_getaffinity, sched_setaffinity};
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{Id, WaitPidFlag, waitid};
use nix::unistd::Pid;
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

/// Workload stdout beyond this size is a malformed record.
pub(crate) const STDOUT_LIMIT: usize = 16 * 1024 * 1024;
/// Only the end of stderr is retained as diagnostic evidence.
pub(crate) const STDERR_LIMIT: usize = 64 * 1024;
/// How long to wait for output after the workload and its process group are gone.
pub(crate) const OUTPUT_GRACE: Duration = Duration::from_secs(2);
/// Attempts to start a program whose file is briefly open for writing elsewhere, such as a just-staged copy.
const BUSY_ATTEMPTS: usize = 50;

enum Output {
    Stdout((Vec<u8>, bool)),
    Stderr(Vec<u8>),
}

/// Spawn, retrying `ETXTBSY`: another thread may have forked while the executable was still open for writing, and that descriptor closes when its child execs.
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

/// Stops a running campaign: the active workload process group is killed and no further samples start.
///
/// Every process the runner starts leads its own process group, so wrapper descendants such as a `perf` child are killed with it. The lock is released only after that cleanup. A cancellation tracks one workload process at a time.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<CancellationState>);

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    active: Mutex<Option<Pid>>,
}

/// Cancellations guarded against termination signals, shared with the process-wide handler thread.
static GUARDED: Mutex<Vec<Cancellation>> = Mutex::new(Vec::new());
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
        if let Some(group) = *active {
            kill_group(group);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Cancel when the process receives SIGINT, SIGTERM, or SIGHUP, until the guard is dropped.
    ///
    /// The first signal cancels every guarded campaign, so workloads are killed before locks are released. A signal while nothing is guarded, or a second signal, exits the process with status 128 plus the signal number. The handler stays installed for the life of the process. On Linux, signals already ignored when it is installed, as under `nohup`, stay ignored.
    pub fn on_termination_signals(&self) -> SignalGuard {
        HANDLER.call_once(|| {
            use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
            let ignored =
                ignored_signals(std::fs::read_to_string("/proc/self/status").ok().as_deref());
            let handled = [SIGINT, SIGTERM, SIGHUP]
                .into_iter()
                .filter(|signal| ignored & (1 << (signal - 1)) == 0);
            let mut signals = signal_hook::iterator::Signals::new(handled)
                .expect("termination signals are not forbidden signals");
            std::thread::spawn(move || {
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

    fn register(&self, group: Pid) {
        let mut active = self
            .0
            .active
            .lock()
            .expect("cancellation state is never poisoned");
        *active = Some(group);
        if self.is_cancelled() {
            kill_group(group);
        }
    }

    /// Kill remaining group members, such as daemonized descendants, while the leader is still unreaped and its process ID cannot be reused.
    fn finish(&self, group: Pid) {
        let mut active = self
            .0
            .active
            .lock()
            .expect("cancellation state is never poisoned");
        kill_group(group);
        *active = None;
    }
}

/// The mask of signals ignored by this process, from the `SigIgn` line of `/proc/self/status`, or none where that is unavailable.
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

fn kill_group(group: Pid) {
    // An empty group (ESRCH) is the normal case after a clean exit.
    let _ = killpg(group, Signal::SIGKILL);
}

#[derive(Debug)]
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
pub(crate) struct Launch<'a> {
    pub program: &'a Path,
    pub arguments: &'a [String],
    pub environment: &'a BTreeMap<String, String>,
    pub directory: &'a Path,
    pub cpus: &'a [usize],
}

/// Run one workload process to completion with bounded output, a deadline, CPU placement, and process-group cleanup.
pub(crate) fn execute(
    launch: &Launch,
    timeout: Duration,
    cancellation: &Cancellation,
) -> io::Result<Execution> {
    let (program, cpus) = (launch.program, launch.cpus);
    let mut command = Command::new(program);
    command
        .args(launch.arguments)
        .env_clear()
        .envs(launch.environment)
        .current_dir(launch.directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let started = Instant::now();
    let mut child = with_affinity(cpus, || spawn(&mut command))
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", program.display())))?;
    let group = Pid::from_raw(i32::try_from(child.id()).expect("process IDs fit in pid_t"));
    cancellation.register(group);

    let (sender, outputs) = mpsc::channel();
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
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

    // Wait for the leader to exit without reaping it, so its process ID, and therefore the group ID, stays reserved during cleanup.
    let _ = std::iter::repeat_with(|| {
        waitid(Id::Pid(group), WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT)
    })
    .find(|result| *result != Err(Errno::EINTR));
    drop(done);
    watchdog.join().expect("the watchdog does not panic");
    cancellation.finish(group);
    // After `waitid` reported the exit, reaping our own child cannot fail.
    let status = child.wait().expect("an exited child can be reaped");
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
fn read_head(mut reader: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut head = Vec::new();
    let _ = (&mut reader).take(limit as u64 + 1).read_to_end(&mut head);
    let truncated = head.len() > limit;
    head.truncate(limit);
    let _ = io::copy(&mut reader, &mut io::sink());
    (head, truncated)
}

/// Keep the last `limit` bytes.
fn read_tail(mut reader: impl Read, limit: usize) -> Vec<u8> {
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

#[cfg(test)]
mod tests;
