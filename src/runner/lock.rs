// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::Cancellation;
use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::fcntl::{Flock, FlockArg};
use nix::sys::stat::Mode;
use std::fs::{File, Permissions};
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
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

/// Open or create the lock file read-only, since `flock` needs no write access.
///
/// The path is shared with every user, so it is never followed as a symbolic link. An existing lock is opened without `O_CREAT`, which Linux's `fs.protected_regular` forbids for another user's file in a sticky `/tmp`. A missing lock is created exclusively, and only that new file is made usable by every user despite the umask.
fn open(path: &Path) -> io::Result<File> {
    open_with(path, |path| {
        let flags =
            OFlag::O_RDONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        nix::fcntl::open(path, flags, Mode::from_bits_truncate(0o666))
    })
}

/// `create` makes a new lock file exclusively, replaceable so tests can lose the creation race.
fn open_with(
    path: &Path,
    mut create: impl FnMut(&Path) -> nix::Result<OwnedFd>,
) -> io::Result<File> {
    loop {
        match nix::fcntl::open(
            path,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            Err(Errno::ENOENT) => {}
            opened => return Ok(File::from(opened?)),
        }
        match create(path) {
            // Another process created it first; open theirs.
            Err(Errno::EEXIST) => {}
            created => {
                let file = File::from(created?);
                // Without this, the lock still works for this user; others would fail to open it with a clear error.
                let _ = file.set_permissions(Permissions::from_mode(0o666));
                return Ok(file);
            }
        }
    }
}
