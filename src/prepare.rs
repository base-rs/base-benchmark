// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::artifact::{
    self, Artifact, ArtifactManifest, ArtifactRole, BuildIdentity, ConfigurationFile,
    DependencyIdentity, MANIFEST_SCHEMA, MANIFEST_VERSION, SourceIdentity, sha256_file,
    sha256_identified,
};
use crate::config::{Suite, canonical_features, invalid_input, valid_label};
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
    /// How many copies of the baseline to rebuild at other paths, with salted crate metadata, each one sample of build layout. Defaults to one when comparing two or more trees, otherwise none.
    pub replicas: Option<usize>,
    /// A new directory that receives frozen executables and `artifacts.json`.
    pub output: PathBuf,
    /// The feature sets to build each tree with, each sorted and without duplicates: those of the scenarios to be run. Empty means the suite's features alone.
    pub feature_sets: Vec<Vec<String>>,
    /// Build every tree with this workload package in place of the tree's own.
    pub overlay: Option<Overlay>,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
}

/// A workload package copied into every tree before building, so that revisions from before the workload existed, or with an older version of it, run the same harness.
///
/// Trees are never modified: each is copied to a sibling directory, so relative path dependencies still resolve, the package directory is replaced, and the copy is built without `--locked`, since the harness may add dependencies the tree's lockfile lacks. Cargo keeps the tree's locked versions where they satisfy the harness, and each artifact records the lockfile it was built with. The tree's workspace must list the package as a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlay {
    /// The package directory, relative to the workspace root.
    pub relative: PathBuf,
    /// The package directory to copy in.
    pub source: PathBuf,
}

impl Overlay {
    /// The workload package of `suite`, which must be a member below its workspace root.
    pub fn of(suite: &Suite) -> io::Result<Self> {
        let source = suite
            .manifest_path
            .parent()
            .unwrap_or(&suite.manifest_path)
            .to_owned();
        let relative = source
            .strip_prefix(&suite.workspace_root)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .ok_or_else(|| {
                invalid_input(format!(
                    "{} is not in a directory below its workspace root, so it cannot be overlaid onto other trees",
                    suite.package
                ))
            })?
            .to_owned();
        Ok(Self { relative, source })
    }

    fn identity(&self, package: &str) -> DependencyIdentity {
        DependencyIdentity {
            name: package.to_owned(),
            path: self.source.clone(),
            revision: git(&self.source, &["rev-parse", "--verify", "HEAD"]),
            // Only the package's own files are copied, so only their state matters.
            dirty: git(&self.source, &["status", "--porcelain", "--", "."])
                .map(|status| !status.is_empty()),
        }
    }

    /// Replace the package directory of the tree copied to `copy`.
    fn apply(&self, copy: &Path, output: &Path) -> io::Result<()> {
        let package = copy.join(&self.relative);
        let _ = std::fs::remove_dir_all(&package);
        std::fs::create_dir_all(&package)
            .and_then(|()| copy_contents(&self.source, &package, output))
    }
}

/// The prepared manifest and where it was written.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Prepared {
    pub manifest_path: PathBuf,
    pub manifest: ArtifactManifest,
}

