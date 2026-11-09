// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::artifact::{
    self, Artifact, ArtifactManifest, ArtifactRole, BuildIdentity, ConfigurationFile,
    DependencyIdentity, HelperArtifact, MANIFEST_SCHEMA, MANIFEST_VERSION, SourceIdentity,
    sha256_file, sha256_identified,
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
    /// Workspace binaries that roles run, built once from the baseline tree: those of the scenarios to be run.
    pub helpers: Vec<Helper>,
    pub cargo: PathBuf,
    pub rustc: PathBuf,
}

/// A workspace binary that a role of a multi-process scenario runs for every subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Helper {
    pub package: String,
    pub binary: String,
    /// Sorted and without duplicates.
    pub features: Vec<String>,
}

/// A workload package copied into every tree before building, so that revisions from before the workload existed, or with an older version of it, run the same harness.
///
/// Trees are never modified: each is copied to a sibling directory, so relative path dependencies still resolve, the package directory is replaced, and the copy is built without `--locked`, since the harness may need dependencies the tree's lockfile lacks. The copy's lockfile is first seeded with the current workspace's entries that the tree does not lock, so that the harness's new dependencies resolve to the current versions in every tree, without new choices from the registry. Where the harness needs a newer version of a package the tree locks, Cargo upgrades it for the whole tree; each artifact records such changes, and recorded campaigns refuse them.
///
/// The tree's workspace must list the package as a member, by name or by a glob, and must define what the package inherits with `workspace = true`. Inherited dependencies take the tree's versions and features, and the tree's `[patch]` applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlay {
    /// The package directory, relative to the workspace root.
    pub relative: PathBuf,
    /// The package directory to copy in.
    pub source: PathBuf,
    /// The current workspace's lockfile, whose entries seed each copy's.
    pub lockfile: Option<PathBuf>,
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
        let lockfile = Some(suite.workspace_root.join("Cargo.lock")).filter(|path| path.is_file());
        Ok(Self {
            relative,
            source,
            lockfile,
        })
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
            .and_then(|()| {
                self.lockfile.as_ref().map_or(Ok(()), |current| {
                    seed_lockfile(&copy.join("Cargo.lock"), current)
                })
            })
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
/// A layout replica copies the baseline to a sibling directory of the baseline tree, so relative path dependencies still resolve, builds it, and removes the copy. Rebuilding the same source at another path alone usually gives an identical executable, since Cargo identifies workspace members by their relative paths, so each replica's workspace crates are also built with a distinct `-C metadata` salt, which changes symbol hashes and therefore code layout. Comparing the baseline with its replicas shows how much a change of layout alone moves the results. A tree's feature sets share its target directory, which is removed once its last executable is frozen. Cancellation is checked between builds and again before the manifest is written: a cancelled preparation fails with `Interrupted` and writes no manifest, whether the build that was running finishes, as when only this process is signalled, or fails, as when a terminal's interrupt reaches Cargo too.
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
    // Builds with an overlay are compared with the current workspace's lockfile, which seeded theirs.
    let current_lockfile = options
        .overlay
        .as_ref()
        .and_then(|harness| harness.lockfile.as_deref());
    let mut artifacts = Vec::new();
    let mut helpers = Vec::new();
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
        let built = build_feature_sets(
            options,
            &feature_sets,
            &output,
            label,
            role,
            directory,
            &source,
            cancellation,
        )?;
        let built = with_lockfile_changes(built, path, directory, current_lockfile);
        // Helpers are built once, from the baseline tree as it was built, with its identity.
        if index == 0 {
            helpers = build_helpers(options, &output, directory, &built[0].source, cancellation)?;
        }
        artifacts.extend(built);
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
            lockfile_changes: Vec::new(),
        };
        let built = build_feature_sets(
            options,
            &feature_sets,
            &output,
            &label,
            ArtifactRole::LayoutReplica,
            &copy.0,
            &source,
            cancellation,
        )?;
        artifacts.extend(with_lockfile_changes(
            built,
            &baseline.path,
            &copy.0,
            current_lockfile,
        ));
    }

    let manifest = ArtifactManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        version: MANIFEST_VERSION,
        package: options.suite.package.clone(),
        binary: options.suite.binary.clone(),
        profile: options.suite.profile.clone(),
        artifacts,
        helpers,
        ..ArtifactManifest::default()
    };
    // A build that Cargo finished after cancellation, such as when only this process was signalled, leaves preparation as cancelled as one stopped between builds.
    if cancellation.is_cancelled() {
        return Err(cancelled());
    }
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

