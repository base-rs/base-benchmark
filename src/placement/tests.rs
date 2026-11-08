// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::role::{RoleKind, RoleProgram};

fn role(name: &str, cpus: &[usize]) -> PlannedRole {
    PlannedRole {
        name: name.to_owned(),
        kind: RoleKind::Service,
        program: RoleProgram::Subject,
        arguments: Vec::new(),
        cpus: cpus.to_vec(),
        ready: None,
        stop: None,
        environment: Default::default(),
    }
}

/// Four cores with two SMT threads each: CPU `n` and `n + 4` are siblings.
fn smt() -> Topology {
    Topology::new(
        (0..8)
            .map(|cpu| (cpu, vec![cpu % 4, cpu % 4 + 4]))
            .collect(),
    )
}

#[test]
fn roles_share_a_core_through_a_logical_cpu_or_its_sibling() {
    let topology = smt();
    let roles = [
        role("server", &[1, 2]),
        role("client", &[6]),
        role("relay", &[3]),
        role("unplaced", &[]),
        role("other", &[1]),
    ];
    let (shared, unknown) = shared_cores(&roles, &topology);
    assert_eq!(
        shared,
        [
            SharedCore {
                roles: ["server".to_owned(), "client".to_owned()],
                cpus: vec![2, 6]
            },
            SharedCore {
                roles: ["server".to_owned(), "other".to_owned()],
                cpus: vec![1]
            },
        ]
    );
    assert!(unknown.is_empty());
    assert_eq!(
        describe(&shared),
        "server and client on CPUs 2, 6; server and other on CPUs 1"
    );
    // Separate physical cores share nothing.
    let (shared, _) = shared_cores(
        &[role("server", &[0, 4]), role("client", &[1, 5])],
        &topology,
    );
    assert!(shared.is_empty());
}

#[test]
fn cpus_of_unknown_topology_are_compared_only_as_logical_cpus() {
    let topology = Topology::new([(0, vec![0, 4])].into());
    let (shared, unknown) = shared_cores(
        &[role("server", &[0, 9]), role("client", &[9, 4])],
        &topology,
    );
    assert_eq!(shared[0].cpus, [0, 4, 9]);
    assert_eq!(unknown, [4, 9]);
}

#[test]
fn topology_is_read_from_sysfs() {
    let directory = tempfile::tempdir().unwrap();
    for (cpu, siblings) in [(0, "0,4\n"), (4, "0,4\n"), (1, "garbage\n")] {
        let path = directory
            .path()
            .join(format!("sys/devices/system/cpu/cpu{cpu}/topology"));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("thread_siblings_list"), siblings).unwrap();
    }
    let topology = Topology::read_from(directory.path(), &[0, 1, 2, 4]);
    assert_eq!(topology, Topology::new([(0, vec![4]), (4, vec![0])].into()));
    // This host reports its own CPUs.
    let host = Topology::read(&[0]);
    assert!(host.core(0).is_some_and(|core| core.contains(&0)));
}
