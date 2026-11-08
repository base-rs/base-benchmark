// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::test_support::suite;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

/// A fake `cargo`: `-Vv` prints a version, `metadata` prints `metadata`, and `build` runs `behavior`, then writes and reports an executable in its target directory.
fn fake_cargo(directory: &Path, name: &str, behavior: &str, metadata: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$1\" in -Vv) echo 'cargo fake'; exit 0;; metadata) echo '{metadata}'; exit 0;; esac\nwhile [ $# -gt 0 ]; do case \"$1\" in --target-dir) target=$2; shift;; --manifest-path) manifest=$2; shift;; esac; shift; done\noutput=$(dirname \"$(dirname \"$target\")\")\n{behavior}\nmkdir -p \"$target\"\nprintf 'binary' > \"$target/fixture\"\necho '{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"fixture\",\"kind\":[\"bin\"]}},\"executable\":\"'\"$target/fixture\"'\"}}'\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn cargo(directory: &Path) -> PathBuf {
    fake_cargo(directory, "cargo", ":", "{}")
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

fn commit(path: &Path) {
    let git = |arguments: &[&str]| {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(path)
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(arguments)
                .output()
                .unwrap()
                .status
                .success()
        )
    };
    git(&["init", "--quiet"]);
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "Initial commit."]);
}

fn options(directory: &Path, trees: Vec<Tree>, cargo: PathBuf) -> PrepareOptions {
    PrepareOptions {
        suite: suite(Path::new("/work.sh"), &[]),
        trees,
        replicas: None,
        output: directory.join("prepared"),
        feature_sets: Vec::new(),
        overlay: None,
        cargo,
        rustc: PathBuf::from("/bin/echo"),
    }
}

fn prepared(options: &PrepareOptions) -> io::Result<Prepared> {
    prepare(options, &Cancellation::new())
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
    let base = tree(directory.path(), "base");
    commit(&base.path);
    let mut options = options(
        directory.path(),
        vec![base.clone(), tree(directory.path(), "change")],
        cargo(directory.path()),
    );
    options.suite.features = vec!["extra".to_owned()];
    let prepared = prepared(&options).unwrap();
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
    assert_eq!(
        (
            baseline.build.toolchain.as_str(),
            baseline.build.cargo.as_str()
        ),
        ("-Vv", "cargo fake")
    );
    assert_eq!(std::fs::read(&baseline.executable).unwrap(), b"binary");
    assert!(
        baseline.executable.starts_with(
            directory
                .path()
                .canonicalize()
                .unwrap()
                .join("prepared/artifacts/base")
        )
    );
    assert!(
        !directory.path().join("prepared/targets/base").exists(),
        "build directories are removed after freezing"
    );
    assert!(baseline.source.lockfile_sha256.is_some());
    assert_eq!(
        ArtifactManifest::read(&prepared.manifest_path).unwrap(),
        prepared.manifest
    );
}

#[test]
fn several_replicas_sample_layout() {
    let directory = tempfile::tempdir().unwrap();
    let mut options = options(
        directory.path(),
        vec![tree(directory.path(), "base")],
        cargo(directory.path()),
    );
    options.replicas = Some(2);
    let labels: Vec<_> = prepared(&options)
        .unwrap()
        .manifest
        .artifacts
        .into_iter()
        .map(|artifact| (artifact.label, artifact.role))
        .collect();
    assert_eq!(
        labels,
        [
            ("base".to_owned(), ArtifactRole::Baseline),
            ("base-replica-1".to_owned(), ArtifactRole::LayoutReplica),
            ("base-replica-2".to_owned(), ArtifactRole::LayoutReplica)
        ]
    );
}

