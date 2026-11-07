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
pub const MANIFEST_VERSION: u32 = 1;

/// Frozen executables prepared before any measurement, with the identity of each source tree and build.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub schema: String,
    pub version: u32,
    pub package: String,
    pub binary: String,
    pub profile: String,
    pub features: Vec<String>,
    pub artifacts: Vec<Artifact>,
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

    /// Describe an executable built outside [`crate::prepare`], such as a test binary, as the only baseline artifact.
    ///
    /// Its source and build are unknown, so it can be used for smoke and screening runs but never satisfies recorded prerequisites.
    pub fn unprepared(suite: &crate::config::Suite, executable: &Path) -> io::Result<Self> {
        Ok(Self {
            schema: MANIFEST_SCHEMA.to_owned(),
            version: MANIFEST_VERSION,
            package: suite.package.clone(),
            binary: suite.binary.clone(),
            profile: "unknown".to_owned(),
            features: Vec::new(),
            artifacts: vec![Artifact {
                label: "unprepared".to_owned(),
                role: ArtifactRole::Baseline,
                sha256: sha256_identified(executable)?,
                executable: executable.to_owned(),
                source: SourceIdentity {
                    path: executable.to_owned(),
                    revision: None,
                    dirty: None,
                    lockfile_sha256: None,
                    replica_of: None,
                    path_dependencies: None,
                },
                build: BuildIdentity {
                    command: Vec::new(),
                    toolchain: "unknown".to_owned(),
                    cargo: "unknown".to_owned(),
                    environment: BTreeMap::new(),
                    configuration: Vec::new(),
                },
            }],
        })
    }

    pub fn baseline(&self) -> &Artifact {
        self.artifacts
            .iter()
            .find(|artifact| artifact.role == ArtifactRole::Baseline)
            .expect("validated manifests have one baseline")
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema != MANIFEST_SCHEMA || self.version != MANIFEST_VERSION {
            return Err(invalid_input(format!(
                "unsupported artifact manifest {} version {}",
                self.schema, self.version
            )));
        }
        let labels: BTreeSet<&str> = self
            .artifacts
            .iter()
            .map(|artifact| artifact.label.as_str())
            .collect();
        if labels.len() != self.artifacts.len()
            || labels
                .iter()
                .any(|label| !crate::config::valid_label(label))
        {
            return Err(invalid_input("artifact labels must be unique and valid"));
        }
        if self
            .artifacts
            .iter()
            .filter(|artifact| artifact.role == ArtifactRole::Baseline)
            .count()
            != 1
        {
            return Err(invalid_input(
                "an artifact manifest needs exactly one baseline",
            ));
        }
        for artifact in &self.artifacts {
            let replica_of = artifact.source.replica_of.as_deref();
            let consistent = match artifact.role {
                ArtifactRole::LayoutReplica => replica_of
                    .is_some_and(|label| labels.contains(label) && label != artifact.label),
                _ => replica_of.is_none(),
            };
            if !consistent {
                return Err(invalid_input(format!(
                    "{}: only layout replicas name an existing replicated artifact",
                    artifact.label
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub label: String,
    pub role: ArtifactRole,
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
    /// The baseline's source copied to another path and rebuilt, exposing build-layout sensitivity.
    LayoutReplica,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    /// The directory that was built.
    pub path: PathBuf,
    /// `git rev-parse HEAD`, when available.
    pub revision: Option<String>,
    /// Whether `git status --porcelain` reported changes, when available.
    pub dirty: Option<bool>,
    pub lockfile_sha256: Option<String>,
    pub replica_of: Option<String>,
    /// Path dependencies outside the tree. `Cargo.lock` does not pin them, so each is identified by its own Git state. `None` when Cargo metadata could not be read, so they are unknown and the source is not frozen. A replica repeats its baseline's.
    pub path_dependencies: Option<Vec<DependencyIdentity>>,
}

impl SourceIdentity {
    /// A committed, unmodified revision whose out-of-tree path dependencies are committed and unmodified too.
    pub fn is_frozen(&self) -> bool {
        self.revision.is_some()
            && self.dirty == Some(false)
            && self
                .path_dependencies
                .as_ref()
                .is_some_and(|dependencies| dependencies.iter().all(DependencyIdentity::is_frozen))
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
    let mut file = File::open(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
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
/// Every write updates the modification time. The change time would also catch a modification time set back on purpose, but staging hard-links executables, which updates it, so every campaign would read them again. Verification guards against rebuilt or replaced executables, not deliberate tampering. A rewrite to the same size within the file system's timestamp granularity of the hash would go unnoticed; Linux 6.13 and later give a timestamp that was read since the last change a fine-grained update, which `stat` here ensures.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FileIdentity {
    #[cfg(unix)]
    file: (u64, u64, i64, i64),
    #[cfg(not(unix))]
    file: (PathBuf, Option<std::time::SystemTime>),
    length: u64,
}

impl FileIdentity {
    fn of(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path).map_err(|error| {
            io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        })?;
        #[cfg(unix)]
        let file = {
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
        Ok(Self {
            file,
            length: metadata.len(),
        })
    }
}

/// Hashes this process has computed, by the identity of the file hashed.
static HASHES: Mutex<BTreeMap<FileIdentity, String>> = Mutex::new(BTreeMap::new());

/// The SHA-256 of `path`, read only when this process has not already hashed the same file with the same identity.
///
/// Runs verify every executable before each sample, and a smoke run verifies the same one for every scenario; reading a large executable each time dominated them, especially in unoptimized test builds. The identity is taken before and after reading, and a file that changed while it was read is not remembered.
pub fn sha256_identified(path: &Path) -> io::Result<String> {
    let identity = FileIdentity::of(path)?;
    if let Some(hash) = hashes().get(&identity) {
        return Ok(hash.clone());
    }
    let hash = sha256_file(path)?;
    // Remember the hash only if the file kept its identity while it was read.
    let _ = FileIdentity::of(path)
        .ok()
        .filter(|after| *after == identity)
        .map(|_| hashes().insert(identity, hash.clone()));
    Ok(hash)
}

fn hashes() -> std::sync::MutexGuard<'static, BTreeMap<FileIdentity, String>> {
    HASHES.lock().expect("the hash cache is never poisoned")
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
