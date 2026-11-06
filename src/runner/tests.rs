// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::artifact::ArtifactRole;
use crate::plan::PairKind;
use crate::test_support::{plan, record, script};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const ROLES: &[(&str, ArtifactRole)] = &[("baseline", ArtifactRole::Baseline)];

/// A workload whose wall time depends on its variant argument.
fn workload(directory: &Path, signature_for_fast: &str) -> PathBuf {
    let slow = record("same", 1000);
    let fast = record(signature_for_fast, 900);
    script(
        directory,
        "work.sh",
        &format!(
            "if [ \"$1\" = fast ]; then echo '{fast}'; else echo '{slow}'; fi\necho note >&2\n"
        ),
    )
}

fn campaign(directory: &Path, body: &str) -> (Plan, PathBuf) {
    let work = script(directory, "work.sh", body);
    (
        plan(&work, &[], ROLES, &directory.join("lock")),
        directory.join("campaign"),
    )
}

#[test]
fn complete_campaigns_journal_samples_and_summarize_pairs() {
    let directory = tempfile::tempdir().unwrap();
    let work = workload(directory.path(), "same");
    let plan = plan(
        &work,
        &["slow", "fast"],
        ROLES,
        &directory.path().join("lock"),
    );
    let output = directory.path().join("results/campaign");
    let mut progress = Vec::new();
    let campaign = run(plan, &output, &Cancellation::new(), &mut progress).unwrap();
    assert!(
        String::from_utf8(progress)
            .unwrap()
            .contains("round 2/2 [2/2]")
    );
    let metadata = read_metadata(&output).unwrap();
    assert_eq!(metadata.status, CampaignStatus::Complete);
    assert!(metadata.finished_unix_ms.is_some());
    let entries = read_entries(&output.join("samples.jsonl")).unwrap();
    assert_eq!(entries.len(), 4);
    assert!(entries.iter().all(|entry| entry.error.is_none()
        && entry.stderr == "note\n"
        && entry.command[0].ends_with("/bin/0")));
    let pair = &campaign.summary.pairs[0];
    assert_eq!(
        (pair.kind, pair.metrics[0].improved, pair.metrics[0].rounds),
        (PairKind::Variant, 2, 2)
    );
    assert!((pair.metrics[0].change.unwrap().median + 0.1).abs() < 1e-9);
    assert!(campaign.report.contains("better 2/2"));
    let regenerated = summarize_directory(&output).unwrap();
    assert_eq!(regenerated.summary, campaign.summary);
    assert_eq!(
        std::fs::read(output.join("summary.json")).unwrap(),
        serde_json::to_vec_pretty(&campaign.summary)
            .map(|mut text| {
                text.push(b'\n');
                text
            })
            .unwrap()
    );
}

#[test]
fn invalid_samples_fail_the_campaign_and_are_retained() {
    let large = "head -c 17000000 /dev/zero";
    let cases = [
        ("exit 4", "exit code Some(4)"),
        ("echo not-json", "not exactly one process record"),
        (large, "exceeded the record size limit"),
        (
            "echo '{\"schema\":\"base-benchmark/process-record\",\"version\":1,\"workload\":\"w\",\"parameters\":{},\"samples\":[]}'",
            "no samples",
        ),
        ("sleep 30", "timed out"),
    ];
    for (body, message) in cases {
        let directory = tempfile::tempdir().unwrap();
        let (mut plan, output) = campaign(directory.path(), body);
        plan.timeout_ms = 300;
        let error = run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        let metadata = read_metadata(&output).unwrap();
        assert_eq!(metadata.status, CampaignStatus::Failed);
        assert!(metadata.error.unwrap().contains(message));
        assert_eq!(
            read_entries(&output.join("samples.jsonl")).unwrap().len(),
            1
        );
        assert!(!output.join("summary.json").exists());
        assert!(
            summarize_directory(&output)
                .unwrap_err()
                .to_string()
                .contains("only complete campaigns")
        );
    }
}

#[test]
fn subjects_must_do_equivalent_work() {
    let directory = tempfile::tempdir().unwrap();
    let work = workload(directory.path(), "different");
    let mut plan = plan(
        &work,
        &["slow", "fast"],
        ROLES,
        &directory.path().join("lock"),
    );
    plan.rounds = 1;
    let error = run(
        plan,
        &directory.path().join("campaign"),
        &Cancellation::new(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("did not do equivalent work"),
        "{error}"
    );
}

#[test]
fn changed_executables_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let (mut plan, output) = campaign(directory.path(), "true");
    let executable = directory.path().join("frozen");
    std::fs::write(&executable, "before").unwrap();
    plan.artifacts.artifacts[0].sha256 = crate::artifact::sha256_file(&executable).unwrap();
    plan.artifacts.artifacts[0].executable = executable.clone();
    std::fs::write(&executable, "after").unwrap();
    let error = run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap_err();
    assert!(error.to_string().contains("changed after preparation"));
}

