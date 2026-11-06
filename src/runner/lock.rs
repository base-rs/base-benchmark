// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::Cancellation;
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

/// How often a waiting campaign rechecks the lock and its cancellation.
const RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// A cooperative, host-wide exclusive lock held for a whole campaign. It is released when dropped, or by the kernel if the process dies.
pub(crate) struct HostLock {
    _lock: Flock<File>,
}

impl HostLock {
    /// Acquire the lock, reporting once when another campaign holds it. Waiting stops if the campaign is cancelled.
    pub fn acquire(
        path: &Path,
        cancellation: &Cancellation,
        progress: &mut dyn Write,
    ) -> io::Result<Self> {
        Self::acquire_with(path, cancellation, progress, |file| {
            Flock::lock(file, FlockArg::LockExclusiveNonblock)
        })
    }

    /// `try_lock` is the non-blocking lock attempt, replaceable so tests can inject lock failures such as `ENOLCK`.
    fn acquire_with(
        path: &Path,
        cancellation: &Cancellation,
        progress: &mut dyn Write,
        mut try_lock: impl FnMut(File) -> Result<Flock<File>, (File, Errno)>,
    ) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|error| {
                io::Error::new(error.kind(), format!("{}: {error}", path.display()))
            })?;
        let mut reported = false;
        loop {
            match try_lock(file) {
                Ok(lock) => return Ok(Self { _lock: lock }),
                Err((unlocked, Errno::EWOULDBLOCK)) => file = unlocked,
                Err((_, error)) => {
                    return Err(io::Error::new(
                        io::Error::from(error).kind(),
                        format!("{}: {error}", path.display()),
                    ));
                }
            }
            if cancellation.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "cancelled while waiting for the host lock",
                ));
            }
            if !reported {
                let _ = writeln!(
                    progress,
                    "base-benchmark: waiting for host lock {}",
                    path.display()
                );
                reported = true;
            }
            std::thread::sleep(RETRY_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests;
