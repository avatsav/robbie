use super::*;
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};

fn recorder(root: &Path, transcript: bool) -> Journal {
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    Journal::open(
        Config {
            state: root.join("state"),
            transcript,
        },
        &workspace,
    )
    .unwrap()
}

fn started(recorder: &mut Journal, model: &str, content: impl FnOnce() -> Content<'static>) {
    recorder.record(
        Metadata::SessionStarted {
            model,
            transcript_enabled: recorder.transcript,
        },
        || Some(content()),
    );
}

fn context() -> Content<'static> {
    Content::SessionContext {
        system: "sensitive system\nsecond line".into(),
        tools: vec![json!({"name": "bash", "description": "sensitive tool"})],
    }
}

fn records(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn state_location_requires_absolute_paths_and_never_uses_workspace() {
    assert_eq!(
        state_home(Some(Path::new("/state")), Some(Path::new("/home/me"))).unwrap(),
        Path::new("/state")
    );
    for xdg in [None, Some(Path::new("")), Some(Path::new("relative"))] {
        assert_eq!(
            state_home(xdg, Some(Path::new("/home/me"))).unwrap(),
            Path::new("/home/me/.local/state")
        );
    }
    assert!(state_home(None, None).is_err());
    assert!(state_home(Some(Path::new("relative")), Some(Path::new("relative"))).is_err());
}

#[test]
fn metadata_only_never_evaluates_or_serializes_content() {
    let root = tempfile::tempdir().unwrap();
    let mut recorder = recorder(root.path(), false);
    started(&mut recorder, "test/model", || {
        panic!("content must not be assembled")
    });
    let records = records(recorder.path().unwrap());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "session_started");
    assert_eq!(records[0]["transcript_enabled"], false);
    assert!(records[0].get("content").is_none());
    let mut keys: Vec<_> = records[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "event",
            "model",
            "sequence",
            "session_id",
            "timestamp_ms",
            "transcript_enabled",
            "version"
        ]
    );
}

#[test]
fn opt_in_records_content_as_one_line_without_modifying_it() {
    let root = tempfile::tempdir().unwrap();
    let mut recorder = recorder(root.path(), true);
    started(&mut recorder, "test/model", context);
    let records = records(recorder.path().unwrap());
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0]["content"]["system"],
        "sensitive system\nsecond line"
    );
    assert_eq!(records[0]["content"]["tools"][0]["name"], "bash");
    assert_eq!(records[0]["transcript_enabled"], true);
}

#[test]
fn appends_are_visible_while_open_and_have_stable_identity_and_sequence() {
    let root = tempfile::tempdir().unwrap();
    let mut recorder = recorder(root.path(), false);
    let path = recorder.path().unwrap().to_owned();
    started(&mut recorder, "first", context);
    assert_eq!(records(&path).len(), 1);
    let prefix = fs::read(&path).unwrap();
    started(&mut recorder, "second", context);
    assert!(fs::read(&path).unwrap().starts_with(&prefix));
    let records = records(&path);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["sequence"], 1);
    assert_eq!(records[1]["sequence"], 2);
    assert_eq!(records[0]["session_id"], records[1]["session_id"]);
    assert!(records[0]["timestamp_ms"].as_u64().unwrap() > 0);
    assert_eq!(records[0]["version"], 1);
    assert!(fs::read(&path).unwrap().ends_with(b"\n"));
}

#[test]
fn separate_sessions_do_not_overwrite_each_other() {
    let root = tempfile::tempdir().unwrap();
    let mut first = recorder(root.path(), false);
    let mut second = recorder(root.path(), false);
    assert_ne!(first.path(), second.path());
    started(&mut first, "first", context);
    started(&mut second, "second", context);
    assert_eq!(records(first.path().unwrap())[0]["model"], "first");
    assert_eq!(records(second.path().unwrap())[0]["model"], "second");
}

#[test]
fn directories_and_files_are_owner_only_even_when_directories_exist() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("state/robbie/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o755)).unwrap();
    let recorder = recorder(root.path(), false);
    for path in [sessions.as_path(), sessions.parent().unwrap()] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        fs::metadata(recorder.path().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn refuses_in_workspace_storage_including_symlinked_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&workspace, &alias).unwrap();
    for state in [workspace.clone(), alias] {
        assert!(
            Journal::open(
                Config {
                    state,
                    transcript: false
                },
                &workspace
            )
            .is_err()
        );
    }
    assert!(!workspace.join("robbie").exists());
}

#[test]
fn refusing_storage_does_not_change_workspace_permissions() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let workspace = state.join("robbie");
    fs::create_dir_all(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        Journal::open(
            Config {
                state,
                transcript: false
            },
            &workspace
        )
        .is_err()
    );
    assert_eq!(
        fs::metadata(&workspace).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(!workspace.join("sessions").exists());
}

#[test]
fn refuses_symlinks_in_robbie_owned_directories_without_chmod_target() {
    for suffix in ["robbie", "robbie/sessions"] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let state = root.path().join("state");
        let link = state.join(suffix);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            Journal::open(
                Config {
                    state,
                    transcript: false
                },
                &workspace
            )
            .is_err()
        );
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

struct FailingWriter {
    writes: Arc<Mutex<usize>>,
    fail_flush: bool,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        *self.writes.lock().unwrap() += 1;
        if self.fail_flush {
            Ok(bytes.len())
        } else {
            Err(io::Error::other(
                "secret payload /private/path?token=secret",
            ))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("secret flush failure"))
    }
}

