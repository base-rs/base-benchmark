// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

#[test]
fn paths_are_normalized_inside_the_mirror() {
    assert_eq!(
        normalized(Path::new("/a/b/../../../c")),
        PathBuf::from("/c")
    );
    let remote = Remote::new("host");
    assert_eq!(
        remote.mirror(&normalized(Path::new("/../../etc"))),
        ".cache/base-benchmark/mirror/etc"
    );
}

#[test]
fn words_are_quoted_for_a_posix_shell() {
    assert_eq!(quote("plain"), "'plain'");
    assert_eq!(quote("it's"), "'it'\\''s'");
    let output = Command::new("/bin/sh")
        .args(["-c", &format!("printf %s {}", quote("a 'b' $c \\d"))])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "a 'b' $c \\d");
}

#[test]
fn only_preparation_and_collection_run_remotely() {
    let remote = Remote::new("host");
    assert_eq!(remote.directory, ".cache/base-benchmark");
    assert_eq!(
        remote.mirror(Path::new("/work/tree")),
        ".cache/base-benchmark/mirror/work/tree"
    );
    for command in ["list", "summarize"] {
        let error = remote
            .execute(command, Vec::new(), &Cancellation::new())
            .unwrap_err();
        assert!(
            error.to_string().contains("only prepare, run, and smoke"),
            "{error}"
        );
    }
    let error = remote
        .execute("prepare", vec!["--output".to_owned()], &Cancellation::new())
        .unwrap_err();
    assert!(
        error.to_string().contains("--output needs a value"),
        "{error}"
    );
}