#[test]
fn cancellation_is_recorded_separately_from_failure() {
    let directory = tempfile::tempdir().unwrap();
    let (plan, output) = campaign(directory.path(), "sleep 30");
    let cancellation = Cancellation::new();
    let canceller = cancellation.clone();
    let stop = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        canceller.cancel();
    });
    let error = run(plan.clone(), &output, &cancellation, &mut Vec::new()).unwrap_err();
    stop.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(
        read_metadata(&output).unwrap().status,
        CampaignStatus::Cancelled
    );

    let output = directory.path().join("again");
    assert_eq!(
        run(plan, &output, &cancellation, &mut Vec::new())
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(
        read_entries(&output.join("samples.jsonl"))
            .unwrap()
            .is_empty()
    );
}

/// A fake `perf`: `perf stat --no-big-num -x, -e EVENTS -o FILE -- PROGRAM ARGS...` writes `counters` to FILE, then runs the workload.
fn fake_perf(directory: &Path, counters: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = directory.join(format!("perf-{}", counters.len()));
    std::fs::write(
        &path,
        format!("#!/bin/sh\noutput=$7\nshift 8\n{counters}\nexec \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn perf_counters_are_whole_process_metrics() {
    let directory = tempfile::tempdir().unwrap();
    let valid = record("same", 100);
    let cases = [
        ("printf '55,,instructions:u\\n' > \"$output\"", None),
        (":", Some("perf stat wrote no counters")),
        (
            "printf '<not counted>,,instructions:u\\n' > \"$output\"",
            Some("did not count"),
        ),
    ];
    for (index, (counters, failure)) in cases.into_iter().enumerate() {
        let (mut plan, _) = campaign(directory.path(), &format!("echo '{valid}'"));
        plan.perf_events = vec!["instructions:u".to_owned()];
        plan.perf_program = fake_perf(directory.path(), counters);
        let output = directory.path().join(format!("campaign-{index}"));
        let result = run(plan, &output, &Cancellation::new(), &mut Vec::new());
        match failure {
            None => {
                let campaign = result.unwrap();
                let metric = campaign.summary.subjects[0]
                    .metrics
                    .iter()
                    .find(|metric| metric.name == "instructions:u")
                    .unwrap();
                assert_eq!(metric.statistic, crate::summary::Statistic::Total);
                assert_eq!(metric.distribution.unwrap().median, 55.0);
                let entries = read_entries(&output.join("samples.jsonl")).unwrap();
                assert_eq!(entries[0].command[1], "stat");
            }
            Some(message) => assert!(result.unwrap_err().to_string().contains(message)),
        }
    }
}

#[test]
fn host_identity_reads_what_is_available() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join("proc/sys/kernel")).unwrap();
    std::fs::create_dir_all(root.join("sys/devices/system/cpu/cpu2/topology")).unwrap();
    std::fs::write(
        root.join("proc/cpuinfo"),
        "processor : 0\n\nmodel name\t: Test CPU\n",
    )
    .unwrap();
    std::fs::write(root.join("proc/sys/kernel/hostname"), "bench\n").unwrap();
    std::fs::write(root.join("proc/sys/kernel/osrelease"), "\n").unwrap();
    std::fs::write(
        root.join("sys/devices/system/cpu/cpu2/topology/thread_siblings_list"),
        "2,18\n",
    )
    .unwrap();
    let host = HostIdentity::capture_from(root, &[2]);
    assert_eq!(host.hostname.as_deref(), Some("bench"));
    assert_eq!(
        (host.kernel, host.cpu_model.as_deref()),
        (None, Some("Test CPU"))
    );
    assert_eq!(host.cpus[0].thread_siblings.as_deref(), Some("2,18"));
    assert_eq!(host.cpus[0].scaling_governor, None);
    std::fs::write(root.join("proc/cpuinfo"), "processor : 0\n").unwrap();
    assert_eq!(HostIdentity::capture_from(root, &[]).cpu_model, None);
    assert!(HostIdentity::capture(&[]).kernel.is_some());
}