#[test]
fn write_and_flush_failure_warn_once_disable_recording_and_do_not_retry() {
    for fail_flush in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut recorder = recorder(root.path(), true);
        let writes = Arc::new(Mutex::new(0));
        recorder.writer = Some(Box::new(FailingWriter {
            writes: Arc::clone(&writes),
            fail_flush,
        }));
        started(&mut recorder, "test", context);
        let warning = recorder.take_warning().unwrap();
        assert_eq!(warning, WARNING);
        assert!(!warning.contains("secret"));
        assert!(!warning.contains("/private"));
        assert!(recorder.take_warning().is_none());
        started(&mut recorder, "ignored", || {
            panic!("disabled recorder must not evaluate content")
        });
        assert_eq!(*writes.lock().unwrap(), 1);
        assert_eq!(recorder.sequence, 0);
        assert!(recorder.writer.is_none());
        assert!(recorder.take_warning().is_none());
    }
}

// Child-only fixture: changing environment in the parent would race other
// tests (and is unsafe in Rust 2024). No credentials or model calls here.
#[test]
#[ignore = "subprocess fixture for recorder configuration"]
fn configured_start_fixture() {
    let recorder = Recorder::start("fixture/model", &env::current_dir().unwrap());
    recorder.session_context(|| TranscriptContext {
        system: "fixture sensitive system".into(),
        tools: vec![json!({"name": "fixture_tool"})],
    });
    if let Some(warning) = recorder.take_warning() {
        println!("{warning}");
    } else {
        assert!(recorder.description().is_some());
    }
}

fn configured_child(root: &Path, opt_in: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new(env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "diagnostics::tests::configured_start_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("XDG_STATE_HOME", root)
        .env("HOME", "relative-invalid-home")
        .env("RECORDER_TEST_SECRET", "fixture environment secret")
        .env_remove("ROBBIE_TRANSCRIPT");
    if let Some(value) = opt_in {
        command.env("ROBBIE_TRANSCRIPT", value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn real_start_reads_configuration_and_only_exact_one_enables_content() {
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("1 "),
        Some("1"),
    ] {
        let root = tempfile::tempdir().unwrap();
        configured_child(root.path(), value);
        let paths: Vec<_> = fs::read_dir(root.path().join("robbie/sessions"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(paths.len(), 1);
        let lines = records(&paths[0]);
        let opt_in = value == Some("1");
        assert_eq!(lines.len(), if opt_in { 2 } else { 1 });
        assert_eq!(lines[0]["transcript_enabled"], opt_in);
        assert!(lines[0].get("content").is_none());
        if opt_in {
            assert_eq!(lines[1]["event"], "session_context");
            assert!(lines[1].get("content").is_some());
        }
        let bytes = fs::read_to_string(&paths[0]).unwrap();
        assert!(!bytes.contains("fixture environment secret"));
        assert_eq!(bytes.contains("fixture sensitive system"), opt_in);
    }
}

#[test]
fn real_start_failure_returns_normally_with_sanitized_warning() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("secret-location");
    fs::write(&blocked, "not a directory").unwrap();
    let output = configured_child(&blocked, Some("1"));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(WARNING));
    assert!(!stdout.contains("secret-location"));
    assert!(!stdout.contains("fixture sensitive system"));
    assert!(!stdout.contains("fixture environment secret"));
    assert_eq!(fs::read_to_string(&blocked).unwrap(), "not a directory");
}

#[test]
fn clones_share_one_sequence_and_unique_request_ids_under_concurrent_writes() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let recorder = recorder.clone();
            std::thread::spawn(move || {
                for _ in 0..8 {
                    let mut request = recorder.model_request("test", "test/model", &Value::Null);
                    request.finish(model::RequestOutcome::Succeeded);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let records = records(recorder.lock().path().unwrap());
    assert_eq!(records.len(), 64);
    let mut ids = std::collections::HashSet::new();
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record["sequence"], index + 1);
        assert_eq!(record["session_id"], records[0]["session_id"]);
        if record["event"] == "request_started" {
            assert!(ids.insert(record["request_id"].as_u64().unwrap()));
        }
    }
    assert_eq!(ids.len(), 32);
}

#[test]
fn poisoned_writer_is_disabled_and_warning_is_shared_and_consumed_once() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let other = recorder.clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _journal = other.lock();
            panic!("simulated panic while holding the journal");
        }))
        .is_err()
    );
    assert_eq!(recorder.take_warning(), Some(WARNING));
    assert!(other.take_warning().is_none());
    assert!(other.description().is_none());
    other.session_context(|| panic!("disabled recorder must not assemble content"));
    assert!(recorder.take_warning().is_none());
}

#[test]
fn opt_in_preserves_non_utf8_response_bytes_without_loss() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), true);
    let mut request = recorder.model_request("main", "test/model", &Value::Null);
    let mut attempt = request.attempt(1);
    attempt.status(200);
    attempt.finish(model::AttemptOutcome::InvalidJson, Some(&[255, 13, 10]));
    request.finish(model::RequestOutcome::ExchangeFailed);
    let records = records(recorder.lock().path().unwrap());
    assert_eq!(records[2]["content"]["body"], json!([255, 13, 10]));
}

#[test]
fn unavailable_recorder_warns_once_and_ignores_content() {
    let mut recorder = Journal::disabled();
    assert!(recorder.path().is_none());
    assert_eq!(recorder.take_warning(), Some(WARNING));
    assert!(recorder.take_warning().is_none());
    started(&mut recorder, "test", || {
        panic!("disabled recorder must not evaluate content")
    });
}
