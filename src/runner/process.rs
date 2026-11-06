// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use nix::errno::Errno;
use nix::sched::{CpuSet, sched_getaffinity, sched_setaffinity};
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{Id, WaitPidFlag, waitid};
use nix::unistd::Pid;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Workload stdout beyond this size is a malformed record.
pub(crate) const STDOUT_LIMIT: usize = 16 * 1024 * 1024;
/// Only the end of stderr is retained as diagnostic evidence.
pub(crate) const STDERR_LIMIT: usize = 64 * 1024;

/// Stops a running campaign: the active workload process group is killed and no further samples start.
///
/// Every process the runner starts leads its own process group, so wrapper descendants such as a `perf` child are killed with it. The lock is released only after that cleanup.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<CancellationState>);

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    active: Mutex<Option<Pid>>,
}

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
    pub fn on_termination_signals(&self) -> SignalGuard {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM, SIGHUP])
            .expect("termination signals are not forbidden signals");
        let handle = signals.handle();
        let cancellation = self.clone();
        let thread = std::thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                eprintln!("base-benchmark: received signal {signal}; stopping the campaign");
                cancellation.cancel();
            }
        });
        SignalGuard {
            handle,
            thread: Some(thread),
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

/// Restores signal delivery to the rest of the process when dropped.
pub struct SignalGuard {
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        self.handle.close();
        let _ = self.thread.take().map(std::thread::JoinHandle::join);
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
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr: Vec<u8>,
    pub duration_ns: u64,
}

impl Execution {
    pub fn succeeded(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.cancelled
    }
}

/// Run one workload process to completion with bounded output, a deadline, CPU placement, and process-group cleanup.
pub(crate) fn execute(
    program: &Path,
    arguments: &[String],
    cpus: &[usize],
    timeout: Duration,
    cancellation: &Cancellation,
) -> io::Result<Execution> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let started = Instant::now();
    let mut child = with_affinity(cpus, || command.spawn())
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", program.display())))?;
    let group = Pid::from_raw(i32::try_from(child.id()).expect("process IDs fit in pid_t"));
    cancellation.register(group);

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let stdout = std::thread::spawn(move || read_head(stdout, STDOUT_LIMIT));
    let stderr = std::thread::spawn(move || read_tail(stderr, STDERR_LIMIT));

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
    let (stdout, stdout_truncated) = stdout.join().expect("the stdout reader does not panic");
    let stderr = stderr.join().expect("the stderr reader does not panic");
    Ok(Execution {
        code: status.code(),
        signal: status.signal(),
        timed_out: timed_out.load(Ordering::SeqCst),
        cancelled: cancellation.is_cancelled(),
        stdout,
        stdout_truncated,
        stderr,
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
