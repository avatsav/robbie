use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

use super::Tool;

pub(super) struct Bash(pub(super) Arc<PathBuf>);

const MAX_CHARS: usize = 50_000;
// Four UTF-8 bytes per retained character, plus lookahead/boundary slack.
const MAX_BYTES: usize = (MAX_CHARS + 1) * 4 + 3;

#[derive(Debug, Default)]
struct Tail {
    bytes: VecDeque<u8>,
    discarded: bool,
}

impl Tail {
    fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= MAX_BYTES {
            self.discarded |= !self.bytes.is_empty() || bytes.len() > MAX_BYTES;
            self.bytes.clear();
            self.bytes.extend(&bytes[bytes.len() - MAX_BYTES..]);
        } else {
            let excess = (self.bytes.len() + bytes.len()).saturating_sub(MAX_BYTES);
            self.discarded |= excess > 0;
            self.bytes.drain(..excess);
            self.bytes.extend(bytes);
        }
    }
}

async fn read_tail(mut pipe: impl AsyncRead + Unpin) -> io::Result<Tail> {
    let mut tail = Tail::default();
    let mut chunk = [0; 8192];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            return Ok(tail);
        }
        tail.push(&chunk[..read]);
    }
}

#[derive(Debug)]
struct Captured {
    status: ExitStatus,
    stdout: Tail,
    stderr: Tail,
}

// Own the group through output collection, not just the shell's lifetime:
// descendants can keep its pipes open after the shell has already exited.
#[cfg(unix)]
struct ProcessGroup(libc::pid_t);

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: spawn created a new group with this positive child PID. A
        // negative PID targets only that group, never Robbie's terminal group.
        // SIGKILL also handles commands that ignore graceful termination.
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
    }
}

async fn run(command: &str, timeout: Duration, workspace: &Path) -> io::Result<Option<Captured>> {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    process.process_group(0);
    let mut child = process.spawn()?;
    #[cfg(unix)]
    let _group = ProcessGroup(
        libc::pid_t::try_from(child.id().expect("a newly spawned child has a PID"))
            .map_err(io::Error::other)?,
    );
    // On cancellation, kill_on_drop kills the direct child and Tokio's orphan
    // reaper handles wait cleanup. The guard also kills its ordinary descendants.
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let collect = async {
        let (status, stdout, stderr) =
            tokio::try_join!(child.wait(), read_tail(stdout), read_tail(stderr))?;
        Ok(Captured {
            status,
            stdout,
            stderr,
        })
    };
    match tokio::time::timeout(timeout, collect).await {
        Ok(output) => output.map(Some),
        Err(_) => {
            // Reap the direct child explicitly on timeout. The group guard also
            // covers descendants, including when wait() already reaped the shell.
            let _ = child.kill().await;
            Ok(None)
        }
    }
}

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn schema(&self) -> Value {
        json!({
            "name": self.name(),
            "description": "Run a shell command in the current workspace.",
            "input_schema": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }
        })
    }

    async fn execute(&mut self, input: &Value) -> (String, bool) {
        let Some(command) = input.get("command").and_then(Value::as_str) else {
            return ("Missing or invalid command".to_string(), true);
        };
        match run(command, Duration::from_secs(120), &self.0).await {
            Ok(Some(output)) => {
                let discarded = output.stdout.discarded || output.stderr.discarded;
                // Preserve the existing stdout-then-stderr presentation; both
                // acquisitions and this combined allocation are bounded.
                let combined: Vec<_> = output
                    .stdout
                    .bytes
                    .into_iter()
                    .chain(output.stderr.bytes)
                    .collect();
                let text = String::from_utf8_lossy(&combined);
                let text = text.trim();
                // Keep the end of long command output: failures and summaries
                // usually live there, and the UI previews this same tail.
                let mut tail: Vec<_> = text.chars().rev().take(MAX_CHARS + 1).collect();
                let truncated = discarded || tail.len() > MAX_CHARS;
                tail.truncate(MAX_CHARS);
                let result: String = tail.into_iter().rev().collect();
                let result = if result.is_empty() {
                    "(no output)".to_string()
                } else {
                    result
                };
                let result = if truncated {
                    format!("[earlier output truncated]\n{result}")
                } else {
                    result
                };
                let status = match output.status.code() {
                    Some(code) => format!("[exit {code}]"),
                    None => format!("[{}]", output.status),
                };
                (format!("{result}\n{status}"), !output.status.success())
            }
            Err(error) => (format!("Could not run command: {error}"), true),
            Ok(None) => ("Command timed out after 120 seconds".to_string(), true),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    async fn run(command: &str, timeout: Duration) -> io::Result<Option<Captured>> {
        super::run(command, timeout, Path::new(".")).await
    }

    #[test]
    fn tail_storage_is_bounded_across_small_and_oversized_chunks() {
        let mut tail = Tail::default();
        for _ in 0..100 {
            tail.push(&[b'x'; 8192]);
            assert!(tail.bytes.len() <= MAX_BYTES);
        }
        assert!(tail.discarded);
        tail.push(&vec![b'y'; MAX_BYTES * 2]);
        assert_eq!(tail.bytes.len(), MAX_BYTES);
        assert!(tail.bytes.iter().all(|byte| *byte == b'y'));
    }

    #[tokio::test]
    async fn noisy_stdout_and_stderr_are_drained_with_bounded_storage() {
        let output = run(
            "(awk 'BEGIN { for (i=0;i<100000;i++) print \"stdout\" }'; printf OUT_TAIL) & (awk 'BEGIN { for (i=0;i<100000;i++) print \"stderr\" }'; printf ERR_TAIL) >&2 & wait",
            Duration::from_secs(10),
        ).await.unwrap().unwrap();
        assert!(output.status.success());
        for (tail, suffix) in [(output.stdout, "OUT_TAIL"), (output.stderr, "ERR_TAIL")] {
            assert!(tail.discarded);
            assert_eq!(tail.bytes.len(), MAX_BYTES);
            let bytes: Vec<_> = tail.bytes.into_iter().collect();
            assert!(bytes.ends_with(suffix.as_bytes()));
        }
    }

    fn descendant_command(ready: &Path, marker: &Path, wait: bool) -> String {
        format!(
            "(printf ready > '{}'; sleep 0.5; printf survived > '{}') & {}",
            ready.display(),
            marker.display(),
            if wait { "wait" } else { "exit 0" },
        )
    }

    async fn ready(path: &Path) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant did not start");
    }

    #[tokio::test]
    async fn timeout_stops_descendants_after_shell_has_exited() {
        let scratch = tempfile::tempdir().unwrap();
        let started = scratch.path().join("ready");
        let marker = scratch.path().join("survived");
        // The shell exits immediately, but its child still holds the output pipes.
        let command = descendant_command(&started, &marker, false);
        let result = run(&command, Duration::from_millis(200)).await.unwrap();
        assert!(result.is_none());
        assert!(started.exists());
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!marker.exists(), "descendant outlived the command timeout");
    }

    #[tokio::test]
    async fn dropping_command_stops_shell_and_descendants() {
        let scratch = tempfile::tempdir().unwrap();
        let started = scratch.path().join("ready");
        let marker = scratch.path().join("survived");
        let command = descendant_command(&started, &marker, true);
        let mut pending = Box::pin(run(&command, Duration::from_secs(10)));
        tokio::select! {
            result = &mut pending => panic!("command finished before cancellation: {result:?}"),
            _ = ready(&started) => {}
        }
        drop(pending);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!marker.exists(), "descendant outlived cancellation");
    }
}