/// An exclusively created copy of a tree, for a layout replica or an overlay, removed when dropped, including after a failed build.
struct TemporaryCopy(PathBuf);

impl TemporaryCopy {
    fn create(path: PathBuf) -> io::Result<Self> {
        std::fs::create_dir(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "{}: {error}; a copy of a tree must be a new directory",
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
                package: &options.suite.package,
                binary: &options.suite.binary,
                tree,
                target: &target,
                features,
                // Each feature set's executable has its own directory, numbered in order.
                executable: output
                    .join("artifacts")
                    .join(label)
                    .join(index.to_string())
                    .join(&options.suite.binary),
                // A replica rebuilt at another path is usually identical to its baseline, since Cargo hashes workspace members by their relative paths; salting every crate's metadata changes symbol hashes and therefore code layout. The wrapper that adds it is a shell script.
                salt: (role == ArtifactRole::LayoutReplica && cfg!(unix))
                    .then(|| format!("base-benchmark-{label}")),
            };
            build.run(options, source.clone(), cancellation)
        })
        .collect::<io::Result<Vec<_>>>();
    // The frozen copies are all that collection uses; the build directory is only disk space.
    let _ = std::fs::remove_dir_all(&target);
    let mut artifacts = artifacts?;
    // Path dependencies are the tree's, whatever its features, so they are read once, after its builds have fetched what they need. A replica repeats its baseline's.
    if role != ArtifactRole::LayoutReplica {
        let platform = artifacts
            .first()
            .and_then(|artifact| host(&artifact.build.toolchain));
        let dependencies = match path_dependencies(&options.cargo, tree, platform, cancellation) {
            Ok(dependencies) => Some(dependencies),
            // Metadata that was not read because preparation was cancelled says nothing about the tree, so preparation fails as cancelled rather than recording the path dependencies as unknown.
            Err(error) if cancellation.is_cancelled() => {
                return Err(cancelled_by(format!("{label}: {error}")));
            }
            Err(error) => {
                // Recorded mode refuses unknown path dependencies, so say why now rather than when a campaign is refused.
                eprintln!(
                    "base-benchmark: warning: {label}: cannot read the Cargo metadata of {}, so its path dependencies are unknown and recorded campaigns will refuse it: {error}",
                    tree.display()
                );
                None
            }
        };
        for artifact in &mut artifacts {
            artifact.source.path_dependencies = dependencies.clone();
        }
    }
    Ok(artifacts)
}

/// Build every helper in `tree`, the baseline's directory, sharing a target directory of their own, removed afterwards, and freeze each under `helpers/N/`.
fn build_helpers(
    options: &PrepareOptions,
    output: &Path,
    tree: &Path,
    source: &SourceIdentity,
    cancellation: &Cancellation,
) -> io::Result<Vec<HelperArtifact>> {
    let target = output.join("targets").join(".helpers");
    let helpers = options
        .helpers
        .iter()
        .enumerate()
        .map(|(index, helper)| {
            let build = Build {
                label: &helper.binary,
                role: ArtifactRole::Helper,
                package: &helper.package,
                binary: &helper.binary,
                tree,
                target: &target,
                features: &helper.features,
                executable: output
                    .join("helpers")
                    .join(index.to_string())
                    .join(&helper.binary),
                salt: None,
            };
            build
                .run(options, source.clone(), cancellation)
                .map(|artifact| HelperArtifact {
                    package: helper.package.clone(),
                    binary: helper.binary.clone(),
                    artifact,
                })
        })
        .collect();
    let _ = std::fs::remove_dir_all(&target);
    helpers
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "preparation was cancelled")
}

