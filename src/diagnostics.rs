//! A diagnostic journal, not resumable conversation history.
//!
//! Metadata is an allowlist. Content has a separate, opt-in serialization path;
//! callers must never supply authentication headers or environment dumps.
use std::{
    env,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::Value;

pub mod agent;
pub mod frontend;
pub mod model;
pub mod tool;

const WARNING: &str = "Diagnostics recording failed and is disabled for this session. Agent execution is unaffected; a partial final record may remain.";
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

pub struct TranscriptContext {
    pub system: String,
    pub tools: Vec<Value>,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Metadata<'a> {
    SessionStarted {
        model: &'a str,
        transcript_enabled: bool,
    },
    SessionContext,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Content<'a> {
    SessionContext {
        system: String,
        tools: Vec<Value>,
    },
    ProtocolRequest {
        body: &'a Value,
    },
    ProtocolResponse {
        body: model::Body<'a>,
    },
    HttpErrorResponse {
        body: model::Body<'a>,
    },
    ToolCall {
        tool_call_id: &'a str,
        name: &'a str,
        arguments: &'a Value,
    },
    ToolResult {
        output: &'a str,
    },
    UserPrompt {
        text: &'a str,
    },
    UiText {
        text: &'a str,
    },
}

#[derive(Serialize)]
struct Record<'a, M> {
    version: u8,
    session_id: &'a str,
    sequence: u64,
    timestamp_ms: u128,
    #[serde(flatten)]
    metadata: M,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Content<'a>>,
}

struct Config {
    state: PathBuf,
    transcript: bool,
}

impl Config {
    fn from_env() -> io::Result<Self> {
        let root = state_home(
            env::var_os("XDG_STATE_HOME").map(PathBuf::from).as_deref(),
            env::var_os("HOME").map(PathBuf::from).as_deref(),
        )?;
        Ok(Self {
            state: root,
            // Only this exact value opts in; typos must not enable content.
            transcript: env::var_os("ROBBIE_TRANSCRIPT").is_some_and(|value| value == "1"),
        })
    }
}

fn state_home(xdg: Option<&Path>, home: Option<&Path>) -> io::Result<PathBuf> {
    if let Some(path) = xdg.filter(|path| path.is_absolute()) {
        return Ok(path.to_owned());
    }
    home.filter(|path| path.is_absolute())
        .map(|path| path.join(".local").join("state"))
        .ok_or_else(|| io::Error::other("no absolute state directory available"))
}

#[derive(Clone)]
pub struct Recorder {
    journal: Arc<Mutex<Journal>>,
}

impl Recorder {
    pub fn start(model: &str, workspace: &Path) -> Self {
        let journal = Config::from_env()
            .and_then(|config| Journal::open(config, workspace))
            .unwrap_or_else(|_| Journal::disabled());
        let recorder = Self {
            journal: Arc::new(Mutex::new(journal)),
        };
        let transcript_enabled = recorder.lock().transcript;
        recorder.record(
            Metadata::SessionStarted {
                model,
                transcript_enabled,
            },
            || None,
        );
        recorder
    }

    pub fn session_context(&self, context: impl FnOnce() -> TranscriptContext) {
        if !self.lock().transcript {
            return;
        }
        self.record(Metadata::SessionContext, || {
            let context = context();
            Some(Content::SessionContext {
                system: context.system,
                tools: context.tools,
            })
        });
    }

    pub fn description(&self) -> Option<String> {
        self.lock().description()
    }

    pub fn take_warning(&self) -> Option<&'static str> {
        self.lock().take_warning()
    }

    fn lock(&self) -> MutexGuard<'_, Journal> {
        match self.journal.lock() {
            Ok(journal) => journal,
            Err(poisoned) => {
                let mut journal = poisoned.into_inner();
                journal.writer = None;
                journal.warning = Some(WARNING);
                self.journal.clear_poison();
                journal
            }
        }
    }

    fn record<'a>(&self, metadata: impl Serialize, content: impl FnOnce() -> Option<Content<'a>>) {
        self.lock().record(metadata, content);
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            journal: Arc::new(Mutex::new(Journal::disabled())),
        }
    }

    #[cfg(all(test, unix))]
    pub fn break_writer_for_test(&self) {
        let mut journal = self.lock();
        // A read-only descriptor deterministically rejects writes on both macOS
        // and Linux, without changing process permissions or depending on /dev/full.
        journal.writer = Some(Box::new(File::open(journal.path().unwrap()).unwrap()));
    }

    #[cfg(all(test, unix))]
    pub fn for_test(state: &Path, transcript: bool) -> Self {
        Self {
            journal: Arc::new(Mutex::new(
                Journal::open(
                    Config {
                        state: state.to_owned(),
                        transcript,
                    },
                    &env::current_dir().unwrap(),
                )
                .unwrap(),
            )),
        }
    }
}

