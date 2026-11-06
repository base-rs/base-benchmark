// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The Cargo package metadata table read by [`discover`]: `[package.metadata.base-benchmark]`.
pub const METADATA_KEY: &str = "base-benchmark";

/// A workspace package that declares benchmark scenarios, including unpublished packages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suite {
    pub package: String,
    pub manifest_path: PathBuf,
    pub workspace_root: PathBuf,
    /// The workload executable target.
    pub binary: String,
    /// The Cargo profile used to prepare artifacts.
    pub profile: String,
    pub features: Vec<String>,
    pub scenarios: Vec<Scenario>,
}

impl Suite {
    pub fn scenario(&self, name: &str) -> io::Result<&Scenario> {
        self.scenarios
            .iter()
            .find(|scenario| scenario.name == name)
            .ok_or_else(|| {
                let names: Vec<_> = self
                    .scenarios
                    .iter()
                    .map(|scenario| scenario.name.as_str())
                    .collect();
                invalid_input(format!(
                    "{} has no scenario {name:?}; available: {}",
                    self.package,
                    names.join(", ")
                ))
            })
    }
}

/// A named workload configuration. Variants are alternative implementations measured within each artifact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Arguments appended only for smoke runs, typically small iteration counts.
    #[serde(default)]
    pub smoke_arguments: Vec<String>,
    #[serde(default)]
    pub variants: Vec<Variant>,
    /// The variant other variants are compared with; defaults to the first.
    #[serde(default)]
    pub baseline: Option<String>,
}

impl Scenario {
    pub fn baseline_variant(&self) -> Option<&Variant> {
        match &self.baseline {
            Some(name) => self.variants.iter().find(|variant| &variant.name == name),
            None => self.variants.first(),
        }
    }

