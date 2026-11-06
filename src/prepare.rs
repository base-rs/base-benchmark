// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::artifact::{
    self, Artifact, ArtifactManifest, ArtifactRole, BuildIdentity, MANIFEST_SCHEMA,
    MANIFEST_VERSION, SourceIdentity, sha256_file,
};
use crate::config::{Suite, invalid_input, valid_label};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

/// A labelled source tree to build. Checking out revisions is the caller's responsibility; trees are never modified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    pub label: String,
    pub path: PathBuf,
}

impl FromStr for Tree {
    type Err = io::Error;

    /// Parse `label=path`.
    fn from_str(value: &str) -> io::Result<Self> {
        match value.split_once('=') {
            Some((label, path)) if !path.is_empty() => Ok(Self {
                label: label.to_owned(),
                path: PathBuf::from(path),
            }),
            _ => Err(invalid_input(format!("expected LABEL=PATH, got {value:?}"))),
        }
    }
}

/// Options for [`prepare`]. The first tree is the baseline.
#[derive(Clone, Debug)]
pub struct PrepareOptions {
    pub suite: Suite,
    pub trees: Vec<Tree>,
    /// Whether to rebuild a copy of the baseline at another path. Defaults to true when comparing two or more trees.
    pub replica: Option<bool>,
    /// A new directory that receives targets, frozen executables, and `artifacts.json`.
    pub output: PathBuf,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
}

/// The prepared manifest and where it was written.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Prepared {
    pub manifest_path: PathBuf,
    pub manifest: ArtifactManifest,
}

/// Build every tree with the same profile, features, and lockfile policy, then freeze each executable.
///
/// A layout replica copies the baseline to a sibling directory of the baseline tree, so relative path dependencies still resolve, builds it, and removes the copy. Rebuilding the same source at a different path changes Cargo's package metadata hashes and therefore code layout; comparing the baseline with its replica shows how much a change of that kind alone moves the results.
pub fn prepare(options: &PrepareOptions) -> io::Result<Prepared> {
    let (trees, replica) = validate(options)?;
    let output = std::path::absolute(&options.output)?;
    std::fs::create_dir_all(output.parent().unwrap_or(&output))
        .and_then(|()| std::fs::create_dir(&output))
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "{}: {error}; preparation needs a new directory",
                    output.display()
                ),
            )
        })?;

    let mut artifacts = Vec::new();
    for (index, (label, path)) in trees.iter().enumerate() {
        let role = if index == 0 {
            ArtifactRole::Baseline
        } else {
            ArtifactRole::Candidate
        };
        artifacts.push(build(
            options,
            &output,
            label,
            role,
            path,
            source_identity(path),
        )?);
    }
    if let Some(copy) = replica {
        let baseline = &artifacts[0];
        let copy = TemporaryCopy(copy);
        copy_tree(&baseline.source.path, &copy.0, &output)?;
        let source = SourceIdentity {
            path: copy.0.clone(),
            revision: baseline.source.revision.clone(),
            dirty: baseline.source.dirty,
            lockfile_sha256: None,
            replica_of: Some(baseline.label.clone()),
        };
        let label = format!("{}-replica", baseline.label);
        artifacts.push(build(
            options,
            &output,
            &label,
            ArtifactRole::LayoutReplica,
            &copy.0,
            source,
        )?);
    }

    let manifest = ArtifactManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        version: MANIFEST_VERSION,
        package: options.suite.package.clone(),
        binary: options.suite.binary.clone(),
        profile: options.suite.profile.clone(),
        features: options.suite.features.clone(),
        artifacts,
    };
    let manifest_path = artifact::manifest_path(&output);
    manifest.write(&manifest_path)?;
    Ok(Prepared {
        manifest_path,
        manifest,
    })
}

/// Check labels and paths before building anything. Returns canonical trees and the replica's copy location, if any.
/// Canonical labelled trees and, when requested, where the replica copy will be made.
type Validated = (Vec<(String, PathBuf)>, Option<PathBuf>);

fn validate(options: &PrepareOptions) -> io::Result<Validated> {
    let Some(baseline) = options.trees.first() else {
        return Err(invalid_input("prepare needs at least one source tree"));
    };
    let replica = options.replica.unwrap_or(options.trees.len() > 1);
    let replica_label = format!("{}-replica", baseline.label);
    let mut labels = BTreeSet::new();
    let mut trees = Vec::new();
    for tree in &options.trees {
        if !valid_label(&tree.label) || !labels.insert(tree.label.as_str()) {
            return Err(invalid_input(format!(
                "invalid or duplicate tree label {:?}",
                tree.label
            )));
        }
        if replica && tree.label == replica_label {
            return Err(invalid_input(format!(
                "tree label {replica_label:?} is reserved for the layout replica"
            )));
        }
        let path = tree.path.canonicalize().map_err(|error| {
            io::Error::new(error.kind(), format!("{}: {error}", tree.path.display()))
        })?;
        trees.push((tree.label.clone(), path));
    }
    if !replica {
        return Ok((trees, None));
    }
    let copy = sibling_copy_path(&trees[0].1, &options.output)?;
    if copy.exists() {
        return Err(invalid_input(format!(
            "{} already exists; remove the stale layout replica copy",
            copy.display()
        )));
    }
    Ok((trees, Some(copy)))
}

/// Removes the replica's source copy, including after a failed build.
struct TemporaryCopy(PathBuf);

