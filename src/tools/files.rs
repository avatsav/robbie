use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::Tool;

pub(super) struct ReadFile;
pub(super) struct WriteFile;
pub(super) struct EditFile;

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

fn run_file_tool(name: &str, input: &Value, root: &Path) -> Result<String> {
    let path = required(input, "path")?;
    if path.is_empty() {
        bail!("path must not be empty");
    }
    match name {
        "read_file" => {
            let path = workspace_path(root, path, false)?;
            let content = std::fs::read_to_string(path)?;
            let limited = match input.get("limit") {
                None => content,
                Some(value) => {
                    let limit = value
                        .as_u64()
                        .context("limit must be a non-negative integer")?;
                    content
                        .split_inclusive('\n')
                        .take(usize::try_from(limit).unwrap_or(usize::MAX))
                        .collect()
                }
            };
            let truncated = limited.chars().count() > 50_000;
            let mut output: String = limited.chars().take(50_000).collect();
            if truncated {
                output.push_str("\n[output truncated]");
            }
            Ok(output)
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

async fn execute(name: &'static str, input: &Value) -> (String, bool) {
    // The blocking task owns its input: it can outlive the async caller on exit.
    let input = input.clone();
    let result = tokio::task::spawn_blocking(move || {
        let root = std::env::current_dir().context("cannot find workspace")?;
        run_file_tool(name, &input, &root)
    })
    .await;
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
        execute(self.name(), input).await
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
        execute(self.name(), input).await
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
        execute(self.name(), input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut tool = ReadFile;
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
