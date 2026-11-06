// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::artifact::{
    self, Artifact, ArtifactManifest, ArtifactRole, BuildIdentity, ConfigurationFile,
    DependencyIdentity, MANIFEST_SCHEMA, MANIFEST_VERSION, SourceIdentity, sha256_file,
};
use crate::config::{Suite, invalid_input, valid_label};
use crate::runner::Cancellation;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
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
    /// How many copies of the baseline to rebuild at other paths, each one sample of build layout. Defaults to one when comparing two or more trees, otherwise none.
    pub replicas: Option<usize>,
    /// A new directory that receives frozen executables and `artifacts.json`.
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
/// A layout replica copies the baseline to a sibling directory of the baseline tree, so relative path dependencies still resolve, builds it, and removes the copy. Rebuilding the same source at a different path changes Cargo's package metadata hashes and therefore code layout; comparing the baseline with its replicas shows how much a change of that kind alone moves the results. Each build's target directory is removed once its executable is frozen. Cancellation is checked between builds.
pub fn prepare(options: &PrepareOptions, cancellation: &Cancellation) -> io::Result<Prepared> {
    let (trees, replicas) = validate(options)?;
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
    let output = output
        .canonicalize()
        .expect("the new output directory exists");
    // Reserve every replica copy before building, so a concurrent preparation cannot claim the same directory.
    let copies = replicas
        .into_iter()
        .map(|(label, path)| TemporaryCopy::create(path).map(|copy| (label, copy)))
        .collect::<io::Result<Vec<_>>>()?;

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
            cancellation,
        )?);
    }
    for (label, copy) in copies {
        let baseline = &artifacts[0];
        copy_contents(&baseline.source.path, &copy.0, &output)?;
        let source = SourceIdentity {
            path: copy.0.clone(),
            revision: baseline.source.revision.clone(),
            dirty: baseline.source.dirty,
            lockfile_sha256: None,
            replica_of: Some(baseline.label.clone()),
            path_dependencies: Vec::new(),
        };
        artifacts.push(build(
            options,
            &output,
            &label,
            ArtifactRole::LayoutReplica,
            &copy.0,
            source,
            cancellation,
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

/// Canonical labelled trees, and each layout replica's label and copy location.
type Validated = (Vec<(String, PathBuf)>, Vec<(String, PathBuf)>);

/// Check labels and paths before building anything.
fn validate(options: &PrepareOptions) -> io::Result<Validated> {
    let Some(baseline) = options.trees.first() else {
        return Err(invalid_input("prepare needs at least one source tree"));
    };
    let count = options
        .replicas
        .unwrap_or(usize::from(options.trees.len() > 1));
    let replica_labels: Vec<String> = match count {
        1 => vec![format!("{}-replica", baseline.label)],
        _ => (1..=count)
            .map(|index| format!("{}-replica-{index}", baseline.label))
            .collect(),
    };
    let mut labels = BTreeSet::new();
    let mut trees = Vec::new();
    for tree in &options.trees {
        if !valid_label(&tree.label) || !labels.insert(tree.label.as_str()) {
            return Err(invalid_input(format!(
                "invalid or duplicate tree label {:?}",
                tree.label
            )));
        }
        if replica_labels.contains(&tree.label) {
            return Err(invalid_input(format!(
                "tree label {:?} is reserved for a layout replica",
                tree.label
            )));
        }
        let path = tree.path.canonicalize().map_err(|error| {
            io::Error::new(error.kind(), format!("{}: {error}", tree.path.display()))
        })?;
        trees.push((tree.label.clone(), path));
    }
    let replicas = replica_labels
        .into_iter()
        .map(|label| {
            sibling_copy_path(&trees[0].1, &label, &options.output).map(|path| (label, path))
        })
        .collect::<io::Result<_>>()?;
    Ok((trees, replicas))
}

/// An exclusively created replica source directory, removed when dropped, including after a failed build.
struct TemporaryCopy(PathBuf);

impl TemporaryCopy {
    fn create(path: PathBuf) -> io::Result<Self> {
        std::fs::create_dir(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "{}: {error}; a layout replica copy must be a new directory",
                    path.display()
                ),
            )
        })?;
        Ok(Self(path))
    }
}

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
    cancellation: &Cancellation,
) -> io::Result<Artifact> {
    if cancellation.is_cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "preparation was cancelled",
        ));
    }
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
    // The frozen copy is all that collection uses; the build directory is only disk space.
    let _ = std::fs::remove_dir_all(&target);
    source.lockfile_sha256 = sha256_file(&tree.join("Cargo.lock")).ok();
    if role != ArtifactRole::LayoutReplica {
        source.path_dependencies = path_dependencies(&options.cargo, tree);
    }
    let version = |program: &Path| {
        capture(Command::new(program).arg("-Vv").current_dir(tree)).ok_or_else(|| {
            io::Error::other(format!("{label}: could not run {} -Vv", program.display()))
        })
    };
    Ok(Artifact {
        label: label.to_owned(),
        role,
        executable,
        sha256,
        source,
        build: BuildIdentity {
            command,
            toolchain: version(&options.rustc)?,
            cargo: version(&options.cargo)?,
            environment: build_environment(std::env::vars()),
            configuration: configuration_files(
                tree,
                std::env::var_os("CARGO_HOME").map(PathBuf::from),
            ),
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

/// Variables that change what Cargo and rustc produce.
pub(crate) fn build_environment(
    variables: impl Iterator<Item = (String, String)>,
) -> BTreeMap<String, String> {
    const PREFIXES: [&str; 6] = [
        "RUSTFLAGS",
        "RUSTC",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_",
        "CARGO_PROFILE_",
        "CARGO_TARGET_",
    ];
    variables
        .filter(|(name, _)| PREFIXES.iter().any(|prefix| name.starts_with(prefix)))
        .collect()
}

/// Cargo configuration that applies to `tree`: `.cargo/config.toml` or `.cargo/config` in the tree and each ancestor, then `$CARGO_HOME`.
pub(crate) fn configuration_files(
    tree: &Path,
    cargo_home: Option<PathBuf>,
) -> Vec<ConfigurationFile> {
    let directories = tree
        .ancestors()
        .map(|directory| directory.join(".cargo"))
        .chain(cargo_home);
    let mut seen = BTreeSet::new();
    directories
        .flat_map(|directory| [directory.join("config.toml"), directory.join("config")])
        .filter(|path| seen.insert(path.clone()))
        .filter_map(|path| {
            sha256_file(&path)
                .ok()
                .map(|sha256| ConfigurationFile { path, sha256 })
        })
        .collect()
}

/// Path dependencies outside `tree`, each with its Git state. They are listed only when Cargo metadata can be read.
fn path_dependencies(cargo: &Path, tree: &Path) -> Vec<DependencyIdentity> {
    let metadata = capture(
        Command::new(cargo)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--offline",
                "--manifest-path",
            ])
            .arg(tree.join("Cargo.toml")),
    )
    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    .unwrap_or_default();
    external_path_packages(&metadata, tree)
        .into_iter()
        .map(|(name, path)| DependencyIdentity {
            revision: git(&path, &["rev-parse", "--verify", "HEAD"]),
            dirty: git(&path, &["status", "--porcelain"]).map(|status| !status.is_empty()),
            name,
            path,
        })
        .collect()
}

/// Packages without a registry or Git source whose directory is outside `tree`.
pub(crate) fn external_path_packages(metadata: &Value, tree: &Path) -> BTreeMap<String, PathBuf> {
    metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|package| package["source"].is_null())
        .filter_map(|package| {
            let directory = Path::new(package["manifest_path"].as_str()?)
                .parent()?
                .to_owned();
            (!directory.starts_with(tree)).then(|| {
                (
                    package["name"].as_str().unwrap_or_default().to_owned(),
                    directory,
                )
            })
        })
        .collect()
}