/// The cancellation, with the failure it caused, whose text, such as the writer's or Cargo metadata's standard error, is in no other output.
fn cancelled_by(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        format!("preparation was cancelled ({error})"),
    )
}

/// One build of a tree with one feature set.
struct Build<'a> {
    label: &'a str,
    role: ArtifactRole,
    package: &'a str,
    binary: &'a str,
    tree: &'a Path,
    target: &'a Path,
    features: &'a [String],
    /// Where to freeze the executable.
    executable: PathBuf,
    /// Salt for the workspace's crate metadata, for layout replicas.
    salt: Option<String>,
}

impl Build<'_> {
    /// Build and freeze the executable, unless preparation was cancelled. A failure once it is cancelled is the cancellation's, which names it: a terminal's interrupt reaches the whole process group, so Cargo, or another program this runs, such as the writer or `rustc -Vv`, dies of it too.
    fn run(
        self,
        options: &PrepareOptions,
        source: SourceIdentity,
        cancellation: &Cancellation,
    ) -> io::Result<Artifact> {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        self.build(options, source).map_err(|error| {
            if cancellation.is_cancelled() {
                cancelled_by(error)
            } else {
                error
            }
        })
    }

    fn build(self, options: &PrepareOptions, mut source: SourceIdentity) -> io::Result<Artifact> {
        let Build {
            label,
            role,
            package,
            binary,
            tree,
            target,
            features,
            executable: frozen,
            salt,
        } = self;
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
            package.to_owned(),
            "--bin".to_owned(),
            binary.to_owned(),
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
        let configuration = configuration_files(
            tree,
            cargo_home(
                std::env::var_os("CARGO_HOME"),
                std::env::var_os("HOME"),
                tree,
            ),
        );
        let salted = salt.as_ref().map_or(Ok(()), |salt| {
            existing_wrapper(
                [
                    std::env::var_os("RUSTC_WRAPPER"),
                    std::env::var_os("CARGO_BUILD_RUSTC_WRAPPER"),
                ],
                &configuration,
                tree,
            )
            .and_then(|existing| salt_crates(&mut cargo, target, salt, existing))
        });
        let output_text = salted
            .and_then(|()| {
                cargo.output().map_err(|error| {
                    io::Error::new(error.kind(), format!("cannot run {}: {error}", command[0]))
                })
            })
            .map_err(|error| io::Error::new(error.kind(), format!("{label}: {error}")))?;
        if !output_text.status.success() {
            return Err(io::Error::other(format!(
                "{label}: cargo build failed with {}",
                output_text.status
            )));
        }
        let built =
            executable(&String::from_utf8_lossy(&output_text.stdout), binary).ok_or_else(|| {
                io::Error::other(format!(
                    "{label}: cargo reported no executable for {binary}"
                ))
            })?;
        let sha256 =
            std::fs::create_dir_all(frozen.parent().expect("an executable has a directory"))
                .and_then(|()| {
                    artifact::write_executable(&frozen, artifact::Contents::Copy(&built))
                })
                .and_then(|()| sha256_identified(&frozen))
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
                configuration,
                layout_salt: salt,
            },
        })
    }
}

