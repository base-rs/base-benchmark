// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::test_support::suite;
use std::os::unix::fs::PermissionsExt;

/// A fake `cargo build` that writes an executable into its target directory and reports it, after running `behavior`.
fn fake_cargo(directory: &Path, name: &str, behavior: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in --target-dir) target=$2; shift;; --manifest-path) manifest=$2; shift;; esac; shift; done\noutput=$(dirname \"$(dirname \"$target\")\")\n{behavior}\nmkdir -p \"$target\"\nprintf 'binary' > \"$target/fixture\"\necho '{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"fixture\",\"kind\":[\"bin\"]}},\"executable\":\"'\"$target/fixture\"'\"}}'\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn tree(directory: &Path, name: &str) -> Tree {
    let path = directory.join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("Cargo.lock"), "lock").unwrap();
    Tree {
        label: name.to_owned(),
        path,
    }
}

fn options(directory: &Path, trees: Vec<Tree>, cargo: PathBuf) -> PrepareOptions {
    PrepareOptions {
        suite: suite(Path::new("/work.sh"), &[]),
        trees,
        replica: None,
        output: directory.join("prepared"),
        cargo,
        rustc: PathBuf::from("/bin/echo"),
    }
}

#[test]
fn trees_are_labelled_paths() {
    assert_eq!(
        "base=/tmp/a=b".parse::<Tree>().unwrap(),
        Tree {
            label: "base".to_owned(),
            path: PathBuf::from("/tmp/a=b")
        }
    );
    assert!("base".parse::<Tree>().is_err());
    assert!("base=".parse::<Tree>().is_err());
}

#[test]
fn freezes_reported_executables_and_rebuilds_a_replica() {
    let directory = tempfile::tempdir().unwrap();
    let cargo = fake_cargo(directory.path(), "cargo", ":");
    let base = tree(directory.path(), "base");
    let git = |arguments: &[&str]| {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&base.path)
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(arguments)
                .output()
                .unwrap()
                .status
                .success()
        )
    };
    git(&["init", "--quiet"]);
    git(&["add", "Cargo.lock"]);
    git(&["commit", "--quiet", "-m", "Initial commit."]);
    let mut options = options(
        directory.path(),
        vec![base.clone(), tree(directory.path(), "change")],
        cargo,
    );
    options.suite.features = vec!["extra".to_owned()];
    let prepared = prepare(&options).unwrap();
    let [baseline, candidate, replica] = &prepared.manifest.artifacts[..] else {
        panic!("expected three artifacts")
    };
    assert!(baseline.source.is_frozen());
    assert_eq!(
        (candidate.source.revision.as_ref(), candidate.role),
        (None, ArtifactRole::Candidate)
    );
    assert_eq!(
        (replica.label.as_str(), replica.source.revision.as_ref()),
        ("base-replica", baseline.source.revision.as_ref())
    );
    assert!(!replica.source.path.exists());
    assert!(baseline.build.command.contains(&"extra".to_owned()));
    assert_eq!(baseline.build.toolchain, "-Vv");
    assert_eq!(std::fs::read(&baseline.executable).unwrap(), b"binary");
    assert!(
        baseline
            .executable
            .starts_with(directory.path().join("prepared/artifacts/base"))
    );
    assert!(baseline.source.lockfile_sha256.is_some());
    assert_eq!(
        ArtifactManifest::read(&prepared.manifest_path).unwrap(),
        prepared.manifest
    );
}