struct Journal {
    // The private writer seam also permits deterministic write/flush failures in tests.
    writer: Option<Box<dyn Write + Send>>,
    path: Option<PathBuf>,
    session_id: String,
    sequence: u64,
    next_request: u64,
    next_turn: u64,
    next_tool: u64,
    transcript: bool,
    warning: Option<&'static str>,
}

impl Journal {
    fn description(&self) -> Option<String> {
        self.writer.as_ref()?;
        let mode = if self.transcript {
            "SENSITIVE transcript capture enabled"
        } else {
            "metadata only"
        };
        self.path().map(|path| {
            format!(
                "Diagnostics: {} ({mode}; retained until deleted)",
                path.display()
            )
        })
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn take_warning(&mut self) -> Option<&'static str> {
        self.warning.take()
    }

    fn disabled() -> Self {
        Self {
            writer: None,
            path: None,
            session_id: String::new(),
            sequence: 0,
            next_request: 0,
            next_turn: 0,
            next_tool: 0,
            transcript: false,
            warning: Some(WARNING),
        }
    }

    fn open(config: Config, workspace: &Path) -> io::Result<Self> {
        // Do not silently offer weaker file protection on unsupported platforms.
        #[cfg(not(unix))]
        return Err(io::Error::other("private diagnostic files require Unix"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            fs::create_dir_all(&config.state)?;
            // Resolve symlinked ancestors and '..' before creating/hardening
            // Robbie directories. Refusing a workspace must not chmod it.
            let application = fs::canonicalize(&config.state)?.join("robbie");
            let directory = application.join("sessions");
            if directory.starts_with(fs::canonicalize(workspace)?) {
                return Err(io::Error::other(
                    "diagnostics must be outside the workspace",
                ));
            }
            private_directory(&application)?;
            private_directory(&directory)?;
            // Opaque identity, not an authentication token. Exclusive creation below
            // prevents overwriting another session even if this identity collides.
            let nanos = now()?.as_nanos();
            let session_id = format!(
                "{nanos:x}-{:x}-{:x}",
                std::process::id(),
                NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
            );
            let path = directory.join(format!("{session_id}.jsonl"));
            let file: File = OpenOptions::new()
                .create_new(true)
                .append(true)
                .mode(0o600)
                .open(&path)?;
            Ok(Self {
                writer: Some(Box::new(file)),
                path: Some(path),
                session_id,
                sequence: 0,
                next_request: 0,
                next_turn: 0,
                next_tool: 0,
                transcript: config.transcript,
                warning: None,
            })
        }
    }

    fn record<'a>(
        &mut self,
        metadata: impl Serialize,
        content: impl FnOnce() -> Option<Content<'a>>,
    ) {
        if self.writer.is_none() {
            return;
        }
        if self.append(metadata, content).is_err() {
            // Never retry a partial append or leak OS error text/path/payloads.
            // Recording is ancillary: no Result escapes into agent/tool control flow.
            self.writer = None;
            self.warning = Some(WARNING);
        }
    }

    fn append<'a>(
        &mut self,
        metadata: impl Serialize,
        content: impl FnOnce() -> Option<Content<'a>>,
    ) -> io::Result<()> {
        let record = Record {
            version: 1,
            session_id: &self.session_id,
            sequence: self.sequence + 1,
            timestamp_ms: now()?.as_millis(),
            metadata,
            content: if self.transcript { content() } else { None },
        };
        // Serialize before touching the file. Embedded newlines are escaped, so
        // each successful append remains one complete JSON object plus newline.
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        if let Some(writer) = &mut self.writer {
            writer.write_all(&line)?;
            writer.flush()?;
            // This is incremental visibility, not fsync/power-loss durability.
            self.sequence += 1;
        }
        Ok(())
    }
}

fn now() -> io::Result<std::time::Duration> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::other("system clock predates Unix epoch"))
}

#[cfg(unix)]
fn private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    // Do not follow a symlink in either Robbie-owned directory or chmod its target.
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::other(
            "diagnostic directory is not a real directory",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(all(test, unix))]
mod tests {
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
                        let mut request =
                            recorder.model_request("test", "test/model", &Value::Null);
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
}