/// The rustc wrapper a build of `tree` would use, which the salting wrapper must run in turn, by Cargo's precedence: `RUSTC_WRAPPER`, then `CARGO_BUILD_RUSTC_WRAPPER`, then `build.rustc-wrapper` in the first of `configuration`, the tree's Cargo configuration files from the nearest to `$CARGO_HOME`'s, that sets it, itself or through the files it includes. An empty value means none.
///
/// Paths containing `/` are resolved as Cargo resolves them: from the environment, against the tree, where Cargo runs; from a file, against the directory that holds its `.cargo` directory. A bare program name is found on `PATH` when it runs. As for Cargo, an environment value that is not UTF-8 is ignored.
pub(crate) fn existing_wrapper(
    environment: [Option<std::ffi::OsString>; 2],
    configuration: &[ConfigurationFile],
    tree: &Path,
) -> io::Result<Option<std::ffi::OsString>> {
    if let Some(value) = environment
        .iter()
        .flatten()
        .find_map(|value| value.to_str())
    {
        return Ok(wrapper_path(value, tree));
    }
    for file in configuration {
        // Cargo reads only `config` from a directory that also has `config.toml`.
        let shadowed = file.path.file_name() == Some("config.toml".as_ref())
            && configuration
                .iter()
                .any(|other| other.path == file.path.with_file_name("config"));
        if shadowed {
            continue;
        }
        if let Some(wrapper) = configured_wrapper(&file.path, &mut Vec::new())? {
            return Ok(wrapper);
        }
    }
    Ok(None)
}

/// A wrapper as a program to run: none when empty, a path containing `/` joined to `base`, or a bare program name.
fn wrapper_path(value: &str, base: &Path) -> Option<std::ffi::OsString> {
    match value {
        "" => None,
        value if value.contains('/') => Some(base.join(value).into_os_string()),
        value => Some(value.into()),
    }
}

/// `build.rustc-wrapper` set by one Cargo configuration file, or by the files it includes: `None` when none sets it, otherwise the wrapper, or `Some(None)` for an empty value.
///
/// As for Cargo, the file's own value wins over the files it includes, and a later include over an earlier one. Includes are relative to the including file, end with `.toml`, and are normalized lexically; a missing optional include is skipped. `including` holds the files that include this one, to report a cycle.
fn configured_wrapper(
    path: &Path,
    including: &mut Vec<PathBuf>,
) -> io::Result<Option<Option<std::ffi::OsString>>> {
    let invalid = |reason: String| invalid_input(format!("{}: {reason}", path.display()));
    if including.iter().any(|other| other == path) {
        return Err(invalid("includes itself".to_owned()));
    }
    let table: toml::Table = std::fs::read_to_string(path)
        .map_err(|error| invalid(error.to_string()))?
        .parse()
        .map_err(|error: toml::de::Error| invalid(error.to_string()))?;
    let wrapper = match table.get("build") {
        Some(toml::Value::Table(build)) => build.get("rustc-wrapper"),
        Some(_) => return Err(invalid("build must be a table".to_owned())),
        None => None,
    };
    if let Some(wrapper) = wrapper {
        let wrapper = wrapper
            .as_str()
            .ok_or_else(|| invalid("build.rustc-wrapper must be a string".to_owned()))?;
        // The file is `DIRECTORY/.cargo/config.toml`, or `$CARGO_HOME/config.toml`; paths are relative to DIRECTORY, or to `$CARGO_HOME`'s parent.
        let base = path.ancestors().nth(2).unwrap_or(Path::new("/"));
        return Ok(Some(wrapper_path(wrapper, base)));
    }
    let includes = match table.get("include") {
        Some(toml::Value::Array(includes)) => includes.as_slice(),
        Some(_) => return Err(invalid("include must be an array".to_owned())),
        None => &[],
    };
    let directory = path.parent().unwrap_or(Path::new("/"));
    including.push(path.to_owned());
    for include in includes.iter().rev() {
        let (included, optional) = match include {
            toml::Value::String(included) => (included.as_str(), false),
            toml::Value::Table(include) => match (include.get("path"), include.get("optional")) {
                (Some(toml::Value::String(included)), None) => (included.as_str(), false),
                (Some(toml::Value::String(included)), Some(toml::Value::Boolean(optional))) => {
                    (included.as_str(), *optional)
                }
                _ => {
                    return Err(invalid(
                        "an include needs a string path and a boolean optional".to_owned(),
                    ));
                }
            },
            _ => return Err(invalid("an include must be a path or a table".to_owned())),
        };
        if !included.ends_with(".toml") {
            return Err(invalid(format!("include {included:?} must end with .toml")));
        }
        // Normalized as Cargo normalizes it, so that `..` neither changes where relative paths in it lead nor hides a cycle.
        let included = crate::remote::normalized(&directory.join(included));
        if optional && !included.exists() {
            continue;
        }
        if let Some(wrapper) = configured_wrapper(&included, including)? {
            return Ok(Some(wrapper));
        }
    }
    including.pop();
    Ok(None)
}

