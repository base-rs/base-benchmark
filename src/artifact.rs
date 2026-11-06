// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::config::invalid_input;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

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
                sha256: sha256_file(executable)?,
                executable: executable.to_owned(),
                source: SourceIdentity {
                    path: executable.to_owned(),
                    revision: None,
                    dirty: None,
                    lockfile_sha256: None,
                    replica_of: None,
                },
                build: BuildIdentity {
                    command: Vec::new(),
                    toolchain: "unknown".to_owned(),
                    rustflags: None,
                    encoded_rustflags: None,
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
        let actual = sha256_file(&self.executable)?;
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
}

impl SourceIdentity {
    /// A committed, unmodified revision.
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
    /// `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS` from the preparation environment.
    pub rustflags: Option<String>,
    pub encoded_rustflags: Option<String>,
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

/// Write a pretty-printed JSON document, naming the path in any error.
pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let mut text = serde_json::to_vec_pretty(value).expect("benchmark documents serialize");
    text.push(b'\n');
    std::fs::write(path, text)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

/// Paths of a prepared artifact set.
pub(crate) fn manifest_path(output: &Path) -> PathBuf {
    output.join("artifacts.json")
}

#[cfg(test)]
mod tests;
