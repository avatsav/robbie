use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

mod files;
mod sub_agent;
mod todo;

// Each tool supplies both its model-facing schema and its local behavior.
#[async_trait]
trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn schema(&self) -> Value;
    async fn execute(&mut self, input: &Value) -> (String, bool);
}

pub struct ToolRegistry {
    tools: HashMap<&'static str, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut registry = Self::child();
        registry.register(Box::new(todo::TodoTool::default()));
        registry
    }

    // Children can use workspace tools, but cannot delegate recursively.
    fn child() -> Self {
        let mut registry = Self { tools: HashMap::new() };
        registry.register(Box::new(Bash));
        registry.register(Box::new(files::ReadFile));
        registry.register(Box::new(files::WriteFile));
        registry.register(Box::new(files::EditFile));
        registry
    }

    pub fn enable_sub_agent(&mut self, client: reqwest::Client, api_key: String, model: String, provider: Option<String>, events: crate::events::EventSender) {
        self.register(Box::new(sub_agent::SubAgent { client, api_key, model, provider, events }));
    }

    fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn schemas(&self) -> Vec<Value> {
        // A stable order makes requests and tests easier to understand.
        let mut tools: Vec<_> = self.tools.values().collect();
        tools.sort_by_key(|tool| tool.name());
        tools.iter().map(|tool| tool.schema()).collect()
    }

    pub async fn execute(&mut self, name: &str, input: &Value) -> (String, bool) {
        match self.tools.get_mut(name) {
            Some(tool) => tool.execute(input).await,
            None => (format!("Unknown tool: {name}"), true),
        }
    }
}

struct Bash;

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
        let mut process = Command::new("sh");
        process.arg("-c").arg(command).kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(120), process.output()).await {
            Ok(Ok(output)) => {
                let combined = [output.stdout, output.stderr].concat();
                let text = String::from_utf8_lossy(&combined);
                let text = text.trim();
                // Keep the end of long command output: failures and summaries
                // usually live there, and the UI previews this same tail.
                let mut tail: Vec<_> = text.chars().rev().take(50_001).collect();
                let truncated = tail.len() > 50_000;
                tail.truncate(50_000);
                let result: String = tail.into_iter().rev().collect();
                let result = if result.is_empty() { "(no output)".to_string() } else { result };
                let result = if truncated { format!("[earlier output truncated]\n{result}") } else { result };
                let status = match output.status.code() {
                    Some(code) => format!("[exit {code}]"),
                    None => format!("[{}]", output.status),
                };
                (format!("{result}\n{status}"), !output.status.success())
            }
            Ok(Err(error)) => (format!("Could not run command: {error}"), true),
            Err(_) => ("Command timed out after 120 seconds".to_string(), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_exposes_and_dispatches_bash() {
        let mut registry = ToolRegistry::new();
        assert_eq!(registry.schemas()[0]["name"], "bash");
        let (output, error) = registry.execute("bash", &json!({"command": "printf hello"})).await;
        assert!(output.contains("hello"));
        assert!(!error);
        let (_, error) = registry.execute("bash", &json!({"command": "exit 1"})).await;
        assert!(error);
    }

    #[tokio::test]
    async fn bash_returns_the_output_tail_and_numeric_exit_code() {
        let mut registry = ToolRegistry::new();
        let (output, is_error) = registry.execute("bash", &json!({
            "command": "printf START; awk 'BEGIN { for (i = 0; i < 51000; i++) printf \"x\" }'; printf '\\nTAIL\\n'; exit 7"
        })).await;
        assert!(is_error);
        assert!(output.starts_with("[earlier output truncated]"));
        assert!(!output.contains("START"));
        assert!(output.ends_with("TAIL\n[exit 7]"));
        let (output, is_error) = registry.execute("bash", &json!({"command":"printf hello"})).await;
        assert!(!is_error);
        assert_eq!(output, "hello\n[exit 0]");
    }

    struct Counter(usize);

    #[async_trait]
    impl Tool for Counter {
        fn name(&self) -> &'static str { "counter" }
        fn schema(&self) -> Value { json!({"name": self.name()}) }
        async fn execute(&mut self, _input: &Value) -> (String, bool) {
            self.0 += 1;
            (self.0.to_string(), false)
        }
    }

    #[tokio::test]
    async fn registry_keeps_tool_state_between_calls() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(Counter(0)));
        assert_eq!(registry.execute("counter", &json!({})).await.0, "1");
        assert_eq!(registry.execute("counter", &json!({})).await.0, "2");
    }

    #[tokio::test]
    async fn registry_reports_unknown_tools_and_invalid_inputs() {
        let mut registry = ToolRegistry::new();
        let (output, error) = registry.execute("missing", &json!({})).await;
        assert!(error);
        assert!(output.contains("Unknown tool"));
        let (_, error) = registry.execute("bash", &json!({})).await;
        assert!(error);
    }
}
