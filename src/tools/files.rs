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
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(error.into()),
            }
            let parent = candidate.parent().context("path has no parent")?.canonicalize()?;
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
    input.get(name).and_then(Value::as_str).with_context(|| format!("missing or invalid {name}"))
}

fn run_file_tool(name: &str, input: &Value, root: &Path) -> Result<String> {
    let path = required(input, "path")?;
    if path.is_empty() { bail!("path must not be empty"); }
    match name {
        "read_file" => {
            let path = workspace_path(root, path, false)?;
            let content = std::fs::read_to_string(path)?;
            let limited = match input.get("limit") {
                None => content,
                Some(value) => {
                    let limit = value.as_u64().context("limit must be a non-negative integer")?;
                    content.split_inclusive('\n').take(usize::try_from(limit).unwrap_or(usize::MAX)).collect()
                }
            };
            let truncated = limited.chars().count() > 50_000;
            let mut output: String = limited.chars().take(50_000).collect();
            if truncated { output.push_str("\n[output truncated]"); }
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
            if old.is_empty() { bail!("old_text must not be empty"); }
            let path = workspace_path(root, path, false)?;
            let content = std::fs::read_to_string(&path)?;
            let count = content.match_indices(old).count();
            if count != 1 { bail!("old_text must match exactly once (found {count})"); }
            std::fs::write(path, content.replacen(old, new, 1))?;
            Ok("File edited".to_string())
        }
        _ => bail!("unknown file tool"),
    }
}

fn execute(name: &str, input: &Value) -> (String, bool) {
    let result = std::env::current_dir().context("cannot find workspace")
        .and_then(|root| run_file_tool(name, input, &root));
    match result {
        Ok(output) => (output, false),
        Err(error) => (format!("Error: {error:#}"), true),
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str { "read_file" }
    fn schema(&self) -> Value { json!({
        "name": self.name(), "description": "Read a UTF-8 file in the workspace, optionally limiting the number of lines.",
        "input_schema": { "type": "object", "properties": {
            "path": {"type": "string"}, "limit": {"type": "integer", "minimum": 0}
        }, "required": ["path"] }
    }) }
    async fn execute(&self, input: &Value) -> (String, bool) { execute(self.name(), input) }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str { "write_file" }
    fn schema(&self) -> Value { json!({
        "name": self.name(), "description": "Create or overwrite a UTF-8 file in the workspace. Its parent directory must exist.",
        "input_schema": { "type": "object", "properties": {
            "path": {"type": "string"}, "content": {"type": "string"}
        }, "required": ["path", "content"] }
    }) }
    async fn execute(&self, input: &Value) -> (String, bool) { execute(self.name(), input) }
}

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &'static str { "edit_file" }
    fn schema(&self) -> Value { json!({
        "name": self.name(), "description": "Replace exactly one occurrence of old_text in a workspace file; fails if the match is absent or ambiguous.",
        "input_schema": { "type": "object", "properties": {
            "path": {"type": "string"}, "old_text": {"type": "string"}, "new_text": {"type": "string"}
        }, "required": ["path", "old_text", "new_text"] }
    }) }
    async fn execute(&self, input: &Value) -> (String, bool) { execute(self.name(), input) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_tools_create_read_and_edit() {
        let root = std::env::temp_dir().join(format!("robbie-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&root).unwrap();
        let _ = std::fs::remove_file(root.join("example.txt"));
        run_file_tool("write_file", &json!({"path":"example.txt", "content":"one\ntwo\n"}), &root).unwrap();
        assert_eq!(run_file_tool("read_file", &json!({"path":"example.txt", "limit":1}), &root).unwrap(), "one\n");
        run_file_tool("edit_file", &json!({"path":"example.txt", "old_text":"two", "new_text":"three"}), &root).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("example.txt")).unwrap(), "one\nthree\n");
        assert!(run_file_tool("edit_file", &json!({"path":"example.txt", "old_text":"e", "new_text":"x"}), &root).is_err());
        assert!(run_file_tool("write_file", &json!({"path":"../outside.txt", "content":"x"}), &root).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.parent().unwrap().join("missing-outside-file"), root.join("link")).unwrap();
            assert!(run_file_tool("write_file", &json!({"path":"link", "content":"x"}), &root).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
