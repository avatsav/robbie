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

fn configured_child(root: &Path, opt_in: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new(env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "diagnostics::tests::configuration::configured_start_fixture",
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

mod storage;

mod records;

mod failures;

mod configuration;