impl Drop for TemporaryCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn build(
    options: &PrepareOptions,
    output: &Path,
    label: &str,
    role: ArtifactRole,
    tree: &Path,
    mut source: SourceIdentity,
) -> io::Result<Artifact> {
    let suite = &options.suite;
    let target = output.join("targets").join(label);
    let mut command = vec![
        options.cargo.display().to_string(),
        "build".to_owned(),
        "--locked".to_owned(),
        "--message-format=json-render-diagnostics".to_owned(),
        "--manifest-path".to_owned(),
        tree.join("Cargo.toml").display().to_string(),
        "--package".to_owned(),
        suite.package.clone(),
        "--bin".to_owned(),
        suite.binary.clone(),
        "--profile".to_owned(),
        suite.profile.clone(),
        "--target-dir".to_owned(),
        target.display().to_string(),
    ];
    if !suite.features.is_empty() {
        command.extend(["--features".to_owned(), suite.features.join(",")]);
    }
    eprintln!("base-benchmark: building {label} from {}", tree.display());
    let result = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(tree)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output();
    let output_text = result.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("{label}: cannot run {}: {error}", command[0]),
        )
    })?;
    if !output_text.status.success() {
        return Err(io::Error::other(format!(
            "{label}: cargo build failed with {}",
            output_text.status
        )));
    }
    let built = executable(&String::from_utf8_lossy(&output_text.stdout), &suite.binary)
        .ok_or_else(|| {
            io::Error::other(format!(
                "{label}: cargo reported no executable for {}",
                suite.binary
            ))
        })?;
    let executable = output.join("artifacts").join(label).join(&suite.binary);
    let sha256 = std::fs::create_dir_all(output.join("artifacts").join(label))
        .and_then(|()| std::fs::copy(&built, &executable))
        .and_then(|_| sha256_file(&executable))
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("{label}: cannot freeze {}: {error}", built.display()),
            )
        })?;
    source.lockfile_sha256 = sha256_file(&tree.join("Cargo.lock")).ok();
    let toolchain =
        capture(Command::new(&options.rustc).arg("-Vv").current_dir(tree)).ok_or_else(|| {
            io::Error::other(format!(
                "{label}: could not run {} -Vv",
                options.rustc.display()
            ))
        })?;
    Ok(Artifact {
        label: label.to_owned(),
        role,
        executable,
        sha256,
        source,
        build: BuildIdentity {
            command,
            toolchain,
            rustflags: std::env::var("RUSTFLAGS").ok(),
            encoded_rustflags: std::env::var("CARGO_ENCODED_RUSTFLAGS").ok(),
        },
    })
}

/// Find the executable Cargo reported for `binary`, wherever the configured target directory layout put it.
pub(crate) fn executable(messages: &str, binary: &str) -> Option<PathBuf> {
    messages
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|message| {
            let is_binary = message["reason"] == "compiler-artifact"
                && message["target"]["name"] == binary
                && message["target"]["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"));
            message["executable"]
                .as_str()
                .filter(|_| is_binary)
                .map(PathBuf::from)
        })
}

fn source_identity(path: &Path) -> SourceIdentity {
    let git = |arguments: &[&str]| capture(Command::new("git").arg("-C").arg(path).args(arguments));
    SourceIdentity {
        path: path.to_owned(),
        revision: git(&["rev-parse", "--verify", "HEAD"]),
        dirty: git(&["status", "--porcelain"]).map(|status| !status.is_empty()),
        lockfile_sha256: None,
        replica_of: None,
    }
}

/// Run a command and return its trimmed stdout, or `None` if it fails.
fn capture(command: &mut Command) -> Option<String> {
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn sibling_copy_path(tree: &Path, output: &Path) -> io::Result<PathBuf> {
    let parent = tree.parent().ok_or_else(|| {
        invalid_input("the baseline tree has no parent directory for its layout replica")
    })?;
    let name = tree
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unique = output
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(parent.join(format!(".{name}.base-benchmark-replica-{unique}")))
}

/// Copy a source tree, skipping version control, Cargo target directories (marked by `CACHEDIR.TAG`), and the preparation output.
pub(crate) fn copy_tree(from: &Path, to: &Path, exclude: &Path) -> io::Result<()> {
    let entries = std::fs::read_dir(from)
        .and_then(|entries| {
            entries
                .map(|entry| {
                    entry.and_then(|entry| {
                        entry
                            .file_type()
                            .map(|kind| (entry.path(), entry.file_name(), kind))
                    })
                })
                .collect::<io::Result<Vec<_>>>()
        })
        .and_then(|entries| std::fs::create_dir(to).map(|()| entries))
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", from.display())))?;
    for (path, name, kind) in entries {
        let destination = to.join(&name);
        let skipped = kind.is_dir()
            && (name == ".git" || path == exclude || path.join("CACHEDIR.TAG").exists());
        let copied = match (kind.is_symlink(), kind.is_dir()) {
            _ if skipped => Ok(()),
            (true, _) => {
                std::fs::read_link(&path).and_then(|target| symlink(&target, &destination))
            }
            (false, true) => copy_tree(&path, &destination, exclude),
            (false, false) => std::fs::copy(&path, &destination).map(|_| ()),
        };
        copied.map_err(|error| {
            io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(target: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{}: copying symbolic links requires Unix", target.display()),
    ))
}

#[cfg(test)]
mod tests;
