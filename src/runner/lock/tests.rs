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