fn source_identity(path: &Path) -> SourceIdentity {
    SourceIdentity {
        path: path.to_owned(),
        revision: git(path, &["rev-parse", "--verify", "HEAD"]),
        dirty: git(path, &["status", "--porcelain"]).map(|status| !status.is_empty()),
        lockfile_sha256: None,
        replica_of: None,
        path_dependencies: Vec::new(),
    }
}

fn git(path: &Path, arguments: &[&str]) -> Option<String> {
    capture(Command::new("git").arg("-C").arg(path).args(arguments))
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

/// A hidden sibling of the baseline tree, unique to this preparation and process.
fn sibling_copy_path(tree: &Path, label: &str, output: &Path) -> io::Result<PathBuf> {
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
    Ok(parent.join(format!(
        ".{name}.base-benchmark-{label}-{unique}-{}",
        std::process::id()
    )))
}

/// Copy a source tree's contents into an existing directory, skipping version control, Cargo target directories (marked by `CACHEDIR.TAG`), and the preparation output.
///
/// Symbolic links are copied as links, so absolute links still point into the original tree. Build scripts that need `.git` see no repository.
pub(crate) fn copy_contents(from: &Path, to: &Path, exclude: &Path) -> io::Result<()> {
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
            (false, true) => std::fs::create_dir(&destination)
                .and_then(|()| copy_contents(&path, &destination, exclude)),
            (false, false) => std::fs::copy(&path, &destination).map(|_| ()),
        };
        copied.map_err(|error| {
            io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        })?;
    }
    Ok(())
}

fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(test)]
mod tests;
