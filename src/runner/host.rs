// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The measuring host, as far as software can describe it. It cannot prove the host was idle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIdentity {
    pub hostname: Option<String>,
    pub kernel: Option<String>,
    pub cpu_model: Option<String>,
    /// `kernel.randomize_va_space`: `2` randomizes stack, heap, and mappings for every process; `0` disables it.
    pub address_space_randomization: Option<String>,
    pub cpus: Vec<CpuIdentity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuIdentity {
    pub cpu: usize,
    /// SMT siblings sharing the core, which should stay idle during measurement.
    pub thread_siblings: Option<String>,
    pub scaling_governor: Option<String>,
}

impl HostIdentity {
    pub fn capture(cpus: &[usize]) -> Self {
        Self::capture_from(Path::new("/"), cpus)
    }

    pub(crate) fn capture_from(root: &Path, cpus: &[usize]) -> Self {
        let read = |path: &str| {
            std::fs::read_to_string(root.join(path))
                .ok()
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        };
        let cpu_model = read("proc/cpuinfo").and_then(|text| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key.trim() == "model name").then(|| value.trim().to_owned())
            })
        });
        Self {
            hostname: read("proc/sys/kernel/hostname"),
            kernel: read("proc/sys/kernel/osrelease"),
            address_space_randomization: read("proc/sys/kernel/randomize_va_space"),
            cpu_model,
            cpus: cpus
                .iter()
                .map(|&cpu| CpuIdentity {
                    cpu,
                    thread_siblings: read(&format!(
                        "sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"
                    )),
                    scaling_governor: read(&format!(
                        "sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor"
                    )),
                })
                .collect(),
        }
    }
}
