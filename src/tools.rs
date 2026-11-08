use std::{collections::HashMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use crate::{diagnostics::tool::ToolContext, skills::SkillRegistry};

mod bash;
mod files;
mod load_skill;
mod sub_agent;
mod todo;

// Each tool supplies both its model-facing schema and its local behavior.
#[async_trait]
trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn schema(&self) -> Value;
    async fn execute(&mut self, input: &Value) -> (String, bool);

    // Most tools ignore tracing context; delegation forwards it to the child.
    async fn execute_in_context(
        &mut self,
        input: &Value,
        _parent: Option<ToolContext>,
    ) -> (String, bool) {
        self.execute(input).await
    }
}

pub struct ToolRegistry {
    tools: HashMap<&'static str, Box<dyn Tool>>,
    skills: Arc<SkillRegistry>,
    workspace: Arc<PathBuf>,
}

impl ToolRegistry {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_skills(
            Arc::new(SkillRegistry::default()),
            std::env::current_dir().unwrap(),
        )
    }

    pub fn with_skills(skills: Arc<SkillRegistry>, workspace: PathBuf) -> Self {
        let mut registry = Self::child(skills, Arc::new(workspace));
        registry.register(Box::new(todo::TodoTool::default()));
        registry
    }

    pub fn skill_descriptions(&self) -> String {
        self.skills.describe_available()
    }

    // Children can use workspace tools and skills, but cannot delegate recursively.
    fn child(skills: Arc<SkillRegistry>, workspace: Arc<PathBuf>) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
            skills: Arc::clone(&skills),
            workspace: Arc::clone(&workspace),
        };
        registry.register(Box::new(load_skill::LoadSkill { skills }));
        registry.register(Box::new(bash::Bash(Arc::clone(&workspace))));
        registry.register(Box::new(files::ReadFile(Arc::clone(&workspace))));
        registry.register(Box::new(files::WriteFile(Arc::clone(&workspace))));
        registry.register(Box::new(files::EditFile(workspace)));
        registry
    }

    // Share immutable connection/protocol configuration, never conversation state.
    pub fn enable_sub_agent(
        &mut self,
        model: Arc<crate::model::ModelClient>,
        events: crate::events::EventSender,
    ) {
        self.register(Box::new(sub_agent::SubAgent {
            model,
            events,
            skills: Arc::clone(&self.skills),
            workspace: Arc::clone(&self.workspace),
        }));
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

    #[cfg(test)]
    pub async fn execute(&mut self, name: &str, input: &Value) -> (String, bool) {
        self.execute_in_context(name, input, None).await
    }

    pub async fn execute_in_context(
        &mut self,
        name: &str,
        input: &Value,
        parent: Option<ToolContext>,
    ) -> (String, bool) {
        match self.tools.get_mut(name) {
            Some(tool) => tool.execute_in_context(input, parent).await,
            None => (format!("Unknown tool: {name}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_and_shell_tools_use_the_injected_workspace() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (root, text) in [(first.path(), "first"), (second.path(), "second")] {
            let mut registry =
                ToolRegistry::with_skills(Arc::new(SkillRegistry::default()), root.to_owned());
            assert!(
                !registry
                    .execute(
                        "write_file",
                        &json!({"path":"injected-workspace", "content":text})
                    )
                    .await
                    .1
            );
            assert_eq!(
                std::fs::read_to_string(root.join("injected-workspace")).unwrap(),
                text
            );
            assert_eq!(
                registry
                    .execute("read_file", &json!({"path":"injected-workspace"}))
                    .await,
                (text.into(), false)
            );
            assert_eq!(
                registry
                    .execute("bash", &json!({"command":"cat injected-workspace"}))
                    .await,
                (format!("{text}\n[exit 0]"), false)
            );
            assert!(
                !registry
                    .execute(
                        "edit_file",
                        &json!({"path":"injected-workspace", "old_text":text, "new_text":"edited"})
                    )
                    .await
                    .1
            );
            assert_eq!(
                std::fs::read_to_string(root.join("injected-workspace")).unwrap(),
                "edited"
            );
        }
    }

    #[tokio::test]
    async fn registry_exposes_and_dispatches_bash() {
        let mut registry = ToolRegistry::new();
        assert_eq!(registry.schemas()[0]["name"], "bash");
        let (output, error) = registry
            .execute("bash", &json!({"command": "printf hello"}))
            .await;
        assert!(output.contains("hello"));
        assert!(!error);
        let (_, error) = registry
            .execute("bash", &json!({"command": "exit 1"}))
            .await;
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
        let (output, is_error) = registry
            .execute("bash", &json!({"command":"printf hello"}))
            .await;
        assert!(!is_error);
        assert_eq!(output, "hello\n[exit 0]");
    }

    struct Counter(usize);

    #[async_trait]
    impl Tool for Counter {
        fn name(&self) -> &'static str {
            "counter"
        }
        fn schema(&self) -> Value {
            json!({"name": self.name()})
        }
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