/// Build every tree with the same profile and lockfile policy, once for each feature set, then freeze each executable.
///
/// A layout replica copies the baseline to a sibling directory of the baseline tree, so relative path dependencies still resolve, builds it, and removes the copy. Rebuilding the same source at another path alone usually gives an identical executable, since Cargo identifies workspace members by their relative paths, so each replica's workspace crates are also built with a distinct `-C metadata` salt, which changes symbol hashes and therefore code layout. Comparing the baseline with its replicas shows how much a change of layout alone moves the results. A tree's feature sets share its target directory, which is removed once its last executable is frozen. Cancellation is checked between builds.
pub fn prepare(options: &PrepareOptions, cancellation: &Cancellation) -> io::Result<Prepared> {
    let (trees, copies) = validate(options)?;
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
    // Reserve every copy before building, so a concurrent preparation cannot claim the same directory.
    let mut copies = copies
        .into_iter()
        .map(|(label, path)| TemporaryCopy::create(path).map(|copy| (label, copy)))
        .collect::<io::Result<Vec<_>>>()?;
    // Overlaid copies of the trees follow the replicas, one per tree.
    let mut overlaid = copies
        .split_off(
            copies.len()
                - if options.overlay.is_some() {
                    trees.len()
                } else {
                    0
                },
        )
        .into_iter();

    // Canonical sets, without duplicates, so that each matches its scenarios and each tree is built once per set.
    let mut feature_sets: Vec<Vec<String>> = Vec::new();
    let requested = if options.feature_sets.is_empty() {
        std::slice::from_ref(&options.suite.features)
    } else {
        options.feature_sets.as_slice()
    };
    for features in requested
        .iter()
        .map(|features| canonical_features(features))
    {
        if !feature_sets.contains(&features) {
            feature_sets.push(features);
        }
    }
    let overlay = options
        .overlay
        .as_ref()
        .map(|overlay| overlay.identity(&options.suite.package));
    let mut artifacts = Vec::new();
    for (index, (label, path)) in trees.iter().enumerate() {
        let role = if index == 0 {
            ArtifactRole::Baseline
        } else {
            ArtifactRole::Candidate
        };
        let source = SourceIdentity {
            overlay: overlay.clone(),
            ..source_identity(path)
        };
        // With an overlay, the tree is built from its overlaid copy, removed when this iteration ends.
        let copy = overlaid.next().map(|(_, copy)| copy);
        let directory = copy
            .as_ref()
            .map_or(path.as_path(), |copy| copy.0.as_path());
        if directory != path.as_path() {
            overlay_copy(path, directory, options, &output)?;
        }
        artifacts.extend(build_feature_sets(
            options,
            &feature_sets,
            &output,
            label,
            role,
            directory,
            &source,
            cancellation,
        )?);
    }
    // The baseline's source identity is the same for each of its feature sets.
    let baseline = artifacts[0].source.clone();
    for (label, copy) in copies {
        overlay_copy(&baseline.path, &copy.0, options, &output)?;
        let source = SourceIdentity {
            path: copy.0.clone(),
            revision: baseline.revision.clone(),
            dirty: baseline.dirty,
            lockfile_sha256: None,
            replica_of: Some(trees[0].0.clone()),
            path_dependencies: baseline.path_dependencies.clone(),
            overlay: baseline.overlay.clone(),
        };
        artifacts.extend(build_feature_sets(
            options,
            &feature_sets,
            &output,
            &label,
            ArtifactRole::LayoutReplica,
            &copy.0,
            &source,
            cancellation,
        )?);
    }

    let manifest = ArtifactManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        version: MANIFEST_VERSION,
        package: options.suite.package.clone(),
        binary: options.suite.binary.clone(),
        profile: options.suite.profile.clone(),
        artifacts,
        ..ArtifactManifest::default()
    };
    let manifest_path = artifact::manifest_path(&output);
    manifest.write(&manifest_path)?;
    Ok(Prepared {
        manifest_path,
        manifest,
    })
}

/// Canonical labelled trees, and the label and location of each copy to build: layout replicas of the baseline, then, with an overlay, each tree's overlaid copy.
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
    // Each copy is a sibling of the tree it copies: replicas copy the baseline.
    let replicas = replica_labels.into_iter().map(|label| (label, &trees[0].1));
    let overlays = trees
        .iter()
        .filter(|_| options.overlay.is_some())
        .map(|(label, path)| (format!("{label}-overlay"), path));
    let copies = replicas
        .chain(overlays)
        .map(|(label, tree)| {
            sibling_copy_path(tree, &label, &options.output).map(|path| (label, path))
        })
        .collect::<io::Result<_>>()?;
    Ok((trees, copies))
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

