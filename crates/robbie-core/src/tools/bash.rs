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
#[derive(Debug, Default)]
struct TextBuffer {
    chars: VecDeque<char>,
    discarded: bool,
}

impl TextBuffer {
    fn push(&mut self, ch: char) {
        if self.chars.len() == MAX_CHARS + 1 {
            self.chars.pop_front();
            self.discarded = true;
        }
        self.chars.push_back(ch);
    }

    fn append(&mut self, other: Self) {
        self.discarded |= other.discarded;
        for ch in other.chars {
            self.push(ch);
        }
    }
}

#[derive(Debug, Default)]
struct Tail {
    // Whitespace is separate because trimming precedes truncation. Otherwise a
    // huge whitespace suffix could evict a short answer. All three are bounded;
    // leading/trailing whitespace is retained only to join stdout with stderr.
    leading: TextBuffer,
    body: TextBuffer,
    trailing: TextBuffer,
    // Only these raw boundary bytes can participate in a code point crossing
    // the stdout/stderr seam: up to three leading continuations and a partial EOF.
    prefix: Vec<u8>,
    suffix: Vec<u8>,
}

impl Tail {
    fn push_text(&mut self, text: &str) {
        for ch in text.chars() {
            if ch.is_whitespace() {
                if self.body.chars.is_empty() {
                    self.leading.push(ch);
                } else {
                    self.trailing.push(ch);
                }
            } else {
                self.body.append(std::mem::take(&mut self.trailing));
                self.body.push(ch);
            }
        }
    }

    fn append(&mut self, other: Self) {
        if self.body.chars.is_empty() {
            self.leading.append(other.leading);
        } else {
            self.trailing.append(other.leading);
        }
        if !other.body.chars.is_empty() {
            self.body.append(std::mem::take(&mut self.trailing));
            self.body.append(other.body);
        }
        self.trailing.append(other.trailing);
    }

    // Return consumed bytes, leaving only an incomplete UTF-8 code point for
    // the next read. Invalid sequences use the same replacement as from_utf8_lossy.
    fn decode(&mut self, bytes: &[u8]) -> usize {
        let mut consumed = 0;
        while consumed < bytes.len() {
            match std::str::from_utf8(&bytes[consumed..]) {
                Ok(text) => {
                    self.push_text(text);
                    return bytes.len();
                }
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    self.push_text(
                        std::str::from_utf8(&bytes[consumed..end]).expect("validated UTF-8 prefix"),
                    );
                    consumed = end;
                    match error.error_len() {
                        Some(len) => {
                            self.push_text("�");
                            consumed += len;
                        }
                        None => break,
                    }
                }
            }
        }
        consumed
    }
}

async fn read_tail(mut pipe: impl AsyncRead + Unpin) -> io::Result<Tail> {
    let mut tail = Tail::default();
    let mut chunk = [0; 8192];
    let mut pending = Vec::new();
    let mut at_start = true;
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            tail.suffix = pending;
            return Ok(tail);
        }
        let mut start = 0;
        while at_start && start < read {
            if tail.prefix.len() < 3 && (0x80..=0xbf).contains(&chunk[start]) {
                tail.prefix.push(chunk[start]);
                start += 1;
            } else {
                at_start = false;
            }
        }
        pending.extend_from_slice(&chunk[start..read]);
        let consumed = tail.decode(&pending);
        pending.drain(..consumed);
        debug_assert!(pending.len() < 4);
    }
}