#[test]
fn out_of_tree_path_dependencies_are_identified() {
    let directory = tempfile::tempdir().unwrap();
    let dependency = directory.path().join("dependency");
    std::fs::create_dir(&dependency).unwrap();
    std::fs::write(dependency.join("Cargo.toml"), "").unwrap();
    commit(&dependency);
    let base = tree(directory.path(), "base");
    commit(&base.path);
    let metadata = json!({ "packages": [
        { "name": "dependency", "source": null, "manifest_path": dependency.join("Cargo.toml") },
        { "name": "member", "source": null, "manifest_path": base.path.join("member/Cargo.toml") },
        { "name": "serde", "source": "registry+https://github.com/rust-lang/crates.io-index", "manifest_path": "/registry/serde/Cargo.toml" },
    ] });
    let cargo = fake_cargo(directory.path(), "cargo", ":", &metadata.to_string());
    let artifact = prepared(&options(directory.path(), vec![base], cargo))
        .unwrap()
        .manifest
        .artifacts
        .remove(0);
    let [dependency_identity] = artifact.source.path_dependencies.as_deref().unwrap() else {
        panic!("{:?}", artifact.source.path_dependencies)
    };
    assert_eq!(
        (dependency_identity.name.as_str(), dependency_identity.dirty),
        ("dependency", Some(false))
    );
    assert!(artifact.source.is_frozen());
    std::fs::write(dependency.join("changed"), "").unwrap();
    let directory_two = tempfile::tempdir().unwrap();
    let base = tree(directory_two.path(), "base");
    commit(&base.path);
    let cargo = fake_cargo(directory_two.path(), "cargo", ":", &metadata.to_string());
    let artifact = prepared(&options(directory_two.path(), vec![base], cargo))
        .unwrap()
        .manifest
        .artifacts
        .remove(0);
    assert!(
        !artifact.source.is_frozen(),
        "a modified path dependency is not frozen"
    );
}