/// Build every crate with `-C metadata=SALT` added, through a `RUSTC_WRAPPER` script that runs `existing`, the wrapper the build would otherwise use, in turn. `RUSTFLAGS` and Cargo configuration are untouched.
///
/// The script lives in `directory`, the replica's own target directory inside the new preparation output, so no other user can replace it; its name comes from the salt and its contents. Cargo does not hash a `RUSTC_WRAPPER`'s path, so the executable depends only on the salt and the wrapper it chains.
#[cfg(unix)]
pub(crate) fn salt_crates(
    cargo: &mut Command,
    directory: &Path,
    salt: &str,
    existing: Option<std::ffi::OsString>,
) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;
    let quote = |value: &[u8]| {
        let mut quoted = b"'".to_vec();
        for &byte in value {
            match byte {
                b'\'' => quoted.extend_from_slice(b"'\\''"),
                byte => quoted.push(byte),
            }
        }
        quoted.push(b'\'');
        quoted
    };
    let mut script = b"#!/bin/sh\nexec ".to_vec();
    if let Some(existing) = &existing {
        script.extend(quote(existing.as_bytes()));
        script.push(b' ');
    }
    script.extend_from_slice(b"\"$@\" -C metadata=");
    script.extend(quote(salt.as_bytes()));
    script.push(b'\n');
    let digest = format!("{:x}", Sha256::digest(&script));
    let wrapper = directory.join(format!("{salt}-{}", &digest[..16]));
    // Write a sibling and rename it into place, so a concurrent preparation never runs a partly written script.
    let partial = directory.join(format!(".{salt}-{}-{}", &digest[..16], std::process::id()));
    // Cargo runs it as soon as it is written, so it is written by a child process, never held open for writing here.
    std::fs::create_dir_all(directory)
        .and_then(|()| artifact::write_executable(&partial, artifact::Contents::Script(&script)))
        .and_then(|()| std::fs::rename(&partial, &wrapper))
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "{}: cannot write the layout salt wrapper: {error}",
                    wrapper.display()
                ),
            )
        })?;
    cargo.env("RUSTC_WRAPPER", wrapper);
    Ok(())
}