#[test]
fn invalid_requests_fail_before_building() {
    let directory = tempfile::tempdir().unwrap();
    let cargo = PathBuf::from("/nonexistent/cargo");
    let a = || tree(directory.path(), "a");
    let named = |label: &str| Tree {
        label: label.to_owned(),
        ..a()
    };
    let cases = [
        (Vec::new(), "at least one source tree"),
        (vec![named("a b")], "invalid or duplicate"),
        (vec![a(), a()], "invalid or duplicate"),
        (
            vec![a(), named("a-replica")],
            "reserved for the layout replica",
        ),
        (
            vec![Tree {
                label: "a".to_owned(),
                path: directory.path().join("missing"),
            }],
            "missing",
        ),
        (
            vec![
                Tree {
                    label: "root".to_owned(),
                    path: PathBuf::from("/"),
                },
                a(),
            ],
            "no parent directory",
        ),
    ];
    for (trees, message) in cases {
        let error = prepare(&options(directory.path(), trees, cargo.clone())).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    std::fs::create_dir(directory.path().join(".a.base-benchmark-replica-prepared")).unwrap();
    let mut stale = options(directory.path(), vec![a()], cargo.clone());
    stale.replica = Some(true);
    assert!(
        prepare(&stale)
            .unwrap_err()
            .to_string()
            .contains("stale layout replica")
    );

    let mut existing = options(directory.path(), vec![a()], cargo.clone());
    existing.output = directory.path().to_owned();
    assert!(
        prepare(&existing)
            .unwrap_err()
            .to_string()
            .contains("needs a new directory")
    );
    existing.output = PathBuf::new();
    assert!(prepare(&existing).is_err());
    existing.output = directory.path().join("a/Cargo.lock/inside");
    assert!(
        prepare(&existing)
            .unwrap_err()
            .to_string()
            .contains("needs a new directory")
    );
    existing.output = directory.path().join("unrunnable");
    assert!(
        prepare(&existing)
            .unwrap_err()
            .to_string()
            .contains("cannot run /nonexistent/cargo")
    );
}

#[test]
fn build_and_freeze_failures_identify_the_tree() {
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        ("exit 1", "a: cargo build failed"),
        ("exit 0", "a: cargo reported no executable"),
        (
            "mkdir -p \"$output/artifacts/a/fixture\"",
            "a: cannot freeze",
        ),
        ("mkdir \"$output/artifacts.json\"", "artifacts.json"),
        (
            "case \"$manifest\" in *replica*) exit 1;; esac",
            "a-replica: cargo build failed",
        ),
    ];
    for (index, (behavior, message)) in cases.into_iter().enumerate() {
        let case = directory.path().join(index.to_string());
        let cargo = fake_cargo(directory.path(), &format!("cargo-{index}"), behavior);
        let mut options = options(&case, vec![tree(&case, "a")], cargo);
        options.replica = Some(true);
        let error = prepare(&options).unwrap_err();
        assert!(error.to_string().contains(message), "{message}: {error}");
        assert!(!case.join(".a.base-benchmark-replica-prepared").exists());
    }
    let mut options = options(
        directory.path(),
        vec![tree(directory.path(), "r")],
        fake_cargo(directory.path(), "cargo", ":"),
    );
    options.rustc = PathBuf::from("/nonexistent/rustc");
    assert!(
        prepare(&options)
            .unwrap_err()
            .to_string()
            .contains("could not run /nonexistent/rustc")
    );
}

#[test]
fn replicas_are_not_built_when_the_copy_fails() {
    let directory = tempfile::tempdir().unwrap();
    let tree = tree(directory.path(), "a");
    let secret = tree.path.join("secret");
    std::fs::write(&secret, "x").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&secret).is_ok() {
        return; // Privileged users can read the file, so the copy cannot fail.
    }
    let mut options = options(
        directory.path(),
        vec![tree],
        fake_cargo(directory.path(), "cargo", ":"),
    );
    options.replica = Some(true);
    assert!(
        prepare(&options)
            .unwrap_err()
            .to_string()
            .contains("secret")
    );
}

#[test]
fn reported_executables_must_be_binaries_of_the_package() {
    let messages = [
        r#"{"reason":"compiler-artifact","target":{"name":"fixture","kind":["lib"]},"executable":null}"#,
        r#"{"reason":"build-script-executed"}"#,
        "not json",
        r#"{"reason":"compiler-artifact","target":{"name":"other","kind":["bin"]},"executable":"/other"}"#,
        r#"{"reason":"compiler-artifact","target":{"name":"fixture","kind":["bin"]},"executable":"/fixture"}"#,
    ];
    assert_eq!(
        executable(&messages.join("\n"), "fixture"),
        Some(PathBuf::from("/fixture"))
    );
    assert_eq!(executable(&messages[..4].join("\n"), "fixture"), None);
}

#[test]
fn copies_skip_version_control_targets_and_output() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    for path in ["src", ".git", "target", "nested/target", "output"] {
        std::fs::create_dir_all(source.join(path)).unwrap();
    }
    std::fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(source.join("target/CACHEDIR.TAG"), "").unwrap();
    std::fs::write(source.join("nested/target/CACHEDIR.TAG"), "").unwrap();
    std::os::unix::fs::symlink("src/main.rs", source.join("link")).unwrap();
    let copy = directory.path().join("copy");
    copy_tree(&source, &copy, &source.join("output")).unwrap();
    assert_eq!(
        std::fs::read_to_string(copy.join("src/main.rs")).unwrap(),
        "fn main() {}"
    );
    assert_eq!(
        std::fs::read_link(copy.join("link")).unwrap(),
        PathBuf::from("src/main.rs")
    );
    for skipped in [".git", "target", "nested/target", "output"] {
        assert!(!copy.join(skipped).exists(), "{skipped}");
    }
    assert!(copy.join("nested").is_dir());
    assert!(copy_tree(&source, &copy, &source).is_err());
    assert!(
        copy_tree(
            &directory.path().join("missing"),
            &directory.path().join("other"),
            &source
        )
        .unwrap_err()
        .to_string()
        .contains("missing")
    );
}

#[test]
fn replicas_are_siblings_of_the_baseline() {
    let path = sibling_copy_path(
        Path::new("/work/project"),
        Path::new("/work/project/target/out-1"),
    )
    .unwrap();
    assert_eq!(
        path,
        PathBuf::from("/work/.project.base-benchmark-replica-out-1")
    );
}
