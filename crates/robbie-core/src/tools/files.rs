use std::{
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::Tool;

pub(super) struct ReadFile(pub(super) Arc<PathBuf>);
pub(super) struct WriteFile(pub(super) Arc<PathBuf>);
pub(super) struct EditFile(pub(super) Arc<PathBuf>);

// Existing paths (including symlinks) are resolved before checking the boundary.
// For a new file, its parent must already exist inside the workspace.
fn workspace_path(root: &Path, path: &str, allow_new: bool) -> Result<PathBuf> {
    let root = root.canonicalize().context("cannot resolve workspace")?;
    let candidate = root.join(path);
    let resolved = match candidate.canonicalize() {
        Ok(path) => path,
        Err(error) if allow_new && error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink is not a new file: following it could escape the workspace.
            match std::fs::symlink_metadata(&candidate) {
                Ok(_) => bail!("cannot resolve existing path"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let parent = candidate
                .parent()
                .context("path has no parent")?
                .canonicalize()?;
            parent.join(candidate.file_name().context("path has no file name")?)
        }
        Err(error) => return Err(error.into()),
    };
    if !resolved.starts_with(&root) {
        bail!("path is outside the workspace");
    }
    Ok(resolved)
}

fn required<'a>(input: &'a Value, name: &str) -> Result<&'a str> {
    input
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing or invalid {name}"))
}

const MAX_READ_CHARS: usize = 50_000;

// Decode only the bounded prefix we return. BufReader limits read-ahead, even
// for a file with no newlines; no line-sized allocation or full-file read.
fn read_prefix(reader: impl Read, line_limit: Option<u64>) -> Result<String> {
    let mut output = String::new();
    if line_limit == Some(0) {
        return Ok(output);
    }
    let mut reader = BufReader::new(reader);
    let mut chars = 0;
    let mut lines = 0;
    loop {
        let mut bytes = [0; 4];
        let mut len = 0;
        let ch = loop {
            if reader.read(&mut bytes[len..len + 1])? == 0 {
                if len != 0 {
                    bail!("file prefix ends with incomplete UTF-8");
                }
                return Ok(output);
            }
            len += 1;
            match std::str::from_utf8(&bytes[..len]) {
                Ok(text) => break text.chars().next().expect("decoded a nonempty character"),
                Err(error) if error.error_len().is_none() && len < bytes.len() => {}
                Err(error) => return Err(error.into()),
            }
        };
        // One decoded character of lookahead distinguishes exact-fit from
        // truncated output. A reached line limit needs no further lookahead.
        if chars == MAX_READ_CHARS {
            output.push_str("\n[output truncated]");
            return Ok(output);
        }
        output.push(ch);
        chars += 1;
        if ch == '\n' {
            lines += 1;
            if line_limit == Some(lines) {
                return Ok(output);
            }
        }
    }
}

fn run_file_tool(name: &str, input: &Value, root: &Path) -> Result<String> {
    let path = required(input, "path")?;
    if path.is_empty() {
        bail!("path must not be empty");
    }
    match name {
        "read_file" => {
            let path = workspace_path(root, path, false)?;
            let limit = input
                .get("limit")
                .map(|value| {
                    value
                        .as_u64()
                        .context("limit must be a non-negative integer")
                })
                .transpose()?;
            read_prefix(std::fs::File::open(path)?, limit)
        }
        "write_file" => {
            let content = required(input, "content")?;
            let path = workspace_path(root, path, true)?;
            std::fs::write(path, content)?;
            Ok("File written".to_string())
        }
        "edit_file" => {
            let old = required(input, "old_text")?;
            let new = required(input, "new_text")?;
            if old.is_empty() {
                bail!("old_text must not be empty");
            }
            let path = workspace_path(root, path, false)?;
            let content = std::fs::read_to_string(&path)?;
            let count = content.match_indices(old).count();
            if count != 1 {
                bail!("old_text must match exactly once (found {count})");
            }
            std::fs::write(path, content.replacen(old, new, 1))?;
            Ok("File edited".to_string())
        }
        _ => bail!("unknown file tool"),
    }
}