// Replicas are salted only on Unix, so this is never called elsewhere.
#[cfg(not(unix))]
pub(crate) fn salt_crates(
    _cargo: &mut Command,
    _directory: &Path,
    _salt: &str,
    _existing: Option<std::ffi::OsString>,
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

/// Path dependencies outside `tree`, each with its Git state, or Cargo's errors when its metadata cannot be read.
///
/// Metadata is filtered to the host platform when it is known, so it needs no packages beyond those a host build can use. It is read in the tree, as a build is, so the tree's Cargo configuration, such as source replacement or `net.offline`, applies. It is read offline first, and, if that fails, again with network access, still locked, unless preparation was cancelled: building one package downloads only what that package needs, so on a host with a fresh Cargo cache the workspace's other members can need packages that are not downloaded.
fn path_dependencies(
    cargo: &Path,
    tree: &Path,
    host: Option<&str>,
    cancellation: &Cancellation,
) -> Result<Vec<DependencyIdentity>, String> {
    let metadata = |offline: bool| {
        let mut command = Command::new(cargo);
        command.args(["metadata", "--format-version", "1", "--locked"]);
        if offline {
            command.arg("--offline");
        }
        command.arg("--manifest-path").arg(tree.join("Cargo.toml"));
        if let Some(host) = host {
            command.args(["--filter-platform", host]);
        }
        cargo_metadata(command.current_dir(tree))
    };
    let metadata = metadata(true).or_else(|offline| {
        // The caller reports the cancellation.
        if cancellation.is_cancelled() {
            return Err(format!("offline, {offline}"));
        }
        // Fetching can take a while, so it should not be a silent pause.
        eprintln!(
            "base-benchmark: reading the Cargo metadata of {} with network access, since reading it offline failed",
            tree.display()
        );
        metadata(false).map_err(|online| format!("offline, {offline}; online, {online}"))
    })?;
    let dependencies = external_path_packages(&metadata, tree)
        .into_iter()
        .map(|(name, path)| DependencyIdentity {
            revision: git(&path, &["rev-parse", "--verify", "HEAD"]),
            dirty: git(&path, &["status", "--porcelain"]).map(|status| !status.is_empty()),
            name,
            path,
        })
        .collect();
    Ok(dependencies)
}

/// Run `cargo metadata` and parse its output, or describe why that failed, with Cargo's own error.
fn cargo_metadata(command: &mut Command) -> Result<Value, String> {
    let output = command.stdin(Stdio::null()).output().map_err(|error| {
        format!(
            "cannot run {}: {error}",
            Path::new(command.get_program()).display()
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cargo metadata printed invalid JSON: {error}"))
}

/// The host target triple from `rustc -Vv`.
pub(crate) fn host(toolchain: &str) -> Option<&str> {
    toolchain
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
}

/// Cargo's home directory for a build in `tree`: `$CARGO_HOME`, relative to the tree, where Cargo runs, or `.cargo` in the home directory. An empty `$CARGO_HOME` counts as unset, as it does for Cargo.
pub(crate) fn cargo_home(
    cargo_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    tree: &Path,
) -> Option<PathBuf> {
    cargo_home
        .filter(|cargo_home| !cargo_home.is_empty())
        .map(|cargo_home| tree.join(cargo_home))
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
        lockfile_changes: Vec::new(),
    }
}

/// Record in each artifact what its build changed: versions `tree`'s lockfile locks that the lockfile of `built`, the directory that was built, changed, and, with an overlay, packages the tree did not lock that resolved differently from the current workspace. Locked builds change nothing.
fn with_lockfile_changes(
    mut artifacts: Vec<Artifact>,
    tree: &Path,
    built: &Path,
    current: Option<&Path>,
) -> Vec<Artifact> {
    // Lockfiles checked out with Windows line endings parse the same.
    let read = |path: &Path| {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .replace("\r\n", "\n")
    };
    let changes = lockfile_changes(
        &read(&tree.join("Cargo.lock")),
        &read(&built.join("Cargo.lock")),
        &current.map(read).unwrap_or_default(),
    );
    for artifact in &mut artifacts {
        artifact.source.lockfile_changes = changes.clone();
    }
    artifacts
}

/// A lockfile's `[[package]]` entries: each name and version, the names it depends on, and its text.
struct Locked<'a> {
    name: &'a str,
    version: &'a str,
    dependencies: Vec<&'a str>,
    text: &'a str,
}

fn locked_packages(text: &str) -> Vec<Locked<'_>> {
    text.split("[[package]]\n")
        .skip(1)
        .map(|entry| {
            let value = |key: &str| {
                entry
                    .lines()
                    .find_map(|line| line.strip_prefix(key)?.strip_prefix(" = "))
                    .unwrap_or_default()
                    .trim_matches('"')
            };
            // Dependencies are listed one per line, as a quoted name optionally followed by a version and source.
            let dependencies = entry
                .split_once("dependencies = [\n")
                .map(|(_, list)| list.split_once(']').map_or(list, |(list, _)| list))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.trim().trim_matches(['"', ',']).split(' ').next())
                .filter(|name| !name.is_empty())
                .collect();
            Locked {
                name: value("name"),
                version: value("version"),
                dependencies,
                text: entry,
            }
        })
        .collect()
}