fn output_text(mut stdout: Tail, mut stderr: Tail) -> (String, bool) {
    let mut content = Tail::default();
    content.push_text(&String::from_utf8_lossy(&std::mem::take(
        &mut stdout.prefix,
    )));
    let mut seam = std::mem::take(&mut stdout.suffix);
    seam.extend(std::mem::take(&mut stderr.prefix));
    let suffix = std::mem::take(&mut stderr.suffix);
    content.append(stdout);
    content.push_text(&String::from_utf8_lossy(&seam));
    content.append(stderr);
    content.push_text(&String::from_utf8_lossy(&suffix));
    let mut body = content.body;
    let truncated = body.discarded || body.chars.len() > MAX_CHARS;
    if body.chars.len() > MAX_CHARS {
        body.chars.pop_front();
    }
    (body.chars.into_iter().collect(), truncated)
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
                let (result, truncated) = output_text(output.stdout, output.stderr);
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

    #[tokio::test]
    async fn meaningful_output_survives_excessive_trailing_whitespace() {
        let mut bash = Bash(Arc::new(std::env::current_dir().unwrap()));
        for (command, expected) in [
            ("printf '\\303\\251'; printf '%200006s' ''", "é\n[exit 0]"),
            ("printf answer; printf '%300000s' ''", "answer\n[exit 0]"),
            (
                "printf '%300000s' ''; printf answer >&2",
                "answer\n[exit 0]",
            ),
            (
                "printf answer; printf '%300000s' '' >&2",
                "answer\n[exit 0]",
            ),
        ] {
            assert_eq!(
                bash.execute(&json!({"command":command})).await,
                (expected.into(), false)
            );
        }
    }

    #[tokio::test]
    async fn raw_stdout_stderr_boundary_matches_concatenate_then_decode() {
        let mut bash = Bash(Arc::new(std::env::current_dir().unwrap()));
        assert_eq!(
            bash.execute(&json!({"command":"printf '\\303'; printf '\\251' >&2"}))
                .await,
            ("é\n[exit 0]".into(), false)
        );
        let cases: &[&[u8]] = &[
            b"",
            b"\xc3",
            b"\xa9",
            b"\xf0\x9f",
            b"\x91\xa9",
            b"a\xe2\x80",
            b"\x83b",
            b"\x80 \t",
            b"\x80\x80\x80\x80",
            b"\xf0",
            b" \xc3",
            b"\xff",
            b"\xe2\x80",
            b"\x83after",
        ];
        for stdout in cases {
            for stderr in cases {
                let bytes = [*stdout, *stderr].concat();
                let expected = String::from_utf8_lossy(&bytes).trim().to_string();
                let actual = output_text(
                    read_tail(*stdout).await.unwrap(),
                    read_tail(*stderr).await.unwrap(),
                );
                assert_eq!(
                    actual,
                    (expected, false),
                    "stdout={stdout:?}, stderr={stderr:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn bounded_capture_matches_trim_then_character_tail_for_both_streams() {
        let cases = [
            String::new(),
            " \té\n猫 \n".into(),
            "é".repeat(MAX_CHARS + 1),
            " ".repeat(MAX_CHARS * 2),
            format!("answer{}", " ".repeat(MAX_CHARS * 2)),
            format!("{}end", " ".repeat(MAX_CHARS * 2)),
            format!("start{}end", "\u{2003}".repeat(MAX_CHARS * 2)),
        ];
        for stdout in &cases {
            for stderr in &cases {
                let original = format!("{stdout}{stderr}");
                let trimmed = original.trim();
                let expected: String = trimmed
                    .chars()
                    .rev()
                    .take(MAX_CHARS)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let actual = output_text(
                    read_tail(stdout.as_bytes()).await.unwrap(),
                    read_tail(stderr.as_bytes()).await.unwrap(),
                );
                assert_eq!(actual, (expected, trimmed.chars().count() > MAX_CHARS));
            }
        }
    }

    #[tokio::test]
    async fn incremental_lossy_decoding_preserves_codepoints_across_reads() {
        let mut bytes = vec![b'a'; 8191];
        bytes.extend_from_slice("é猫👩‍💻".as_bytes());
        bytes.extend_from_slice(&[0xff, 0xc3]);
        let expected = String::from_utf8_lossy(&bytes).into_owned();
        assert_eq!(
            output_text(read_tail(bytes.as_slice()).await.unwrap(), Tail::default()),
            (expected, false)
        );
    }

    #[test]
    fn tail_storage_is_bounded_across_small_and_oversized_chunks() {
        let mut tail = Tail::default();
        for _ in 0..100 {
            tail.push_text(&"x".repeat(8192));
            assert!(tail.body.chars.len() <= MAX_CHARS + 1);
        }
        assert!(tail.body.discarded);
        tail.push_text(&"y".repeat(MAX_CHARS * 2));
        assert_eq!(tail.body.chars.len(), MAX_CHARS + 1);
        assert!(tail.body.chars.iter().all(|ch| *ch == 'y'));
        tail.push_text(&" ".repeat(MAX_CHARS * 2));
        assert_eq!(tail.trailing.chars.len(), MAX_CHARS + 1);
        let mut leading = Tail::default();
        leading.push_text(&" ".repeat(MAX_CHARS * 2));
        assert_eq!(leading.leading.chars.len(), MAX_CHARS + 1);
    }

    #[tokio::test]
    async fn noisy_stdout_and_stderr_are_drained_with_bounded_storage() {
        let output = run(
            "(awk 'BEGIN { for (i=0;i<100000;i++) print \"stdout\" }'; printf OUT_TAIL) & (awk 'BEGIN { for (i=0;i<100000;i++) print \"stderr\" }'; printf ERR_TAIL) >&2 & wait",
            Duration::from_secs(10),
        ).await.unwrap().unwrap();
        assert!(output.status.success());
        for (tail, suffix) in [(output.stdout, "OUT_TAIL"), (output.stderr, "ERR_TAIL")] {
            assert!(tail.body.discarded);
            assert_eq!(tail.body.chars.len(), MAX_CHARS + 1);
            let text: String = tail.body.chars.into_iter().collect();
            assert!(text.ends_with(suffix));
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