#[test]
fn build_settings_are_recorded() {
    let environment = build_environment(
        [
            ("RUSTFLAGS", "-Ctarget-cpu=native"),
            ("CARGO_PROFILE_RELEASE_LTO", "true"),
            ("HOME", "/home"),
            ("CARGO_PKG_NAME", "x"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned())),
    );
    assert_eq!(
        environment.keys().collect::<Vec<_>>(),
        ["CARGO_PROFILE_RELEASE_LTO", "RUSTFLAGS"]
    );

    let directory = tempfile::tempdir().unwrap();
    let tree = directory.path().join("outer/tree");
    std::fs::create_dir_all(tree.join(".cargo")).unwrap();
    std::fs::create_dir_all(directory.path().join("outer/.cargo")).unwrap();
    std::fs::create_dir_all(directory.path().join("home")).unwrap();
    std::fs::write(tree.join(".cargo/config.toml"), "[build]\n").unwrap();
    std::fs::write(directory.path().join("outer/.cargo/config"), "[build]\n").unwrap();
    std::fs::write(directory.path().join("home/config.toml"), "[net]\n").unwrap();
    let files = configuration_files(&tree, Some(directory.path().join("home")));
    let paths: Vec<_> = files
        .iter()
        .map(|file| file.path.strip_prefix(directory.path()).unwrap().to_owned())
        .collect();
    assert_eq!(
        paths,
        [
            PathBuf::from("outer/tree/.cargo/config.toml"),
            PathBuf::from("outer/.cargo/config"),
            PathBuf::from("home/config.toml")
        ]
    );
    assert_eq!(
        configuration_files(&tree, Some(tree.join(".cargo"))).len(),
        2,
        "a repeated directory is listed once"
    );

    let tree = Path::new("/workspace");
    let metadata = json!({ "packages": [{ "name": "inside", "source": null, "manifest_path": "/workspace/a/Cargo.toml" }, { "name": "broken", "source": null }, { "name": "root", "source": null, "manifest_path": "/" }] });
    assert!(external_path_packages(&metadata, tree).is_empty());
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
            "reserved for a layout replica",
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
        let error = prepared(&options(directory.path(), trees, cargo.clone())).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    let mut stale = options(directory.path(), vec![a()], cargo.clone());
    stale.replicas = Some(1);
    stale.output = directory.path().join("stale");
    std::fs::create_dir(directory.path().join(format!(
        ".a.base-benchmark-a-replica-stale-{}",
        std::process::id()
    )))
    .unwrap();
    assert!(
        prepared(&stale)
            .unwrap_err()
            .to_string()
            .contains("must be a new directory")
    );

    let mut existing = options(directory.path(), vec![a()], cargo.clone());
    existing.output = directory.path().to_owned();
    assert!(
        prepared(&existing)
            .unwrap_err()
            .to_string()
            .contains("needs a new directory")
    );
    existing.output = PathBuf::new();
    assert!(prepared(&existing).is_err());
    existing.output = directory.path().join("a/Cargo.lock/inside");
    assert!(
        prepared(&existing)
            .unwrap_err()
            .to_string()
            .contains("needs a new directory")
    );
    existing.output = directory.path().join("unrunnable");
    assert!(
        prepared(&existing)
            .unwrap_err()
            .to_string()
            .contains("cannot run /nonexistent/cargo")
    );
    existing.output = directory.path().join("cancelled");
    let cancellation = Cancellation::new();
    cancellation.cancel();
    assert_eq!(
        prepare(&existing, &cancellation).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn build_and_freeze_failures_identify_the_tree() {
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        ("exit 1", "a: cargo build failed"),
        ("exit 0", "a: cargo reported no executable"),
        (
            "mkdir -p \"$output/artifacts/a/0/fixture\"",
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
        let cargo = fake_cargo(directory.path(), &format!("cargo-{index}"), behavior, "{}");
        let mut options = options(&case, vec![tree(&case, "a")], cargo);
        options.replicas = Some(1);
        let error = prepared(&options).unwrap_err();
        assert!(error.to_string().contains(message), "{message}: {error}");
        let leftovers: Vec<_> = std::fs::read_dir(&case)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("replica"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "replica copies are removed after a failure"
        );
    }
    let mut options = options(
        directory.path(),
        vec![tree(directory.path(), "r")],
        cargo(directory.path()),
    );
    options.rustc = PathBuf::from("/nonexistent/rustc");
    assert!(
        prepared(&options)
            .unwrap_err()
            .to_string()
            .contains("could not run /nonexistent/rustc")
    );
    let unversioned = fake_cargo(directory.path(), "unversioned", ":", "{}");
    std::fs::write(
        &unversioned,
        std::fs::read_to_string(&unversioned)
            .unwrap()
            .replace("-Vv) echo 'cargo fake'; exit 0", "-Vv) exit 1"),
    )
    .unwrap();
    options.rustc = PathBuf::from("/bin/echo");
    options.cargo = unversioned;
    options.output = directory.path().join("unversioned-output");
    assert!(
        prepared(&options)
            .unwrap_err()
            .to_string()
            .contains("unversioned -Vv")
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
    let mut options = options(directory.path(), vec![tree], cargo(directory.path()));
    options.replicas = Some(1);
    assert!(
        prepared(&options)
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
    std::fs::create_dir(&copy).unwrap();
    copy_contents(&source, &copy, &source.join("output")).unwrap();
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
    assert!(copy_contents(&source, &copy, &source).is_err());
    assert!(
        copy_contents(&directory.path().join("missing"), &copy, &source)
            .unwrap_err()
            .to_string()
            .contains("missing")
    );
}

#[test]
fn replicas_are_hidden_siblings_unique_to_the_process() {
    let path = sibling_copy_path(
        Path::new("/work/project"),
        "base-replica",
        Path::new("/work/project/target/out-1"),
    )
    .unwrap();
    assert_eq!(
        path,
        PathBuf::from(format!(
            "/work/.project.base-benchmark-base-replica-out-1-{}",
            std::process::id()
        ))
    );
}

#[test]
fn unreadable_metadata_leaves_path_dependencies_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let base = tree(directory.path(), "base");
    commit(&base.path);
    let cargo = fake_cargo(directory.path(), "cargo", ":", "not json");
    let mut options = options(directory.path(), vec![base], cargo);
    options.replicas = Some(1);
    let artifacts = prepared(&options).unwrap().manifest.artifacts;
    assert_eq!(artifacts[0].source.path_dependencies, None);
    assert!(
        !artifacts[0].source.is_frozen(),
        "unknown path dependencies are not frozen"
    );
    assert_eq!(
        artifacts[1].source.path_dependencies, None,
        "a replica repeats its baseline's"
    );
}

#[test]
fn hosts_and_cargo_homes_are_resolved() {
    assert_eq!(
        host("rustc 1.99.1\nhost: x86_64-unknown-linux-gnu\nrelease: 1.99.1"),
        Some("x86_64-unknown-linux-gnu")
    );
    assert_eq!(host("-Vv"), None);
    let tree = Path::new("/tree");
    assert_eq!(
        cargo_home(Some("/cargo".into()), Some("/home".into()), tree),
        Some(PathBuf::from("/cargo"))
    );
    // A relative `$CARGO_HOME` is relative to the tree, where Cargo runs, and an empty one is unset.
    assert_eq!(
        cargo_home(Some("cargo".into()), None, tree),
        Some(PathBuf::from("/tree/cargo"))
    );
    assert_eq!(
        cargo_home(Some("".into()), Some("/home".into()), tree),
        Some(PathBuf::from("/home/.cargo"))
    );
    assert_eq!(
        cargo_home(None, Some("/home".into()), tree),
        Some(PathBuf::from("/home/.cargo"))
    );
    assert_eq!(cargo_home(None, None, tree), None);
}

#[test]
fn feature_sets_are_canonical_and_built_once() {
    let directory = tempfile::tempdir().unwrap();
    let mut options = options(
        directory.path(),
        vec![tree(directory.path(), "t")],
        cargo(directory.path()),
    );
    options.feature_sets = vec![
        vec!["b".to_owned(), "a".to_owned()],
        vec!["a, b".to_owned()],
    ];
    let prepared = prepared(&options).unwrap();
    assert_eq!(prepared.manifest.artifacts.len(), 1);
    assert_eq!(
        prepared.manifest.artifacts[0].features.as_deref(),
        Some(&["a".to_owned(), "b".to_owned()][..])
    );
    assert!(
        prepared.manifest.artifacts[0]
            .build
            .command
            .ends_with(&["--features".to_owned(), "a,b".to_owned()])
    );
}

#[test]
fn overlay_failures_stop_preparation_and_remove_every_copy() {
    let directory = tempfile::tempdir().unwrap();
    let mut options = options(
        directory.path(),
        vec![tree(directory.path(), "t")],
        cargo(directory.path()),
    );
    options.replicas = Some(1);
    options.overlay = Some(Overlay {
        relative: PathBuf::from("bench"),
        source: directory.path().join("missing"),
        lockfile: None,
    });
    let error = prepared(&options).unwrap_err();
    assert!(error.to_string().contains("missing"), "{error}");
    let leftovers = std::fs::read_dir(directory.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
        .count();
    assert_eq!(leftovers, 0, "reserved copies are removed after a failure");
}

const TREE_LOCK: &str = "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"library\"\nversion = \"0.1.0\"\ndependencies = [\n \"signal\",\n]\n\n[[package]]\nname = \"signal\"\nversion = \"0.4.5\"\nsource = \"registry+https://example.com\"\n";

#[test]
fn seeded_lockfiles_add_only_entries_the_tree_lacks() {
    let directory = tempfile::tempdir().unwrap();
    let copy = directory.path().join("Cargo.lock");
    let current = directory.path().join("current.lock");
    std::fs::write(&copy, TREE_LOCK).unwrap();
    std::fs::write(
        &current,
        "version = 4\n\n[[package]]\nname = \"harness\"\nversion = \"1.0.0\"\nsource = \"registry+https://example.com\"\ndependencies = [\n \"helper 2.0.0\",\n]\n\n[[package]]\nname = \"helper\"\nversion = \"2.0.0\"\n\n[[package]]\nname = \"runner\"\nversion = \"1.0.0\"\ndependencies = [\n \"signal\",\n]\n\n[[package]]\nname = \"user\"\nversion = \"1.0.0\"\ndependencies = [\n \"runner\",\n]\n\n[[package]]\nname = \"signal\"\nversion = \"0.4.6\"\nsource = \"registry+https://example.com\"\n",
    )
    .unwrap();
    seed_lockfile(&copy, &current).unwrap();
    let seeded = std::fs::read_to_string(&copy).unwrap();
    assert!(seeded.starts_with(TREE_LOCK.trim_end()));
    // Unlocked packages are added with their dependencies.
    assert_eq!(seeded.matches("name = \"harness\"").count(), 1);
    assert_eq!(seeded.matches("name = \"helper\"").count(), 1);
    // The tree keeps its own version of a package it locks, and what depends on another version of it, directly or not, is left to Cargo.
    assert_eq!(seeded.matches("version = \"0.4.6\"").count(), 0);
    assert_eq!(seeded.matches("name = \"runner\"").count(), 0);
    assert_eq!(seeded.matches("name = \"user\"").count(), 0);
    assert!(seed_lockfile(&copy, &directory.path().join("missing.lock")).is_err());
    // A tree without a lockfile takes the current one's entries.
    let empty = directory.path().join("empty.lock");
    seed_lockfile(&empty, &current).unwrap();
    assert_eq!(
        std::fs::read_to_string(&empty)
            .unwrap()
            .matches("[[package]]")
            .count(),
        5
    );
}

#[test]
fn lockfile_changes_name_every_locked_version_that_moved_and_harness_drift() {
    let bumped = TREE_LOCK.replace("0.4.5", "0.4.6");
    assert_eq!(
        lockfile_changes(TREE_LOCK, &bumped, ""),
        ["signal 0.4.5 -> 0.4.6"]
    );
    let added = format!("{TREE_LOCK}\n[[package]]\nname = \"harness\"\nversion = \"1.0.0\"\n");
    assert!(
        lockfile_changes(TREE_LOCK, &added, &added).is_empty(),
        "the current version of an added package changes nothing"
    );
    let current = added.replace("1.0.0", "1.0.1");
    assert_eq!(
        lockfile_changes(TREE_LOCK, &added, &current),
        ["harness 1.0.0, not the current workspace's 1.0.1"]
    );
    let removed = TREE_LOCK
        .split("[[package]]\nname = \"signal\"")
        .next()
        .unwrap();
    assert!(
        lockfile_changes(TREE_LOCK, removed, "").is_empty(),
        "a package no longer used changes nothing"
    );
    assert!(lockfile_changes("", TREE_LOCK, "").is_empty());
}

#[test]
fn lockfiles_with_windows_line_endings_are_read_alike() {
    let directory = tempfile::tempdir().unwrap();
    let copy = directory.path().join("Cargo.lock");
    let current = directory.path().join("current.lock");
    std::fs::write(&copy, TREE_LOCK.replace('\n', "\r\n")).unwrap();
    std::fs::write(&current, TREE_LOCK.replace('\n', "\r\n")).unwrap();
    seed_lockfile(&copy, &current).unwrap();
    // Nothing is added twice: every entry of the current lockfile is already locked.
    assert_eq!(
        std::fs::read_to_string(&copy)
            .unwrap()
            .matches("[[package]]")
            .count(),
        2
    );
}

#[test]
fn layout_salt_wrappers_add_the_salt_after_any_existing_wrapper() {
    let directory = tempfile::tempdir().unwrap();
    let wrapper = |existing: Option<&str>| {
        let mut cargo = Command::new("cargo");
        salt_crates(
            &mut cargo,
            directory.path(),
            "it's",
            existing.map(std::ffi::OsString::from),
        )
        .unwrap();
        cargo
            .get_envs()
            .find(|(name, _)| *name == "RUSTC_WRAPPER")
            .and_then(|(_, value)| value)
            .map(PathBuf::from)
            .unwrap()
    };
    let run = |wrapper: &Path| {
        // Run through the shell rather than executed, so writing it cannot race with another thread's fork. `printf %s|` stands in for rustc, so the output shows the arguments it received.
        let output = Command::new(crate::test_support::SHELL)
            .arg(wrapper)
            .args(["printf", "%s|"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    let plain = wrapper(None);
    assert_eq!(run(&plain), "-C|metadata=it's|");
    let chained = wrapper(Some("/usr/bin/env"));
    assert_eq!(run(&chained), "-C|metadata=it's|");
    // The name depends only on the salt and the chained wrapper, so the build depends on nothing else.
    assert_ne!(plain, chained);
    assert_eq!(wrapper(None), plain);
    assert!(plain.starts_with(directory.path()));
    let error = salt_crates(
        &mut Command::new("cargo"),
        &plain.join("inside"),
        "salt",
        None,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot write the layout salt wrapper"),
        "{error}"
    );
}

#[test]
fn wrappers_follow_cargos_precedence_and_paths() {
    let directory = tempfile::tempdir().unwrap();
    let tree = directory.path().join("project");
    let os = |value: &str| Some(std::ffi::OsString::from(value));
    assert_eq!(
        existing_wrapper([os("sccache"), os("other")], &[], &tree).unwrap(),
        os("sccache")
    );
    assert_eq!(
        existing_wrapper([None, os("other")], &[], &tree).unwrap(),
        os("other")
    );
    let nearest = directory.path().join("project/.cargo");
    let home = directory.path().join("home/.cargo");
    std::fs::create_dir_all(&nearest).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let file = |path: PathBuf| ConfigurationFile {
        path,
        sha256: String::new(),
    };
    let files = [
        file(nearest.join("config.toml")),
        file(home.join("config.toml")),
    ];
    let write = |path: &Path, text: &str| std::fs::write(path, text).unwrap();
    write(
        &home.join("config.toml"),
        "[build]\nrustc-wrapper = \"sccache\"\n",
    );
    // The nearest file that sets the wrapper wins; one that does not defers to the next.
    write(
        &nearest.join("config.toml"),
        "[build]\njobs = 4 # rustc-wrapper = \"x\"\n",
    );
    assert_eq!(
        existing_wrapper([None, None], &files, &tree).unwrap(),
        os("sccache")
    );
    // Without a file that sets it, there is no wrapper.
    assert_eq!(
        existing_wrapper([None, None], &files[..1], &tree).unwrap(),
        None
    );
    // A path from the environment is relative to the tree, where Cargo runs; a value that is not UTF-8 is ignored, as Cargo ignores it.
    assert_eq!(
        existing_wrapper([os("bin/wrap"), None], &files, &tree).unwrap(),
        Some(tree.join("bin/wrap").into_os_string())
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let invalid = Some(std::ffi::OsString::from_vec(vec![0xff]));
        assert_eq!(
            existing_wrapper([invalid, os("other")], &files, &tree).unwrap(),
            os("other")
        );
    }
    // An empty value in the environment overrides every configuration.
    assert_eq!(
        existing_wrapper([os(""), None], &files, &tree).unwrap(),
        None
    );
    // A path with a slash is relative to the directory holding `.cargo`; every TOML spelling of the key counts.
    let relative = directory.path().join("project/bin/wrap").into_os_string();
    for spelling in [
        "[build]\nrustc-wrapper = \"bin/wrap\"\n",
        "build.rustc-wrapper = \"bin/wrap\"\n",
        "build = { jobs = 4, rustc-wrapper = \"bin/wrap\" }\n",
        "[build]\n\"rustc-wrapper\" = \"bin/wrap\" # a comment\n",
    ] {
        write(&nearest.join("config.toml"), spelling);
        assert_eq!(
            existing_wrapper([None, None], &files, &tree).unwrap(),
            Some(relative.clone()),
            "{spelling}"
        );
    }
    write(
        &nearest.join("config.toml"),
        "[build]\nrustc-wrapper = \"/opt/#tools/wrap\"\n",
    );
    assert_eq!(
        existing_wrapper([None, None], &files, &tree).unwrap(),
        os("/opt/#tools/wrap")
    );
    // An empty configured value overrides the files after it.
    write(
        &nearest.join("config.toml"),
        "[build]\nrustc-wrapper = \"\"\n",
    );
    assert_eq!(existing_wrapper([None, None], &files, &tree).unwrap(), None);
    // Cargo reads `config` instead of a `config.toml` beside it.
    write(&nearest.join("config"), "[build]\njobs = 2\n");
    let shadowed = [
        file(nearest.join("config.toml")),
        file(nearest.join("config")),
        file(home.join("config.toml")),
    ];
    assert_eq!(
        existing_wrapper([None, None], &shadowed, &tree).unwrap(),
        os("sccache")
    );
    // Files Cargo could not read are errors that name them.
    for (text, message) in [
        ("[build\n", "config.toml"),
        (
            "[build]\nrustc-wrapper = 3\n",
            "build.rustc-wrapper must be a string",
        ),
    ] {
        write(&nearest.join("config.toml"), text);
        let error = existing_wrapper([None, None], &files, &tree)
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{message}: {error}");
    }
    let unreadable = [file(directory.path().to_owned())];
    assert!(existing_wrapper([None, None], &unreadable, &tree).is_err());
}

#[test]
fn replicas_chain_a_configured_wrapper() {
    let directory = tempfile::tempdir().unwrap();
    let tree = tree(directory.path(), "t");
    std::fs::create_dir_all(tree.path.join(".cargo")).unwrap();
    std::fs::write(
        tree.path.join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \"configured-wrapper\"\n",
    )
    .unwrap();
    // The replica's build keeps a copy of the salting script it was given.
    let cargo = fake_cargo(
        directory.path(),
        "copying-cargo",
        "case \"$manifest\" in *replica*) cp \"$RUSTC_WRAPPER\" \"$output/salting\";; esac",
        "{}",
    );
    let mut options = options(directory.path(), vec![tree], cargo);
    options.replicas = Some(1);
    prepared(&options).unwrap();
    let script = std::fs::read_to_string(directory.path().join("prepared/salting")).unwrap();
    // A wrapper in the environment, as `cargo llvm-cov` sets, takes precedence over the configured one, as it does for Cargo.
    let expected = ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .unwrap_or_else(|| "configured-wrapper".to_owned());
    assert!(
        script.contains(&format!("exec '{expected}' \"$@\" -C metadata=")),
        "{script}"
    );
}

#[test]
fn included_configuration_sets_wrappers_as_cargo_includes_it() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home/.cargo");
    std::fs::create_dir_all(&home).unwrap();
    let config = home.join("config.toml");
    let files = [ConfigurationFile {
        path: config.clone(),
        sha256: String::new(),
    }];
    let write = |name: &str, text: &str| std::fs::write(home.join(name), text).unwrap();
    let wrapper = || existing_wrapper([None, None], &files, directory.path());
    let found = |name: &str| Some(std::ffi::OsString::from(name));
    // A path in an included file is relative to the directory holding its directory, here the home directory.
    write("config.toml", "include = [\"sccache.toml\"]\n");
    write("sccache.toml", "[build]\nrustc-wrapper = \"bin/wrap\"\n");
    assert_eq!(
        wrapper().unwrap(),
        Some(directory.path().join("home/bin/wrap").into_os_string())
    );
    // The including file's own value wins, and a later include over an earlier one.
    write("a.toml", "build.rustc-wrapper = \"a\"\n");
    write("b.toml", "build.rustc-wrapper = \"b\"\n");
    write(
        "config.toml",
        "include = [\"a.toml\", \"b.toml\"]\nbuild.rustc-wrapper = \"own\"\n",
    );
    assert_eq!(wrapper().unwrap(), found("own"));
    write("config.toml", "include = [\"a.toml\", \"b.toml\"]\n");
    assert_eq!(wrapper().unwrap(), found("b"));
    // An include that does not set it defers to the one before; includes nest; missing optional ones are skipped.
    write(
        "b.toml",
        "include = [{ path = \"c.toml\", optional = false }]\n",
    );
    write("c.toml", "[build]\njobs = 1\n");
    write(
        "config.toml",
        "include = [\"a.toml\", \"b.toml\", { path = \"missing.toml\", optional = true }]\n",
    );
    assert_eq!(wrapper().unwrap(), found("a"));
    write("c.toml", "build.rustc-wrapper = \"c\"\n");
    assert_eq!(wrapper().unwrap(), found("c"));
    write("config.toml", "include = [{ path = \"a.toml\" }]\n");
    assert_eq!(wrapper().unwrap(), found("a"));
    // `..` in an include is normalized, so a relative wrapper in it is relative to the directory holding the included file's directory.
    std::fs::write(
        directory.path().join("home/wrap.toml"),
        "build.rustc-wrapper = \"bin/w\"\n",
    )
    .unwrap();
    write("config.toml", "include = [\"../wrap.toml\"]\n");
    assert_eq!(
        wrapper().unwrap(),
        Some(directory.path().join("bin/w").into_os_string())
    );
    // Malformed or missing includes, cycles, and a `build` that is not a table are errors, as for Cargo.
    for (text, message) in [
        ("include = [\"missing.toml\"]\n", "missing.toml"),
        ("include = \"a.toml\"\n", "include must be an array"),
        ("include = [3]\n", "an include must be a path or a table"),
        (
            "include = [{ path = 3 }]\n",
            "an include needs a string path",
        ),
        (
            "include = [{ path = \"a.toml\", optional = \"yes\" }]\n",
            "an include needs a string path",
        ),
        ("include = [\"config.toml\"]\n", "includes itself"),
        ("include = [\"../.cargo/config.toml\"]\n", "includes itself"),
        ("include = [\"a.txt\"]\n", "must end with .toml"),
        ("build = 1\n", "build must be a table"),
        ("build = [\n", "line 1"),
    ] {
        write("config.toml", text);
        let error = wrapper().unwrap_err().to_string();
        assert!(error.contains(message), "{text}: {message}: {error}");
    }
}
