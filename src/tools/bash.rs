use std::{
    io,
    process::{Output, Stdio},
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use super::Tool;

pub(super) struct Bash;

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

async fn run(command: &str, timeout: Duration) -> io::Result<Option<Output>> {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    process.process_group(0);
    let child = process.spawn()?;
    #[cfg(unix)]
    let _group = ProcessGroup(
        libc::pid_t::try_from(child.id().expect("a newly spawned child has a PID"))
            .map_err(io::Error::other)?,
    );
    // On cancellation, kill_on_drop kills the direct child and Tokio's orphan
    // reaper handles wait cleanup. The guard also kills its ordinary descendants.
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(output) => output.map(Some),
        Err(_) => Ok(None),
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
        match run(command, Duration::from_secs(120)).await {
            Ok(Some(output)) => {
                let combined = [output.stdout, output.stderr].concat();
                let text = String::from_utf8_lossy(&combined);
                let text = text.trim();
                // Keep the end of long command output: failures and summaries
                // usually live there, and the UI previews this same tail.
                let mut tail: Vec<_> = text.chars().rev().take(50_001).collect();
                let truncated = tail.len() > 50_000;
                tail.truncate(50_000);
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
    use std::path::Path;

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