#[test]
fn campaign_directories_must_be_new_and_readable() {
    let directory = tempfile::tempdir().unwrap();
    let (plan, _) = campaign(directory.path(), "true");
    assert!(
        run(
            plan.clone(),
            directory.path(),
            &Cancellation::new(),
            &mut Vec::new()
        )
        .unwrap_err()
        .to_string()
        .contains("new directory")
    );
    let mut recorded = plan;
    recorded.mode = crate::plan::Mode::Recorded;
    assert!(
        run(
            recorded,
            &directory.path().join("recorded"),
            &Cancellation::new(),
            &mut Vec::new()
        )
        .is_err()
    );
    assert!(read_metadata(directory.path()).is_err());
    std::fs::write(directory.path().join("metadata.json"), "{").unwrap();
    assert!(read_metadata(directory.path()).is_err());
    assert!(read_entries(&directory.path().join("missing.jsonl")).is_err());
    std::fs::write(directory.path().join("bad.jsonl"), "{}\n").unwrap();
    assert!(
        read_entries(&directory.path().join("bad.jsonl"))
            .unwrap_err()
            .to_string()
            .contains("line 1")
    );
    assert!(read_entries(directory.path()).is_err());
}

/// Whether file permissions stop this process; privileged users bypass them.
fn permissions_apply(directory: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let probe = directory.join("probe");
    std::fs::write(&probe, "").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::write(&probe, "x").is_err()
}

#[test]
fn journal_failures_are_reported_with_the_original_error() {
    let directory = tempfile::tempdir().unwrap();
    if !permissions_apply(directory.path()) {
        return;
    }
    let valid = record("same", 100);
    let cases = [
        (format!("chmod 555 \"$1\"; echo '{valid}'"), "summary.json"),
        (
            "chmod 555 \"$1\"; exit 2".to_owned(),
            "recording the failure also failed",
        ),
        (
            format!("chmod 444 \"$1/samples.jsonl\"; echo '{valid}'"),
            "samples.jsonl",
        ),
    ];
    for (index, (body, message)) in cases.into_iter().enumerate() {
        let output = directory.path().join(format!("campaign-{index}"));
        let (mut plan, _) = campaign(directory.path(), &body);
        plan.rounds = 1;
        // The campaign directory is the script's first argument.
        plan.subjects[0]
            .arguments
            .insert(1, output.display().to_string());
        let error = run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains(message), "{message}: {error}");
        std::fs::set_permissions(&output, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
    let (mut plan, _) = campaign(directory.path(), &format!("echo '{valid}'"));
    plan.rounds = 1;
    let output = directory.path().join("complete");
    run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o555)).unwrap();
    assert!(
        summarize_directory(&output)
            .unwrap_err()
            .to_string()
            .contains("summary.json")
    );
    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(output.join("samples.jsonl")).unwrap();
    assert!(
        summarize_directory(&output)
            .unwrap_err()
            .to_string()
            .contains("samples.jsonl")
    );
}