/// Build one tree for each feature set, sharing its target directory, then remove the directory.
#[allow(clippy::too_many_arguments)]
fn build_feature_sets(
    options: &PrepareOptions,
    feature_sets: &[Vec<String>],
    output: &Path,
    label: &str,
    role: ArtifactRole,
    tree: &Path,
    source: &SourceIdentity,
    cancellation: &Cancellation,
) -> io::Result<Vec<Artifact>> {
    let target = output.join("targets").join(label);
    let artifacts = feature_sets
        .iter()
        .enumerate()
        .map(|(index, features)| {
            let build = Build {
                label,
                role,
                tree,
                target: &target,
                features,
                // Each feature set's executable has its own directory, numbered in order.
                executable: output
                    .join("artifacts")
                    .join(label)
                    .join(index.to_string())
                    .join(&options.suite.binary),
                // A replica rebuilt at another path is usually identical to its baseline, since Cargo hashes workspace members by their relative paths; salting their metadata changes symbol hashes and therefore code layout. The wrapper that adds it is a shell script.
                salt: (role == ArtifactRole::LayoutReplica && cfg!(unix))
                    .then(|| format!("base-benchmark-{label}")),
            };
            build.run(options, source.clone(), cancellation)
        })
        .collect();
    // The frozen copies are all that collection uses; the build directory is only disk space.
    let _ = std::fs::remove_dir_all(&target);
    artifacts
}

/// One build of a tree with one feature set.
struct Build<'a> {
    label: &'a str,
    role: ArtifactRole,
    tree: &'a Path,
    target: &'a Path,
    features: &'a [String],
    /// Where to freeze the executable.
    executable: PathBuf,
    /// Salt for the workspace's crate metadata, for layout replicas.
    salt: Option<String>,
}

impl Build<'_> {
    fn run(
        self,
        options: &PrepareOptions,
        mut source: SourceIdentity,
        cancellation: &Cancellation,
    ) -> io::Result<Artifact> {
        let Build {
            label,
            role,
            tree,
            target,
            features,
            executable: frozen,
            salt,
        } = self;
        if cancellation.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "preparation was cancelled",
            ));
        }
        let suite = &options.suite;
        let mut command = vec![options.cargo.display().to_string(), "build".to_owned()];
        // An overlaid harness may need dependencies the tree's lockfile lacks.
        if options.overlay.is_none() {
            command.push("--locked".to_owned());
        }
        command.extend([
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
        ]);
        if !features.is_empty() {
            command.extend(["--features".to_owned(), features.join(",")]);
        }
        eprintln!(
            "base-benchmark: building {label} from {}{}",
            tree.display(),
            if features.is_empty() {
                String::new()
            } else {
                format!(" with features {}", features.join(","))
            }
        );
        let mut cargo = Command::new(&command[0]);
        cargo
            .args(&command[1..])
            .current_dir(tree)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit());
        if let Some(salt) = &salt {
            salt_workspace(
                &mut cargo,
                target,
                salt,
                std::env::var("RUSTC_WORKSPACE_WRAPPER").ok(),
            )?;
        }
        let result = cargo.output();
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
        let sha256 =
            std::fs::create_dir_all(frozen.parent().expect("an executable has a directory"))
                .and_then(|()| std::fs::copy(&built, &frozen))
                .and_then(|_| sha256_identified(&frozen))
                .map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("{label}: cannot freeze {}: {error}", built.display()),
                    )
                })?;
        source.lockfile_sha256 = sha256_file(&tree.join("Cargo.lock")).ok();
        let version = |program: &Path| {
            capture(Command::new(program).arg("-Vv").current_dir(tree)).ok_or_else(|| {
                io::Error::other(format!("{label}: could not run {} -Vv", program.display()))
            })
        };
        let toolchain = version(&options.rustc)?;
        if role != ArtifactRole::LayoutReplica {
            source.path_dependencies = path_dependencies(&options.cargo, tree, host(&toolchain));
        }
        Ok(Artifact {
            label: label.to_owned(),
            role,
            features: Some(features.to_vec()),
            executable: frozen,
            sha256,
            source,
            build: BuildIdentity {
                command,
                toolchain,
                cargo: version(&options.cargo)?,
                environment: build_environment(std::env::vars()),
                configuration: configuration_files(
                    tree,
                    cargo_home(std::env::var_os("CARGO_HOME"), std::env::var_os("HOME")),
                ),
                layout_salt: salt,
            },
        })
    }
}

