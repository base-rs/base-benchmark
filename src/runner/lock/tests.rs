// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

#[test]
fn waits_reports_and_stops_when_cancelled() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.lock");
    let held = HostLock::acquire(&path, &Cancellation::new(), &mut Vec::new()).unwrap();
    let cancellation = Cancellation::new();
    let canceller = cancellation.clone();
    let stop = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        canceller.cancel();
    });
    let mut progress = Vec::new();
    let error = HostLock::acquire(&path, &cancellation, &mut progress)
        .err()
        .unwrap();
    stop.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(
        String::from_utf8(progress)
            .unwrap()
            .matches("waiting for host lock")
            .count(),
        1
    );
    drop(held);
    HostLock::acquire(&path, &Cancellation::new(), &mut Vec::new()).unwrap();
}

#[test]
fn unopenable_lock_paths_are_errors() {
    let error = HostLock::acquire(
        Path::new("/nonexistent/directory/host.lock"),
        &Cancellation::new(),
        &mut Vec::new(),
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("/nonexistent/directory/host.lock")
    );
}

#[test]
fn lock_failures_other_than_contention_are_errors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.lock");
    let error = HostLock::acquire_with(&path, &Cancellation::new(), &mut Vec::new(), |file| {
        Err((file, Errno::ENOLCK))
    })
    .err()
    .unwrap();
    assert!(error.to_string().contains("host.lock"));
}

#[test]
fn locks_are_shared_between_users() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.lock");
    drop(HostLock::acquire(&path, &Cancellation::new(), &mut Vec::new()).unwrap());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o666
    );
    // An existing lock is opened read-only, which is enough for `flock`.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    drop(HostLock::acquire(&path, &Cancellation::new(), &mut Vec::new()).unwrap());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&path).is_err() {
        assert!(HostLock::acquire(&path, &Cancellation::new(), &mut Vec::new()).is_err());
    }
}

#[test]
fn planted_links_are_never_followed_or_changed() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let victim = directory.path().join("victim");
    let symlink = directory.path().join("symlink.lock");
    std::os::unix::fs::symlink(&victim, &symlink).unwrap();
    assert!(HostLock::acquire(&symlink, &Cancellation::new(), &mut Vec::new()).is_err());
    assert!(
        !victim.exists(),
        "a dangling symbolic link must not create its target"
    );

    std::fs::write(&victim, "secret").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
    let hard = directory.path().join("hard.lock");
    std::fs::hard_link(&victim, &hard).unwrap();
    drop(HostLock::acquire(&hard, &Cancellation::new(), &mut Vec::new()).unwrap());
    assert_eq!(
        std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
