// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::config::invalid_input;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MANIFEST_SCHEMA: &str = "base-benchmark/artifacts";
/// Version 2 records each artifact's features, so one manifest can hold several builds of each tree. Version 1 manifests embedded in older campaigns can still be read, so those campaigns can be summarized again, but preparation must be repeated to run them.
pub const MANIFEST_VERSION: u32 = 2;

/// Frozen executables prepared before any measurement, with the identity of each source tree and build.
///
/// A tree is built once for each set of features its scenarios need, so a label names one tree and appears once per feature set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub schema: String,
    pub version: u32,
    pub package: String,
    pub binary: String,
    pub profile: String,
    pub artifacts: Vec<Artifact>,
    /// Version 1 recorded one feature set for every artifact. It is accepted only so that campaigns embedding a version 1 manifest still parse.
    #[serde(default, rename = "features", skip_serializing)]
    pub(crate) legacy_features: Option<Vec<String>>,
}

impl ArtifactManifest {
    pub fn read(path: &Path) -> io::Result<Self> {
        let text = std::fs::read(path).map_err(|error| {
            io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        })?;
        let manifest: Self = serde_json::from_slice(&text)
            .map_err(|error| invalid_input(format!("{}: {error}", path.display())))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn write(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        write_json(path, self)
    }

    /// Describe an executable built outside [`prepare`](fn@crate::prepare), such as a test binary, as the only baseline artifact.
    ///
    /// Its source and build are unknown, so it can be used for smoke and screening runs but never satisfies recorded prerequisites. Its features are unknown too, so it serves every scenario; callers choose which scenarios it can run.
    pub fn unprepared(suite: &crate::config::Suite, executable: &Path) -> io::Result<Self> {
        Ok(Self {
            schema: MANIFEST_SCHEMA.to_owned(),
            version: MANIFEST_VERSION,
            package: suite.package.clone(),
            binary: suite.binary.clone(),
            profile: "unknown".to_owned(),
            legacy_features: None,
            artifacts: vec![Artifact {
                label: "unprepared".to_owned(),
                role: ArtifactRole::Baseline,
                features: None,
                sha256: sha256_identified(executable)?,
                executable: executable.to_owned(),
                source: SourceIdentity {
                    path: executable.to_owned(),
                    revision: None,
                    dirty: None,
                    lockfile_sha256: None,
                    replica_of: None,
                    path_dependencies: None,
                    overlay: None,
                    lockfile_changes: Vec::new(),
                },
                build: BuildIdentity {
                    command: Vec::new(),
                    toolchain: "unknown".to_owned(),
                    cargo: "unknown".to_owned(),
                    environment: BTreeMap::new(),
                    configuration: Vec::new(),
                    layout_salt: None,
                },
            }],
        })
    }

    /// The baseline. Call it on a manifest of one feature set, such as one from [`Self::for_features`].
    pub fn baseline(&self) -> &Artifact {
        self.artifacts
            .iter()
            .find(|artifact| artifact.role == ArtifactRole::Baseline)
            .expect("validated manifests have a baseline")
    }

    /// The artifacts built with exactly `features`, sorted and without duplicates, as a manifest of their own; artifacts whose features are unknown serve any.
    pub fn for_features(&self, features: &[String]) -> io::Result<Self> {
        let artifacts: Vec<Artifact> = self
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact
                    .features
                    .as_deref()
                    .is_none_or(|built| built == features)
            })
            .cloned()
            .collect();
        if artifacts.is_empty() {
            return Err(invalid_input(format!(
                "no artifact was built with features {features:?}; prepare the scenarios that need them"
            )));
        }
        Ok(Self {
            artifacts,
            ..self.clone()
        })
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema != MANIFEST_SCHEMA || self.version != MANIFEST_VERSION {
            let advice = if self.version < MANIFEST_VERSION {
                "; prepare the trees again"
            } else {
                ""
            };
            return Err(invalid_input(format!(
                "unsupported artifact manifest {} version {}{advice}",
                self.schema, self.version
            )));
        }
        // Unknown features serve every scenario, so they cannot be mixed with known ones without giving one plan two baselines.
        if self
            .artifacts
            .iter()
            .any(|artifact| artifact.features.is_none())
            && self
                .artifacts
                .iter()
                .any(|artifact| artifact.features.is_some())
        {
            return Err(invalid_input(
                "an artifact manifest cannot mix artifacts with unknown features and known ones",
            ));
        }
        let mut builds = BTreeMap::<Option<&[String]>, Vec<&Artifact>>::new();
        for artifact in &self.artifacts {
            builds
                .entry(artifact.features.as_deref())
                .or_default()
                .push(artifact);
        }
        for artifacts in builds.values() {
            let labels: BTreeSet<&str> = artifacts
                .iter()
                .map(|artifact| artifact.label.as_str())
                .collect();
            if labels.len() != artifacts.len()
                || labels
                    .iter()
                    .any(|label| !crate::config::valid_label(label))
            {
                return Err(invalid_input(
                    "artifact labels must be valid and unique within each feature set",
                ));
            }
            if artifacts
                .iter()
                .filter(|artifact| artifact.role == ArtifactRole::Baseline)
                .count()
                != 1
            {
                return Err(invalid_input(
                    "an artifact manifest needs exactly one baseline for each feature set",
                ));
            }
            for artifact in artifacts {
                let replica_of = artifact.source.replica_of.as_deref();
                let consistent = match artifact.role {
                    ArtifactRole::LayoutReplica => replica_of
                        .is_some_and(|label| labels.contains(label) && label != artifact.label),
                    _ => replica_of.is_none(),
                };
                if !consistent {
                    return Err(invalid_input(format!(
                        "{}: only layout replicas name an existing replicated artifact with the same features",
                        artifact.label
                    )));
                }
            }
        }
        // A label names one tree, with the same role and source, and every feature set builds the same trees.
        let mut trees = BTreeMap::<&str, (ArtifactRole, &Path, Option<&str>)>::new();
        for artifact in &self.artifacts {
            let tree = (
                artifact.role,
                artifact.source.path.as_path(),
                artifact.source.replica_of.as_deref(),
            );
            if *trees.entry(&artifact.label).or_insert(tree) != tree {
                return Err(invalid_input(format!(
                    "{}: a label must name the same tree, with the same role, in every feature set",
                    artifact.label
                )));
            }
        }
        // Labels are unique within each set, so equal counts mean equal labels.
        if builds
            .values()
            .any(|artifacts| artifacts.len() != trees.len())
        {
            return Err(invalid_input("every feature set must build the same trees"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub label: String,
    pub role: ArtifactRole,
    /// The Cargo features it was built with, sorted and without duplicates, or `None` when unknown, as for an executable built outside preparation or recorded by a version 1 manifest.
    #[serde(default)]
    pub features: Option<Vec<String>>,
    /// The frozen copy of the executable, outside any Cargo target directory.
    pub executable: PathBuf,
    pub sha256: String,
    pub source: SourceIdentity,
    pub build: BuildIdentity,
}

impl Artifact {
    /// Fail if the frozen executable changed after preparation.
    pub fn verify(&self) -> io::Result<()> {
        self.verify_at(&self.executable)
    }

    /// Fail if `path`, such as a staged link or copy, does not contain the frozen executable.
    ///
    /// Each sample verifies its executable, so a file this process has already hashed is not read again while its identity is unchanged; see [`sha256_identified`].
    pub fn verify_at(&self, path: &Path) -> io::Result<()> {
        let actual = sha256_identified(path)?;

        if actual != self.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: executable changed after preparation", self.label),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactRole {
    Baseline,
    Candidate,
    /// The baseline's source copied to another path and rebuilt with salted crate metadata, exposing build-layout sensitivity.
    LayoutReplica,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    /// The tree that was built: the labelled tree, also when it was built from an overlaid copy, or a layout replica's copy.
    pub path: PathBuf,
    /// `git rev-parse HEAD`, when available.
    pub revision: Option<String>,
    /// Whether `git status --porcelain` reported changes, when available.
    pub dirty: Option<bool>,
    pub lockfile_sha256: Option<String>,
    pub replica_of: Option<String>,
    /// Path dependencies outside the tree. `Cargo.lock` does not pin them, so each is identified by its own Git state. `None` when Cargo metadata could not be read, so they are unknown and the source is not frozen. A replica repeats its baseline's.
    pub path_dependencies: Option<Vec<DependencyIdentity>>,
    /// The workload package built in place of the tree's own, from the workspace that prepared it, when preparation overlaid it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<DependencyIdentity>,
    /// Versions the tree's lockfile locks that its overlaid build used differently, such as `signal-hook 0.4.5 -> 0.4.6` when the workload needs a newer release. Empty when the tree was built as locked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lockfile_changes: Vec<String>,
}

impl SourceIdentity {
    /// A committed, unmodified revision whose out-of-tree path dependencies, and overlaid workload package, are committed and unmodified too.
    pub fn is_frozen(&self) -> bool {
        self.revision.is_some()
            && self.dirty == Some(false)
            && self
                .path_dependencies
                .as_ref()
                .is_some_and(|dependencies| dependencies.iter().all(DependencyIdentity::is_frozen))
            && self
                .overlay
                .as_ref()
                .is_none_or(DependencyIdentity::is_frozen)
    }
}

/// A path dependency outside the built tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyIdentity {
    pub name: String,
    pub path: PathBuf,
    pub revision: Option<String>,
    pub dirty: Option<bool>,
}

impl DependencyIdentity {
    pub fn is_frozen(&self) -> bool {
        self.revision.is_some() && self.dirty == Some(false)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildIdentity {
    pub command: Vec<String>,
    /// `rustc -Vv` in the source tree.
    pub toolchain: String,
    /// `cargo -Vv` in the source tree.
    pub cargo: String,
    /// Build-affecting variables from the preparation environment: `RUSTFLAGS`, `RUSTC*`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_*`, `CARGO_PROFILE_*`, and `CARGO_TARGET_*`.
    pub environment: BTreeMap<String, String>,
    /// Cargo configuration files that apply to the tree, from the tree upward and `$CARGO_HOME`, with their contents' hashes.
    pub configuration: Vec<ConfigurationFile>,
    /// The `-C metadata` value added to the workspace's own crates, which gives a layout replica a different code layout from its baseline. `None` for every other build.
    #[serde(default)]
    pub layout_salt: Option<String>,
}

impl BuildIdentity {
    /// Settings that must match between artifacts of a recorded comparison. Configuration is compared by content, since replicas live at other paths.
    pub fn settings(&self) -> (&str, &str, &BTreeMap<String, String>, Vec<&str>) {
        let mut configuration: Vec<&str> = self
            .configuration
            .iter()
            .map(|file| file.sha256.as_str())
            .collect();
        configuration.sort_unstable();
        (
            &self.toolchain,
            &self.cargo,
            &self.environment,
            configuration,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationFile {
    pub path: PathBuf,
    pub sha256: String,
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    sha256_read(&mut open(path)?)
}

fn open(path: &Path) -> io::Result<File> {
    File::open(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

fn sha256_read(file: &mut File) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// What identifies a file's contents without reading them: on Unix its device, inode, size, and modification time; elsewhere its path, size, and modification time.
///
/// Writes update the modification time, at the file system's timestamp granularity; Linux 6.13 and later make an update after the time was read fine-grained on file systems with multigrain timestamps, such as ext4, XFS, btrfs, and tmpfs. The change time would also catch a modification time set back on purpose, but staging hard-links executables, which updates it, so every campaign would read them again. Verification guards against rebuilt or replaced executables, not deliberate tampering: a same-size rewrite that keeps or restores the modification time goes unnoticed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FileIdentity {
    #[cfg(unix)]
    file: (u64, u64, i64, i64),
    #[cfg(not(unix))]
    file: (PathBuf, Option<std::time::SystemTime>),
    length: u64,
}

impl FileIdentity {
    /// The identity of an open file, so that it describes what is read from it.
    fn of(file: &File, path: &Path) -> Self {
        let metadata = file.metadata().expect("an open file has metadata");
        #[cfg(unix)]
        let file = {
            let _ = path;
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            )
        };
        #[cfg(not(unix))]
        let file = (path.to_owned(), metadata.modified().ok());
        Self {
            file,
            length: metadata.len(),
        }
    }

    fn at(path: &Path) -> io::Result<Self> {
        File::open(path).map(|file| Self::of(&file, path))
    }
}

/// Hashes this process has computed, by the identity of the file hashed.
static HASHES: Mutex<BTreeMap<FileIdentity, String>> = Mutex::new(BTreeMap::new());

fn hashes() -> std::sync::MutexGuard<'static, BTreeMap<FileIdentity, String>> {
    // Entries are inserted whole, so a panic elsewhere cannot leave one partly written.
    HASHES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The SHA-256 of `path`, read only when this process has not already hashed the same file with the same identity.
///
/// Runs verify every executable before each sample, and a smoke run verifies the same one for every scenario; reading a large executable each time dominated them, especially in unoptimized test builds. The identity comes from the opened file before and after reading it, and a file that changed while it was read is not remembered.
pub fn sha256_identified(path: &Path) -> io::Result<String> {
    identified(path).map(|(_, hash)| hash)
}

/// The identity a file had when it was opened, and its hash.
fn identified(path: &Path) -> io::Result<(FileIdentity, String)> {
    let mut file = open(path)?;
    let identity = FileIdentity::of(&file, path);
    if let Some(hash) = hashes().get(&identity).cloned() {
        return Ok((identity, hash));
    }
    let hash = sha256_read(&mut file)?;
    // Remember the hash only if the file kept its identity while it was read.
    let _ = Some(FileIdentity::of(&file, path))
        .filter(|after| *after == identity)
        .map(|_| hashes().insert(identity.clone(), hash.clone()));
    Ok((identity, hash))
}

/// Copy `from` to `to`, and remember the copy's hash as the source's when the source kept the identity it was hashed under, so that the copy is not read again.
///
/// Staging falls back to copying when it cannot hard-link, such as from one file system to another, and each campaign stages again.
pub(crate) fn copy_identified(from: &Path, to: &Path) -> io::Result<()> {
    let (identity, hash) = identified(from)?;
    write_executable(to, Contents::Copy(from)).map(|()| {
        let _ = FileIdentity::at(from)
            .ok()
            .filter(|after| *after == identity)
            .and_then(|_| FileIdentity::at(to).ok())
            .map(|copied| hashes().insert(copied, hash));
    })
}

/// What [`write_executable`] writes.
pub(crate) enum Contents<'a> {
    /// A copy of this file, with its permissions.
    Copy(&'a Path),
    /// A script, executable by everyone.
    #[cfg(unix)]
    Script(&'a [u8]),
}

/// Write an executable that will be run, through a child process, so that this process never holds it open for writing.
///
/// A process forked by another thread while a file is open for writing shares that descriptor, close-on-exec or not, until it execs or exits, and running the file meanwhile fails with `ETXTBSY`. The runner retries that when it starts a program itself, but a program that execs the file in turn, such as `perf` running a staged executable or Cargo running the layout salt wrapper, does not, and a caller's child that never execs holds it until it exits. The child that writes, `cat` run by `/bin/sh`, starts no other process meanwhile.
#[cfg(unix)]
pub(crate) fn write_executable(path: &Path, contents: Contents) -> io::Result<()> {
    write_through(Path::new("/bin/sh"), path, contents)
}

/// Write `path` with `writer`, a POSIX shell, running `exec cat > PATH`.
#[cfg(unix)]
fn write_through(writer: &Path, path: &Path, contents: Contents) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let source = match contents {
        Contents::Copy(from) => File::open(from).and_then(|file| {
            file.metadata()
                .map(|metadata| (Stdio::from(file), None, metadata.permissions()))
        }),
        Contents::Script(script) => Ok((
            Stdio::piped(),
            Some(script),
            std::fs::Permissions::from_mode(0o755),
        )),
    };
    let (output, written, permissions) = source.and_then(|(input, script, permissions)| {
        Command::new(writer)
            .args(["-c", "exec cat > \"$0\""])
            .arg(path)
            .stdin(input)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            // Named, so that a missing shell is not taken for a missing source.
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("cannot run the writer, {}: {error}", writer.display()),
                )
            })
            .and_then(|mut writer| {
                let written = writer.stdin.take().map_or(Ok(()), |mut stdin| {
                    stdin.write_all(script.unwrap_or_default())
                });
                writer
                    .wait_with_output()
                    .map(|output| (output, written, permissions))
            })
    })?;
    // A writer that fails before reading its input closes the pipe, so its own error explains a failed write.
    if !output.status.success() {
        // A writer stopped by a signal usually says nothing.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let explanation = Some(stderr.trim())
            .filter(|stderr| !stderr.is_empty())
            .map_or(String::new(), |stderr| format!(": {stderr}"));
        return Err(io::Error::other(format!(
            "writing failed with {}{explanation}",
            output.status
        )));
    }
    written.and_then(|()| std::fs::set_permissions(path, permissions))
}

/// Write an executable that will be run. Without `fork`, another thread's child cannot share the descriptor, so the file is written here.
#[cfg(not(unix))]
pub(crate) fn write_executable(path: &Path, contents: Contents) -> io::Result<()> {
    let Contents::Copy(from) = contents;
    std::fs::copy(from, path).map(|_| ())
}

/// Write a pretty-printed JSON document, naming the path in any error.
pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let mut text = serde_json::to_vec_pretty(value).expect("benchmark documents serialize");
    text.push(b'\n');
    // Write a sibling and rename it into place, so readers never see a partly written document.
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, text)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

/// Paths of a prepared artifact set.
pub(crate) fn manifest_path(output: &Path) -> PathBuf {
    output.join("artifacts.json")
}

#[cfg(test)]
mod tests;