#[test]
fn unsigned_records_unusable_locks_and_missing_programs() {
    let directory = tempfile::tempdir().unwrap();
    let unsigned = record("same", 100).replace(r#""signature":"same","#, "");
    let (plan, output) = campaign(directory.path(), &format!("echo '{unsigned}'"));
    let campaign_result =
        run(plan.clone(), &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    assert_eq!(campaign_result.summary.subjects[0].processes, 2);

    let mut locked = plan.clone();
    locked.lock = directory.path().join("missing/lock");
    let error = run(
        locked,
        &directory.path().join("locked"),
        &Cancellation::new(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("missing/lock"));

    let mut perf = plan.clone();
    perf.perf_events = vec!["cycles:u".to_owned()];
    perf.perf_program = PathBuf::from("/nonexistent/perf");
    let error = run(
        perf,
        &directory.path().join("perf"),
        &Cancellation::new(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("/nonexistent/perf"));

    let file = directory.path().join("file");
    std::fs::write(&file, "").unwrap();
    assert!(
        run(
            plan,
            &file.join("campaign"),
            &Cancellation::new(),
            &mut Vec::new()
        )
        .unwrap_err()
        .to_string()
        .contains("new directory")
    );
}

fn variant_workload(directory: &Path, fast: &str) -> PathBuf {
    let slow = record("same", 1000);
    script(
        directory,
        "variants.sh",
        &format!("if [ \"$1\" = fast ]; then echo '{fast}'; else echo '{slow}'; fi\n"),
    )
}

#[test]
fn records_must_share_workload_signature_and_parameters() {
    let cases = [
        (
            record("same", 900).replace("\"workload\":\"fixture\"", "\"workload\":\"other\""),
            "did not do equivalent work",
        ),
        (
            record("same", 900).replace(r#""signature":"same","#, ""),
            "did not do equivalent work",
        ),
    ];
    for (fast, message) in cases {
        let directory = tempfile::tempdir().unwrap();
        let work = variant_workload(directory.path(), &fast);
        let mut plan = plan(
            &work,
            &["slow", "fast"],
            ROLES,
            &directory.path().join("lock"),
        );
        plan.rounds = 1;
        plan.seed = 1;
        let error = run(
            plan,
            &directory.path().join("campaign"),
            &Cancellation::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains(message), "{message}: {error}");
    }
}

#[test]
fn workloads_start_from_equal_length_staged_paths_with_stack_padding() {
    let directory = tempfile::tempdir().unwrap();
    let valid = record("same", 100);
    let work = script(
        directory.path(),
        "padded.sh",
        &format!("printf %s \"${{#BASE_BENCHMARK_STACK_PADDING}}\" >&2; pwd >&2; echo '{valid}'\n"),
    );
    // An executable owned by this user can be hard linked; `/bin/sh` usually cannot, so it is copied.
    let shell = directory.path().join("sh");
    std::fs::copy("/bin/sh", &shell).unwrap();
    let mut plan = plan(
        &work,
        &[],
        &[
            ("baseline", ArtifactRole::Baseline),
            ("candidate", ArtifactRole::Candidate),
        ],
        &directory.path().join("lock"),
    );
    plan.artifacts.artifacts[1] =
        crate::test_support::artifact("candidate", ArtifactRole::Candidate, &shell);
    for subject in &mut plan.subjects {
        if subject.artifact == "candidate" {
            subject.program = shell.clone();
        }
    }
    let output = directory.path().join("campaign");
    run(plan.clone(), &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    let entries = read_entries(&output.join("samples.jsonl")).unwrap();
    for entry in &entries {
        let padding = entry.stack_padding.unwrap();
        assert!(padding < crate::plan::STACK_PADDING_LIMIT);
        assert_eq!(entry.stderr, format!("{padding}/\n"));
        let staged = Path::new(&entry.command[0]);
        assert_eq!(staged.parent().unwrap(), output.join("bin"));
        assert_eq!(staged.file_name().unwrap().len(), 1);
    }
    assert_eq!(
        std::fs::metadata(output.join("bin/1")).unwrap().ino(),
        std::fs::metadata(&shell).unwrap().ino(),
        "owned executables are hard linked"
    );

    plan.stack_padding = false;
    let output = directory.path().join("unpadded");
    run(plan.clone(), &output, &Cancellation::new(), &mut Vec::new()).unwrap();
    assert!(
        read_entries(&output.join("samples.jsonl"))
            .unwrap()
            .iter()
            .all(|entry| entry.stack_padding.is_none() && entry.stderr == "0/\n")
    );

    plan.artifacts.artifacts[1].executable = directory.path().join("missing");
    let output = directory.path().join("unstaged");
    assert!(
        run(plan, &output, &Cancellation::new(), &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("candidate: cannot stage")
    );
}

#[test]
fn escaped_descendants_fail_the_campaign() {
    let directory = tempfile::tempdir().unwrap();
    let (plan, output) = campaign(directory.path(), "setsid sleep 10 & sleep 0.3; echo '{}'");
    let error = run(plan, &output, &Cancellation::new(), &mut Vec::new()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("left the workload's process group"),
        "{error}"
    );
}

#[test]
fn artifacts_must_run_a_variant_with_the_same_parameters() {
    let directory = tempfile::tempdir().unwrap();
    let normal = record("same", 100);
    let changed = normal.replace("\"parameters\":{}", "\"parameters\":{\"items\":2}");
    // The candidate's shell is staged as `bin/1`; its changed default shows up as different parameters.
    let work = script(
        directory.path(),
        "defaults.sh",
        &format!(
            "case \"$(readlink /proc/$$/exe)\" in */bin/1) echo '{changed}';; *) echo '{normal}';; esac\n"
        ),
    );
    let shell = directory.path().join("sh");
    std::fs::copy("/bin/sh", &shell).unwrap();
    let mut plan = plan(
        &work,
        &[],
        &[
            ("baseline", ArtifactRole::Baseline),
            ("candidate", ArtifactRole::Candidate),
        ],
        &directory.path().join("lock"),
    );
    plan.artifacts.artifacts[1] =
        crate::test_support::artifact("candidate", ArtifactRole::Candidate, &shell);
    let error = run(
        plan,
        &directory.path().join("campaign"),
        &Cancellation::new(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("parameters differ from another run of the same variant"),
        "{error}"
    );
}
