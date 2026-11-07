// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::test_support::{artifact, manifest};
use std::path::Path;

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
    rewrite(&executable, "two");
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

/// Rewrite `path` and give it a modification time a second later, as a write after a clock tick would, independent of the file system's timestamp granularity.
fn rewrite(path: &Path, contents: &str) {
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    std::fs::write(path, contents).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified + std::time::Duration::from_secs(1))
        .unwrap();
}

#[test]
fn identified_hashes_are_read_once_until_the_file_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tool");
    std::fs::write(&path, "one").unwrap();
    let identity = FileIdentity::at(&path).unwrap();
    let hash = sha256_identified(&path).unwrap();
    assert_eq!(hash, sha256_file(&path).unwrap());
    assert_eq!(hashes().get(&identity).cloned(), Some(hash));
    // A remembered hash is returned without reading: replace it to observe that.
    hashes().insert(identity.clone(), "remembered".to_owned());
    assert_eq!(sha256_identified(&path).unwrap(), "remembered");
    // A hard link is the same file, so it shares the identity.
    let link = directory.path().join("link");
    std::fs::hard_link(&path, &link).unwrap();
    #[cfg(unix)]
    assert_eq!(sha256_identified(&link).unwrap(), "remembered");
    // A same-size rewrite with a later modification time is read again.
    rewrite(&path, "two");
    assert_ne!(FileIdentity::at(&path).unwrap(), identity);
    assert_eq!(
        sha256_identified(&path).unwrap(),
        sha256_file(&path).unwrap()
    );
    assert!(sha256_identified(&directory.path().join("missing")).is_err());
    assert!(sha256_identified(directory.path()).is_err());
}

#[test]
fn copies_inherit_the_hash_of_an_unchanged_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("tool");
    std::fs::write(&source, "one").unwrap();
    let copy = directory.path().join("copy");
    copy_identified(&source, &copy).unwrap();
    let hash = sha256_file(&source).unwrap();
    assert_eq!(
        hashes().get(&FileIdentity::at(&copy).unwrap()).cloned(),
        Some(hash)
    );
    // A changed copy is read again.
    rewrite(&copy, "two");
    assert_eq!(
        sha256_identified(&copy).unwrap(),
        sha256_file(&copy).unwrap()
    );
    assert!(copy_identified(&directory.path().join("missing"), &copy).is_err());
    assert!(copy_identified(&source, &directory.path().join("absent/copy")).is_err());
}

#[test]
fn hashing_reports_read_errors() {
    let directory = tempfile::tempdir().unwrap();
    assert!(sha256_file(directory.path()).is_err());
    let big = directory.path().join("big");
    std::fs::write(&big, vec![7; 200_000]).unwrap();
    assert_eq!(sha256_file(&big).unwrap().len(), 64);
}

#[test]
fn unprepared_executables_are_unfrozen_baselines() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    std::fs::write(&executable, "one").unwrap();
    let suite = crate::test_support::suite(&executable, &[]);
    let manifest = ArtifactManifest::unprepared(&suite, &executable).unwrap();
    manifest.validate().unwrap();
    assert_eq!(manifest.baseline().label, "unprepared");
    assert!(!manifest.baseline().source.is_frozen());
    assert!(ArtifactManifest::unprepared(&suite, &directory.path().join("missing")).is_err());
}

#[test]
fn each_feature_set_has_its_own_baseline_and_labels() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("tool");
    std::fs::write(&executable, "one").unwrap();
    let with = |label: &str, role, features: Option<&[&str]>| Artifact {
        features: features.map(|features| features.iter().map(|f| (*f).to_owned()).collect()),
        ..artifact(label, role, &executable)
    };
    let plain: &[&str] = &[];
    let extra: &[&str] = &["extra"];
    let valid = manifest(vec![
        with("baseline", ArtifactRole::Baseline, Some(plain)),
        with("baseline-replica", ArtifactRole::LayoutReplica, Some(plain)),
        with("baseline", ArtifactRole::Baseline, Some(extra)),
        with("baseline-replica", ArtifactRole::LayoutReplica, Some(extra)),
    ]);
    valid.validate().unwrap();
    let extra_build = valid.for_features(&["extra".to_owned()]).unwrap();
    assert_eq!(extra_build.artifacts.len(), 2);
    assert!(
        extra_build
            .artifacts
            .iter()
            .all(|artifact| artifact.features.as_deref() == Some(&["extra".to_owned()][..]))
    );
    assert_eq!(
        valid.for_features(&[]).unwrap().baseline().label,
        "baseline"
    );
    assert!(
        valid
            .for_features(&["other".to_owned()])
            .unwrap_err()
            .to_string()
            .contains("no artifact was built with features [\"other\"]")
    );
    // Unknown features, as for an unprepared executable, serve any scenario.
    let unknown = manifest(vec![with("baseline", ArtifactRole::Baseline, None)]);
    assert_eq!(
        unknown
            .for_features(&["other".to_owned()])
            .unwrap()
            .artifacts
            .len(),
        1
    );

    for (artifacts, message) in [
        (
            vec![
                with("baseline", ArtifactRole::Baseline, Some(plain)),
                with("baseline", ArtifactRole::Candidate, Some(plain)),
            ],
            "unique within each feature set",
        ),
        (
            vec![
                with("baseline", ArtifactRole::Baseline, Some(plain)),
                with("candidate", ArtifactRole::Candidate, Some(extra)),
            ],
            "exactly one baseline for each feature set",
        ),
        (
            vec![
                with("baseline", ArtifactRole::Baseline, Some(plain)),
                with("other", ArtifactRole::Baseline, Some(extra)),
                Artifact {
                    source: SourceIdentity {
                        replica_of: Some("baseline".to_owned()),
                        ..artifact("other-replica", ArtifactRole::LayoutReplica, &executable).source
                    },
                    ..with("other-replica", ArtifactRole::LayoutReplica, Some(extra))
                },
            ],
            "with the same features",
        ),
    ] {
        let error = manifest(artifacts).validate().unwrap_err().to_string();
        assert!(error.contains(message), "{message}: {error}");
    }
}
