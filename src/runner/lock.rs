// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::Cancellation;
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use std::fs::{File, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
        let mut file = open(path).map_err(|error| {
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

/// Open or create the lock file read-only, since `flock` needs no write access and a sticky `/tmp` may forbid opening another user's file for writing.
///
/// The path is shared with every user, so it is never followed as a symbolic link. A created lock is made usable by every user despite the umask, but only when it has a single link: a planted hard link to someone's file must never have its permissions changed.
fn open(path: &Path) -> io::Result<File> {
    use nix::fcntl::OFlag;
    let flags = OFlag::O_RDONLY | OFlag::O_CREAT | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let file = File::from(nix::fcntl::open(
        path,
        flags,
        nix::sys::stat::Mode::from_bits_truncate(0o666),
    )?);
    if file.metadata().is_ok_and(|metadata| metadata.nlink() == 1) {
        // Fails harmlessly for another user's lock.
        let _ = file.set_permissions(Permissions::from_mode(0o666));
    }
    Ok(file)
}