    fn validate(&self, package: &str) -> io::Result<()> {
        let context = |message: String| {
            invalid_input(format!("{package} scenario {:?}: {message}", self.name))
        };
        if !valid_label(&self.name) {
            return Err(context(
                "names may contain only ASCII letters, digits, '.', '_', and '-'".to_owned(),
            ));
        }
        let mut names = BTreeSet::new();
        for variant in &self.variants {
            if !valid_label(&variant.name) || !names.insert(variant.name.as_str()) {
                return Err(context(format!(
                    "invalid or duplicate variant {:?}",
                    variant.name
                )));
            }
        }
        if let Some(baseline) = &self.baseline {
            if !names.contains(baseline.as_str()) {
                return Err(context(format!("baseline {baseline:?} is not a variant")));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub name: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

/// `[package.metadata.base-benchmark]`. Package-level `arguments` are prepended to every scenario's; `smoke-arguments`, `variants`, and `baseline` are defaults a scenario can replace.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct PackageConfiguration {
    binary: Option<String>,
    #[serde(default = "release")]
    profile: String,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default)]
    smoke_arguments: Vec<String>,
    #[serde(default)]
    variants: Vec<Variant>,
    baseline: Option<String>,
    #[serde(default)]
    scenario: Vec<ScenarioConfiguration>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ScenarioConfiguration {
    name: String,
    #[serde(default)]
    arguments: Vec<String>,
    smoke_arguments: Option<Vec<String>>,
    variants: Option<Vec<Variant>>,
    baseline: Option<String>,
}

impl PackageConfiguration {
    /// Resolve package-level defaults into each scenario.
    fn scenarios(&self) -> Vec<Scenario> {
        self.scenario
            .iter()
            .map(|scenario| Scenario {
                name: scenario.name.clone(),
                arguments: self
                    .arguments
                    .iter()
                    .chain(&scenario.arguments)
                    .cloned()
                    .collect(),
                smoke_arguments: scenario
                    .smoke_arguments
                    .clone()
                    .unwrap_or_else(|| self.smoke_arguments.clone()),
                variants: scenario
                    .variants
                    .clone()
                    .unwrap_or_else(|| self.variants.clone()),
                baseline: match &scenario.variants {
                    Some(_) => scenario.baseline.clone(),
                    None => scenario.baseline.clone().or_else(|| self.baseline.clone()),
                },
            })
            .collect()
    }
}

fn release() -> String {
    "release".to_owned()
}

/// Discover benchmark suites in the workspace containing `manifest_path`, using locked Cargo metadata.
///
/// Packages opt in with `[package.metadata.base-benchmark]`. Unpublished packages are included; nothing is executed or built.
pub fn discover(cargo: &Path, manifest_path: &Path) -> io::Result<Vec<Suite>> {
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--locked",
            "--manifest-path",
        ])
        .arg(manifest_path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "cargo metadata failed for {}: {}",
            manifest_path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
    parse_metadata(&metadata)
}

pub(crate) fn parse_metadata(metadata: &Value) -> io::Result<Vec<Suite>> {
    let workspace_root = metadata["workspace_root"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| invalid_input("cargo metadata has no workspace_root"))?;
    let mut suites = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let Some(configuration) = package["metadata"].get(METADATA_KEY) else {
            continue;
        };
        let name = package["name"].as_str().unwrap_or_default().to_owned();
        let configuration = PackageConfiguration::deserialize(configuration).map_err(|error| {
            invalid_input(format!(
                "{name}: invalid [package.metadata.{METADATA_KEY}]: {error}"
            ))
        })?;
        let binaries: Vec<&str> = package["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|target| {
                target["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
            })
            .filter_map(|target| target["name"].as_str())
            .collect();
        let binary = match configuration.binary.clone() {
            Some(binary) if binaries.contains(&binary.as_str()) => binary,
            Some(binary) => {
                return Err(invalid_input(format!(
                    "{name}: binary {binary:?} is not a binary target"
                )));
            }
            None if binaries.len() == 1 => binaries[0].to_owned(),
            None => {
                return Err(invalid_input(format!(
                    "{name}: choose a workload with `binary`; found {} binary targets",
                    binaries.len()
                )));
            }
        };
        let scenarios = configuration.scenarios();
        let mut scenario_names = BTreeSet::new();
        for scenario in &scenarios {
            scenario.validate(&name)?;
            if !scenario_names.insert(scenario.name.as_str()) {
                return Err(invalid_input(format!(
                    "{name}: duplicate scenario {:?}",
                    scenario.name
                )));
            }
        }
        suites.push(Suite {
            manifest_path: PathBuf::from(package["manifest_path"].as_str().unwrap_or_default()),
            package: name,
            workspace_root: workspace_root.clone(),
            binary,
            profile: configuration.profile,
            features: configuration.features,
            scenarios,
        });
    }
    Ok(suites)
}

/// Select `package`, or the only suite when no package is named.
pub fn select(suites: Vec<Suite>, package: Option<&str>) -> io::Result<Suite> {
    let names: Vec<String> = suites.iter().map(|suite| suite.package.clone()).collect();
    let mut matching: Vec<Suite> = suites
        .into_iter()
        .filter(|suite| package.is_none_or(|package| suite.package == package))
        .collect();
    match (matching.len(), package) {
        (1, _) => Ok(matching.remove(0)),
        (0, Some(package)) => Err(invalid_input(format!(
            "no benchmark suite named {package:?}; available: {}",
            names.join(", ")
        ))),
        (0, None) => Err(invalid_input(format!(
            "no workspace package declares [package.metadata.{METADATA_KEY}]"
        ))),
        _ => Err(invalid_input(format!(
            "several benchmark suites exist; choose one with --package: {}",
            names.join(", ")
        ))),
    }
}

/// The Cargo executable: `$CARGO` when running under Cargo, otherwise `cargo`.
pub fn cargo_program() -> PathBuf {
    program(std::env::var_os("CARGO"), "cargo")
}

/// A tool from its environment variable, or by name from `PATH`.
pub(crate) fn program(variable: Option<OsString>, name: &str) -> PathBuf {
    variable.map_or_else(|| PathBuf::from(name), PathBuf::from)
}

/// Labels become directory names, so they are restricted to a portable character set.
pub(crate) fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && !label.starts_with('.')
        && label.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

pub(crate) fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests;
