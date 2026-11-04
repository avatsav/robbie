use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolRegistry};
use crate::skills::SkillRegistry;
use crate::{Agent, diagnostics::tool::ToolContext};

pub(super) struct SubAgent {
    pub(super) model: Arc<crate::model::ModelClient>,
    pub(super) events: crate::events::EventSender,
    pub(super) skills: Arc<SkillRegistry>,
}

impl SubAgent {
    fn new_child(&self) -> Agent {
        Agent {
            model: Arc::clone(&self.model),
            tools: ToolRegistry::child(Arc::clone(&self.skills)),
            events: self.events.clone(),
            source: "subagent",
            messages: Vec::new(),
        }
    }
}

#[async_trait]
impl Tool for SubAgent {
    fn name(&self) -> &'static str {
        "sub_agent"
    }

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
        self.execute_in_context(input, None).await
    }

    async fn execute_in_context(
        &mut self,
        input: &Value,
        parent: Option<ToolContext>,
    ) -> (String, bool) {
        let Some(prompt) = input
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ("Error: prompt must be a non-empty string".to_string(), true);
        };
        let mut child = self.new_child();
        match child.run_with_parent(prompt, parent).await {
            Ok(response) => (response, false),
            Err(error) => (format!("Subagent failed: {error:#}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub_agent() -> SubAgent {
        let (events, _) = tokio::sync::mpsc::unbounded_channel();
        SubAgent {
            model: Arc::new(crate::model::ModelClient::new(
                crate::model::client().unwrap(),
                crate::model::OpenRouter::new("test".into(), None),
                crate::model::AnthropicMessages,
                "test/model".into(),
                crate::diagnostics::Recorder::disabled(),
            )),
            events,
            skills: Arc::new(SkillRegistry::default()),
        }
    }

    #[test]
    fn children_have_fresh_context_and_limited_tools() {
        let tool = sub_agent();
        let mut first = tool.new_child();
        first
            .messages
            .push(crate::model::Message::User("private".into()));
        let second = tool.new_child();
        assert!(second.messages.is_empty());
        assert!(Arc::ptr_eq(&tool.model, &first.model));
        assert!(Arc::ptr_eq(&first.model, &second.model));
        let names: Vec<String> = second
            .tools
            .schemas()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["bash", "edit_file", "load_skill", "read_file", "write_file"]
        );
    }

    #[tokio::test]
    async fn children_share_skill_catalog_but_not_loaded_conversation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("SKILL.md"), "---\nname: ownership\ndescription: Explain Rust ownership.\n---\nSecret ownership instructions.").unwrap();
        let mut tool = sub_agent();
        tool.skills = Arc::new(SkillRegistry::scan(root.path()).unwrap());
        let mut first = tool.new_child();
        assert!(Arc::ptr_eq(&tool.skills, &first.tools.skills));
        assert!(
            first
                .system_prompt()
                .contains("ownership: Explain Rust ownership.")
        );
        assert!(
            !first
                .system_prompt()
                .contains("Secret ownership instructions.")
        );
        let (content, error) = first
            .tools
            .execute("load_skill", &json!({"name": "ownership"}))
            .await;
        assert!(!error);
        assert!(content.contains("Secret ownership instructions."));
        first.messages.push(crate::model::Message::User(content));
        let second = tool.new_child();
        assert!(second.messages.is_empty());
        assert_eq!(first.system_prompt(), second.system_prompt());

        let mut parent = ToolRegistry::with_skills(Arc::clone(&tool.skills));
        parent.enable_sub_agent(Arc::clone(&tool.model), tool.events.clone());
        assert_eq!(
            parent.skill_descriptions(),
            second.tools.skill_descriptions()
        );
        assert!(
            parent
                .schemas()
                .iter()
                .any(|schema| schema["name"] == "sub_agent")
        );
    }

    #[test]
    fn child_reports_activity_to_the_shared_channel() {
        let mut tool = sub_agent();
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        tool.events = events;
        let child = tool.new_child();
        child.emit(crate::events::EventKind::AssistantText(
            "Hello".into(),
            None,
        ));
        let event = receiver.try_recv().unwrap();
        assert_eq!(event.source, "subagent");
        assert!(
            matches!(event.kind, crate::events::EventKind::AssistantText(text, _) if text == "Hello")
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn closed_frontend_does_not_panic() {
        let child = sub_agent().new_child();
        child.emit(crate::events::EventKind::ModelStarted);
    }

    #[tokio::test]
    async fn parent_registry_exposes_and_dispatches_sub_agent() {
        let mut registry = ToolRegistry::new();
        let (events, _) = tokio::sync::mpsc::unbounded_channel();
        registry.enable_sub_agent(Arc::clone(&sub_agent().model), events);
        assert!(
            registry
                .schemas()
                .iter()
                .any(|schema| schema["name"] == "sub_agent")
        );
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
