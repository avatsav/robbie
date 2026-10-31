use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolRegistry};
use crate::Agent;

pub(super) struct SubAgent {
    pub(super) client: reqwest::Client,
    pub(super) api_key: String,
    pub(super) model: String,
    pub(super) provider: Option<String>,
}

impl SubAgent {
    fn new_child(&self) -> Agent {
        Agent {
            client: self.client.clone(),
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            tools: ToolRegistry::child(),
            messages: Vec::new(),
        }
    }
}

#[async_trait]
impl Tool for SubAgent {
    fn name(&self) -> &'static str { "sub_agent" }

    fn schema(&self) -> Value {
        json!({
            "name": self.name(),
            "description": "Delegate a focused task to a child agent with a fresh conversation; returns its final response. The child shares the workspace.",
            "input_schema": {
                "type": "object",
                "properties": { "prompt": { "type": "string", "description": "The complete task and context to give the child." } },
                "required": ["prompt"]
            }
        })
    }

    async fn execute(&mut self, input: &Value) -> (String, bool) {
        let Some(prompt) = input.get("prompt").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
            return ("Error: prompt must be a non-empty string".to_string(), true);
        };
        let mut child = self.new_child();
        match child.run(prompt).await {
            Ok(response) => (response, false),
            Err(error) => (format!("Subagent failed: {error:#}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub_agent() -> SubAgent {
        SubAgent { client: reqwest::Client::new(), api_key: "test".into(), model: "test/model".into(), provider: None }
    }

    #[test]
    fn children_have_fresh_context_and_limited_tools() {
        let tool = sub_agent();
        let mut first = tool.new_child();
        first.messages.push(json!({"role": "user", "content": "private"}));
        let second = tool.new_child();
        assert!(second.messages.is_empty());
        let names: Vec<String> = second.tools.schemas().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["bash", "edit_file", "read_file", "write_file"]);
    }

    #[tokio::test]
    async fn parent_registry_exposes_and_dispatches_sub_agent() {
        let mut registry = ToolRegistry::new();
        registry.enable_sub_agent(reqwest::Client::new(), "test".into(), "test/model".into(), None);
        assert!(registry.schemas().iter().any(|schema| schema["name"] == "sub_agent"));
        let (output, is_error) = registry.execute("sub_agent", &json!({})).await;
        assert!(is_error);
        assert!(output.contains("prompt must be a non-empty string"));
    }

    #[tokio::test]
    async fn invalid_prompt_is_a_tool_error() {
        let mut tool = sub_agent();
        assert!(tool.execute(&json!({"prompt": "  "})).await.1);
        assert!(tool.execute(&json!({})).await.1);
    }
}
