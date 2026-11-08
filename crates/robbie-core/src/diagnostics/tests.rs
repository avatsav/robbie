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

mod storage;

mod records;

mod failures;
