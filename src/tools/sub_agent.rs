use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolRegistry};
use crate::skills::SkillRegistry;
use crate::{Agent, diagnostics::tool::ToolContext};

pub(super) struct SubAgent {
    pub(super) model: Arc<crate::model::ModelClient>,
    pub(super) events: crate::events::EventSender,
    pub(super) skills: Arc<SkillRegistry>,
    pub(super) workspace: Arc<PathBuf>,
}

impl SubAgent {
    fn new_child(&self) -> Agent {
        Agent::new(
            Arc::clone(&self.model),
            ToolRegistry::child(Arc::clone(&self.skills), Arc::clone(&self.workspace)),
            self.events.clone(),
            "subagent",
        )
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
            workspace: Arc::new(std::env::current_dir().unwrap()),
        }
    }

    #[tokio::test]
    async fn children_inherit_the_explicit_workspace() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("child-workspace"), "isolated").unwrap();
        let mut tool = sub_agent();
        tool.workspace = Arc::new(root.path().to_owned());
        let mut child = tool.new_child();
        assert_eq!(
            child
                .tools_for_test()
                .execute("read_file", &json!({"path":"child-workspace"}))
                .await,
            ("isolated".into(), false)
        );
        assert_eq!(
            child
                .tools_for_test()
                .execute("bash", &json!({"command":"cat child-workspace"}))
                .await,
            ("isolated\n[exit 0]".into(), false)
        );
    }

    #[test]
    fn children_have_fresh_context_and_limited_tools() {
        let tool = sub_agent();
        let first = tool
            .new_child()
            .with_history(vec![crate::model::Message::User("private".into())]);
        let second = tool.new_child();
        assert!(second.history().is_empty());
        assert!(Arc::ptr_eq(&tool.model, first.model_for_test()));
        assert!(Arc::ptr_eq(first.model_for_test(), second.model_for_test()));
        let names: Vec<String> = second
            .transcript_context()
            .tools
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
        assert!(
            first
                .system_prompt_for_test()
                .contains("ownership: Explain Rust ownership.")
        );
        assert!(
            !first
                .system_prompt_for_test()
                .contains("Secret ownership instructions.")
        );
        let (content, error) = first
            .tools_for_test()
            .execute("load_skill", &json!({"name": "ownership"}))
            .await;
        assert!(!error);
        assert!(content.contains("Secret ownership instructions."));
        first = first.with_history(vec![crate::model::Message::User(content)]);
        let second = tool.new_child();
        assert!(second.history().is_empty());
        assert_eq!(
            first.system_prompt_for_test(),
            second.system_prompt_for_test()
        );

        let mut parent =
            ToolRegistry::with_skills(Arc::clone(&tool.skills), tool.workspace.as_ref().clone());
        parent.enable_sub_agent(Arc::clone(&tool.model), tool.events.clone());
        assert_eq!(
            parent.skill_descriptions(),
            tool.skills.describe_available()
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
        child.emit_for_test(crate::events::EventKind::AssistantText(
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
        child.emit_for_test(crate::events::EventKind::ModelStarted);
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
