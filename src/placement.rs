// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Whether roles' CPU sets share physical cores on the measuring host.

use crate::plan::{PlannedRole, parse_cpus};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Which logical CPUs share a physical core: each CPU's SMT siblings, itself included, as the measuring host reports them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Topology {
    siblings: BTreeMap<usize, BTreeSet<usize>>,
}

impl Topology {
    /// A topology from each CPU's siblings, for example to plan for another host or in tests.
    pub fn new(siblings: BTreeMap<usize, Vec<usize>>) -> Self {
        Self {
            siblings: siblings
                .into_iter()
                .map(|(cpu, siblings)| (cpu, siblings.into_iter().chain([cpu]).collect()))
                .collect(),
        }
    }

    /// The siblings of `cpus` on this host, from `/sys/devices/system/cpu/cpuN/topology/thread_siblings_list`. CPUs whose siblings cannot be read are unknown.
    pub fn read(cpus: &[usize]) -> Self {
        Self::read_from(Path::new("/"), cpus)
    }

    pub(crate) fn read_from(root: &Path, cpus: &[usize]) -> Self {
        Self::new(
            cpus.iter()
                .filter_map(|&cpu| {
                    let path = root.join(format!(
                        "sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"
                    ));
                    let siblings = std::fs::read_to_string(path).ok()?;
                    parse_cpus(siblings.trim())
                        .ok()
                        .map(|siblings| (cpu, siblings))
                })
                .collect(),
        )
    }

    fn core(&self, cpu: usize) -> Option<&BTreeSet<usize>> {
        self.siblings.get(&cpu)
    }
}

/// Two roles whose CPU sets share a physical core: the same logical CPU, or SMT siblings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedCore {
    pub roles: [String; 2],
    /// The CPUs of either role on a core that the other role also uses.
    pub cpus: Vec<usize>,
}

/// Every pair of placed roles that share a physical core, and the placed CPUs whose siblings are unknown, for which sharing cannot be ruled out. A CPU of unknown topology is still compared with the other roles' CPUs as a logical CPU.
pub(crate) fn shared_cores(
    roles: &[PlannedRole],
    topology: &Topology,
) -> (Vec<SharedCore>, Vec<usize>) {
    // Siblings are symmetric, so a CPU whose own siblings are unknown still shares a core with a CPU that names it.
    let named =
        |cpu: usize, other: usize| topology.core(cpu).is_some_and(|core| core.contains(&other));
    let same_core = |first: usize, second: usize| {
        first == second || named(first, second) || named(second, first)
    };
    let mut shared = Vec::new();
    for (index, first) in roles.iter().enumerate() {
        for second in &roles[index + 1..] {
            let on = |of: &PlannedRole, other: &PlannedRole| {
                of.cpus
                    .iter()
                    .copied()
                    .filter(|cpu| other.cpus.iter().any(|sibling| same_core(*cpu, *sibling)))
                    .collect::<Vec<_>>()
            };
            let mut cpus: Vec<usize> = on(first, second)
                .into_iter()
                .chain(on(second, first))
                .collect();
            cpus.sort_unstable();
            cpus.dedup();
            if !cpus.is_empty() {
                shared.push(SharedCore {
                    roles: [first.name.clone(), second.name.clone()],
                    cpus,
                });
            }
        }
    }
    let unknown: BTreeSet<usize> = roles
        .iter()
        .flat_map(|role| &role.cpus)
        .copied()
        .filter(|cpu| topology.core(*cpu).is_none())
        .collect();
    (shared, unknown.into_iter().collect())
}

/// A sharing, for reports: `server and client on CPUs 2, 3`.
pub(crate) fn describe(shared: &[SharedCore]) -> String {
    shared
        .iter()
        .map(|shared| {
            format!(
                "{} and {} on CPUs {}",
                shared.roles[0],
                shared.roles[1],
                crate::plan::cpu_list(&shared.cpus)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests;