async fn execute(name: &'static str, input: &Value, root: Arc<PathBuf>) -> (String, bool) {
    // The blocking task owns its input: it can outlive the async caller on exit.
    let input = input.clone();
    let result = tokio::task::spawn_blocking(move || run_file_tool(name, &input, &root)).await;
    match result {
        Ok(Ok(output)) => (output, false),
        Ok(Err(error)) => (format!("Error: {error:#}"), true),
        Err(error) => (format!("File task failed: {error}"), true),
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn schema(&self) -> Value {
        json!({
            "name": self.name(), "description": "Read a UTF-8 file in the workspace, optionally limiting the number of lines.",
            "input_schema": { "type": "object", "properties": {
                "path": {"type": "string"}, "limit": {"type": "integer", "minimum": 0}
            }, "required": ["path"] }
        })
    }
    async fn execute(&mut self, input: &Value) -> (String, bool) {
        execute(self.name(), input, Arc::clone(&self.0)).await
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn schema(&self) -> Value {
        json!({
            "name": self.name(), "description": "Create or overwrite a UTF-8 file in the workspace. Its parent directory must exist.",
            "input_schema": { "type": "object", "properties": {
                "path": {"type": "string"}, "content": {"type": "string"}
            }, "required": ["path", "content"] }
        })
    }
    async fn execute(&mut self, input: &Value) -> (String, bool) {
        execute(self.name(), input, Arc::clone(&self.0)).await
    }
}

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn schema(&self) -> Value {
        json!({
            "name": self.name(), "description": "Replace exactly one occurrence of old_text in a workspace file; fails if the match is absent or ambiguous.",
            "input_schema": { "type": "object", "properties": {
                "path": {"type": "string"}, "old_text": {"type": "string"}, "new_text": {"type": "string"}
            }, "required": ["path", "old_text", "new_text"] }
        })
    }
    async fn execute(&mut self, input: &Value) -> (String, bool) {
        execute(self.name(), input, Arc::clone(&self.0)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counted<R> {
        reader: R,
        bytes: usize,
    }

    impl<R: Read> Read for Counted<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.reader.read(buffer)?;
            self.bytes += read;
            Ok(read)
        }
    }

    #[test]
    fn prefix_acquisition_is_bounded_even_without_newlines() {
        let mut source = Counted {
            reader: std::io::repeat(b'x'),
            bytes: 0,
        };
        let output = read_prefix(&mut source, Some(1)).unwrap();
        assert_eq!(
            output,
            format!("{}\n[output truncated]", "x".repeat(MAX_READ_CHARS))
        );
        assert!(source.bytes <= MAX_READ_CHARS + 1 + 8192);
    }

    #[test]
    fn zero_and_short_line_limits_do_not_consume_the_whole_source() {
        let mut source = Counted {
            reader: std::io::repeat(b'\n'),
            bytes: 0,
        };
        assert_eq!(read_prefix(&mut source, Some(0)).unwrap(), "");
        assert_eq!(source.bytes, 0);
        assert_eq!(read_prefix(&mut source, Some(2)).unwrap(), "\n\n");
        assert!(source.bytes <= 8192);
    }

    #[test]
    fn prefix_preserves_utf8_and_distinguishes_exact_fit_from_truncation() {
        for text in ["", "é猫\nlast", "\n\n"] {
            assert_eq!(read_prefix(text.as_bytes(), None).unwrap(), text);
        }
        let exact = "猫".repeat(MAX_READ_CHARS);
        assert_eq!(read_prefix(exact.as_bytes(), None).unwrap(), exact);
        let longer = format!("{exact}é");
        assert_eq!(
            read_prefix(longer.as_bytes(), None).unwrap(),
            format!("{exact}\n[output truncated]")
        );
        assert_eq!(
            read_prefix(b"one\ntwo\nthree".as_slice(), Some(2)).unwrap(),
            "one\ntwo\n"
        );
        assert!(read_prefix(b"\xff".as_slice(), None).is_err());
        assert!(read_prefix(b"\xc3".as_slice(), None).is_err());
        // Only the returned prefix is decoded, not bytes beyond its line limit.
        assert_eq!(
            read_prefix(b"one\n\xff".as_slice(), Some(1)).unwrap(),
            "one\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn blocked_file_read_does_not_block_the_executor() {
        let workspace = std::env::current_dir().unwrap().canonicalize().unwrap();
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().canonicalize().unwrap();
        assert!(
            !scratch.starts_with(&workspace),
            "FIFO fixture workspace must be outside the repository"
        );
        // Exercise the real tool with an isolated cwd: changing this process's
        // cwd would race other tests, and an outside path must remain forbidden.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tools::files::tests::blocked_file_read_fixture",
                "--ignored",
                "--nocapture",
            ])
            .current_dir(&scratch)
            .env("ROBBIE_FILE_TEST_WORKSPACE", &scratch)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "FIFO fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        // Explicit close verifies cleanup; TempDir also cleans up on assertion unwind.
        root.close().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "subprocess fixture; run blocked_file_read_does_not_block_the_executor"]
    async fn blocked_file_read_fixture() {
        use std::time::{Duration, Instant};
        let expected = std::env::var_os("ROBBIE_FILE_TEST_WORKSPACE")
            .expect("requires a temporary workspace supplied by the parent test");
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(root, PathBuf::from(expected).canonicalize().unwrap());
        let fifo = root.join("pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        // An OS thread guarantees the old blocking implementation eventually
        // unblocks too, so a failure cannot hang the test runtime.
        let writer_path = fifo.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            std::fs::write(writer_path, "released").unwrap();
        });
        let input = json!({"path": fifo});
        let mut tool = ReadFile(Arc::new(root));
        let start = Instant::now();
        let heartbeat = async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            start.elapsed()
        };
        let ((output, is_error), latency) = tokio::join!(tool.execute(&input), heartbeat);
        writer.join().unwrap();
        assert_eq!(output, "released");
        assert!(!is_error);
        assert!(
            latency < Duration::from_millis(350),
            "executor stalled for {latency:?}"
        );
    }

    #[test]
    fn file_tools_create_read_and_edit() {
        let root = std::env::temp_dir().join(format!(
            "robbie-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _ = std::fs::remove_file(root.join("example.txt"));
        run_file_tool(
            "write_file",
            &json!({"path":"example.txt", "content":"one\ntwo\n"}),
            &root,
        )
        .unwrap();
        assert_eq!(
            run_file_tool(
                "read_file",
                &json!({"path":"example.txt", "limit":1}),
                &root
            )
            .unwrap(),
            "one\n"
        );
        run_file_tool(
            "edit_file",
            &json!({"path":"example.txt", "old_text":"two", "new_text":"three"}),
            &root,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("example.txt")).unwrap(),
            "one\nthree\n"
        );
        assert!(
            run_file_tool(
                "edit_file",
                &json!({"path":"example.txt", "old_text":"e", "new_text":"x"}),
                &root
            )
            .is_err()
        );
        assert!(
            run_file_tool(
                "write_file",
                &json!({"path":"../outside.txt", "content":"x"}),
                &root
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                root.parent().unwrap().join("missing-outside-file"),
                root.join("link"),
            )
            .unwrap();
            assert!(
                run_file_tool("write_file", &json!({"path":"link", "content":"x"}), &root).is_err()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
