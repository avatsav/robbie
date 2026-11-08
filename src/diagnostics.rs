//! A diagnostic journal, not resumable conversation history.
//!
//! Metadata is an allowlist. Content has a separate, opt-in serialization path;
//! callers must never supply authentication headers or environment dumps.
use std::{
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

pub mod activity;
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

#[derive(Clone, Copy, Default)]
pub enum CapturePolicy {
    #[default]
    MetadataOnly,
    Transcript,
}

#[derive(Default)]
pub struct RecordingSettings {
    /// Resolved absolute state directory. None disables recording with a warning.
    pub state_directory: Option<PathBuf>,
    pub capture: CapturePolicy,
}

struct Config {
    state: PathBuf,
    transcript: bool,
}

#[derive(Clone)]
pub struct Recorder {
    journal: Arc<Mutex<Journal>>,
}

impl Recorder {
    pub fn start(model: &str, workspace: &Path, settings: RecordingSettings) -> Self {
        let journal = settings
            .state_directory
            .ok_or_else(|| io::Error::other("no state directory available"))
            .and_then(|state| {
                Journal::open(
                    Config {
                        state,
                        transcript: matches!(settings.capture, CapturePolicy::Transcript),
                    },
                    workspace,
                )
            })
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

    #[cfg(all(unix, any(test, feature = "test-support")))]
    pub(crate) fn break_writer_for_test(&self) {
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
                    &std::env::current_dir().unwrap(),
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
        if !config.state.is_absolute() {
            return Err(io::Error::other("state directory must be absolute"));
        }
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
mod tests;