fn versions<'a>(packages: &[Locked<'a>]) -> BTreeMap<&'a str, BTreeSet<&'a str>> {
    let mut versions = BTreeMap::<&str, BTreeSet<&str>>::new();
    for package in packages {
        versions
            .entry(package.name)
            .or_default()
            .insert(package.version);
    }
    versions
}

/// Add to the lockfile at `copy` the entries of `current` for packages it does not lock at all, so that Cargo resolves them as the current workspace does.
///
/// Packages the tree locks keep its versions, since a second, compatible version would leave Cargo unable to choose. An entry that depends, directly or through other added entries, on a package the tree locks at another version would contradict the tree's lockfile, so it is left for Cargo to resolve.
fn seed_lockfile(copy: &Path, current: &Path) -> io::Result<()> {
    let tree = std::fs::read_to_string(copy)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    let current = std::fs::read_to_string(current)?.replace("\r\n", "\n");
    let (locked, available) = (locked_packages(&tree), locked_packages(&current));
    let (tree_versions, current_versions) = (versions(&locked), versions(&available));
    // Names the seed cannot use: packages the tree locks differently, then whatever depends on them.
    let mut excluded: BTreeSet<&str> = tree_versions
        .iter()
        .filter(|(name, versions)| {
            current_versions
                .get(*name)
                .is_some_and(|current| current != *versions)
        })
        .map(|(name, _)| *name)
        .collect();
    let mut seeds: Vec<&Locked> = available
        .iter()
        .filter(|package| !tree_versions.contains_key(package.name))
        .collect();
    while let Some(index) = seeds.iter().position(|package| {
        package
            .dependencies
            .iter()
            .any(|dependency| excluded.contains(dependency))
    }) {
        excluded.insert(seeds.remove(index).name);
    }
    let mut seeded = tree.trim_end().to_owned();
    for package in seeds {
        seeded.push_str("\n\n[[package]]\n");
        seeded.push_str(package.text.trim_end());
    }
    seeded.push('\n');
    std::fs::write(copy, seeded)
}

/// What a build changed, as readable lines: each package the `original` lockfile locks that `built` uses at a version it did not lock, as `name before -> after`, and each package `built` adds whose versions differ from the `current` workspace's, as `name versions, not the current workspace's versions`. A package the build no longer uses, such as a dependency only an older workload had, changes nothing that is measured.
pub(crate) fn lockfile_changes(original: &str, built: &str, current: &str) -> Vec<String> {
    let (original, built, current) = (
        versions(&locked_packages(original)),
        versions(&locked_packages(built)),
        versions(&locked_packages(current)),
    );
    let join = |versions: &BTreeSet<&str>| versions.iter().copied().collect::<Vec<_>>().join(", ");
    let changed = original.iter().filter_map(|(name, before)| {
        let after = built.get(name).filter(|after| !after.is_subset(before))?;
        Some(format!("{name} {} -> {}", join(before), join(after)))
    });
    let drifted = built
        .iter()
        .filter(|(name, _)| !original.contains_key(*name))
        .filter_map(|(name, versions)| {
            let wanted = current.get(name).filter(|wanted| *wanted != versions)?;
            Some(format!(
                "{name} {}, not the current workspace's {}",
                join(versions),
                join(wanted)
            ))
        });
    changed.chain(drifted).collect()
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
/// Symbolic links are copied as links, so absolute links still point into the original tree. Build scripts that need `.git` see no repository. Files are written in this process, unlike frozen executables (see [`artifact::write_executable`]), so a program in the tree that the build runs, such as a rustc wrapper or linker given by a path into it, can be busy if another thread forks a child that never execs while the file is open.
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