/// Build the workspace's own crates with `-C metadata=SALT` added, through a `RUSTC_WORKSPACE_WRAPPER` script in the target directory. A wrapper already in the environment still runs, before the salt is added; `RUSTFLAGS` and Cargo configuration are untouched.
#[cfg(unix)]
pub(crate) fn salt_workspace(
    cargo: &mut Command,
    target: &Path,
    salt: &str,
    existing: Option<String>,
) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let chained = existing
        .map(|wrapper| format!("{} ", quote(&wrapper)))
        .unwrap_or_default();
    let wrapper = target.join("base-benchmark-layout-salt");
    std::fs::create_dir_all(target)
        .and_then(|()| {
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nexec {chained}\"$@\" -C metadata={}\n",
                    quote(salt)
                ),
            )
        })
        .and_then(|()| std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)))
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "{}: cannot write the layout salt wrapper: {error}",
                    wrapper.display()
                ),
            )
        })?;
    cargo.env("RUSTC_WORKSPACE_WRAPPER", wrapper);
    Ok(())
}

// Replicas are salted only on Unix, so this is never called elsewhere.
#[cfg(not(unix))]
pub(crate) fn salt_workspace(
    _cargo: &mut Command,
    _target: &Path,
    _salt: &str,
    _existing: Option<String>,
) -> io::Result<()> {
    Ok(())
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

/// Path dependencies outside `tree`, each with its Git state, or `None` when Cargo metadata cannot be read.
///
/// Metadata is filtered to the host platform when it is known, so it needs no packages beyond those a host build can use.
fn path_dependencies(
    cargo: &Path,
    tree: &Path,
    host: Option<&str>,
) -> Option<Vec<DependencyIdentity>> {
    let mut command = Command::new(cargo);
    command
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--manifest-path",
        ])
        .arg(tree.join("Cargo.toml"));
    if let Some(host) = host {
        command.args(["--filter-platform", host]);
    }
    let metadata =
        capture(&mut command).and_then(|text| serde_json::from_str::<Value>(&text).ok())?;
    let dependencies = external_path_packages(&metadata, tree)
        .into_iter()
        .map(|(name, path)| DependencyIdentity {
            revision: git(&path, &["rev-parse", "--verify", "HEAD"]),
            dirty: git(&path, &["status", "--porcelain"]).map(|status| !status.is_empty()),
            name,
            path,
        })
        .collect();
    Some(dependencies)
}

/// The host target triple from `rustc -Vv`.
pub(crate) fn host(toolchain: &str) -> Option<&str> {
    toolchain
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
}

/// Cargo's home directory: `$CARGO_HOME`, or `.cargo` in the home directory.
pub(crate) fn cargo_home(
    cargo_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    cargo_home
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".cargo")))
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
        path_dependencies: None,
        overlay: None,
    }
}

/// Copy `tree` into the reserved directory `copy`, then overlay the workload package, if any, onto the copy.
fn overlay_copy(
    tree: &Path,
    copy: &Path,
    options: &PrepareOptions,
    output: &Path,
) -> io::Result<()> {
    copy_contents(tree, copy, output).and_then(|()| {
        options
            .overlay
            .as_ref()
            .map_or(Ok(()), |harness| harness.apply(copy, output))
    })
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

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

// Windows distinguishes file and directory links, and creating either needs a privilege, so replicas of trees with links need Unix.
#[cfg(not(unix))]
fn symlink(_target: &Path, link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{}: copying symbolic links needs Unix", link.display()),
    ))
}

#[cfg(test)]
mod tests;
