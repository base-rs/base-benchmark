// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::test_support::{artifact, manifest};

#[test]
fn manifests_round_trip_and_verify_executables() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    std::fs::write(&executable, "one").unwrap();
    let manifest = manifest(vec![
        artifact("baseline", ArtifactRole::Baseline, &executable),
        artifact("baseline-replica", ArtifactRole::LayoutReplica, &executable),
    ]);
    let path = manifest_path(directory.path());
    manifest.write(&path).unwrap();
    let read = ArtifactManifest::read(&path).unwrap();
    assert_eq!(read, manifest);
    assert_eq!(read.baseline().label, "baseline");
    read.baseline().verify().unwrap();
    assert!(read.baseline().source.is_frozen());
    std::fs::write(&executable, "two").unwrap();
    assert!(
        read.baseline()
            .verify()
            .unwrap_err()
            .to_string()
            .contains("changed after preparation")
    );
    std::fs::remove_file(&executable).unwrap();
    assert!(read.baseline().verify().is_err());
}

#[test]
fn invalid_manifests_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    std::fs::write(&executable, "one").unwrap();
    let baseline = artifact("baseline", ArtifactRole::Baseline, &executable);
    let mut duplicate_label = baseline.clone();
    duplicate_label.role = ArtifactRole::Candidate;
    let mut orphan_replica = artifact("replica", ArtifactRole::LayoutReplica, &executable);
    orphan_replica.source.replica_of = Some("missing".to_owned());
    let mut candidate_replica_of = artifact("candidate", ArtifactRole::Candidate, &executable);
    candidate_replica_of.source.replica_of = Some("baseline".to_owned());
    let cases = [
        (
            ArtifactManifest {
                version: 9,
                ..manifest(vec![baseline.clone()])
            },
            "unsupported artifact manifest",
        ),
        (manifest(vec![baseline.clone(), duplicate_label]), "unique"),
        (
            manifest(vec![artifact(
                "candidate",
                ArtifactRole::Candidate,
                &executable,
            )]),
            "exactly one baseline",
        ),
        (
            manifest(vec![baseline.clone(), orphan_replica]),
            "only layout replicas",
        ),
        (
            manifest(vec![baseline, candidate_replica_of]),
            "only layout replicas",
        ),
    ];
    for (manifest, message) in cases {
        let error = manifest
            .write(&directory.path().join("out.json"))
            .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    std::fs::write(directory.path().join("bad.json"), "{").unwrap();
    assert!(ArtifactManifest::read(&directory.path().join("bad.json")).is_err());
    std::fs::write(
        directory.path().join("invalid.json"),
        serde_json::to_vec(&ArtifactManifest {
            version: 9,
            ..manifest(Vec::new())
        })
        .unwrap(),
    )
    .unwrap();
    assert!(ArtifactManifest::read(&directory.path().join("invalid.json")).is_err());
    assert!(
        ArtifactManifest::read(&directory.path().join("missing.json"))
            .unwrap_err()
            .to_string()
            .contains("missing.json")
    );
}

#[test]
fn hashing_reports_read_errors() {
    let directory = tempfile::tempdir().unwrap();
    assert!(sha256_file(directory.path()).is_err());
    let big = directory.path().join("big");
    std::fs::write(&big, vec![7; 200_000]).unwrap();
    assert_eq!(sha256_file(&big).unwrap().len(), 64);
}
