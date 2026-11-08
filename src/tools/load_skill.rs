use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::Tool;
use crate::skills::SkillRegistry;

pub(super) struct LoadSkill {
    pub(super) skills: Arc<SkillRegistry>,
}

#[async_trait]
impl Tool for LoadSkill {
    fn name(&self) -> &'static str {
        "load_skill"
    }

    fn schema(&self) -> Value {
        json!({
            "name": self.name(),
            "description": "Load a skill's instructions by its exact advertised name before doing a matching task. Relative references in the skill use its base directory.",
            "input_schema": {
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Name from the available skills list." } },
                "required": ["name"]
            }
        })
    }

    async fn execute(&mut self, input: &Value) -> (String, bool) {
        let Some(name) = input
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
        else {
            return ("Error: name must be a non-empty string".to_string(), true);
        };
        match self.skills.load(name) {
            Ok(content) => (content, false),
            Err(error) => (format!("Error: {error:#}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_dispatches_skill_content_and_validation_errors() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("example")).unwrap();
        std::fs::write(
            root.path().join("example/SKILL.md"),
            "---\nname: example\ndescription: A test.\n---\nFollow these steps.",
        )
        .unwrap();
        let skills = Arc::new(SkillRegistry::scan(root.path()).unwrap());
        let mut registry = super::super::ToolRegistry::with_skills(skills, root.path().to_owned());
        assert!(
            registry
                .schemas()
                .iter()
                .any(|schema| schema["name"] == "load_skill")
        );
        let (content, error) = registry
            .execute("load_skill", &json!({"name": "example"}))
            .await;
        assert!(!error);
        assert!(content.contains("Follow these steps."));
        for input in [
            json!({}),
            json!({"name": " "}),
            json!({"name": 42}),
            json!({"name": "missing"}),
            json!({"name": "../example/SKILL.md"}),
        ] {
            let (message, error) = registry.execute("load_skill", &input).await;
            assert!(error, "{input}");
            assert!(message.starts_with("Error:"));
        }
    }
}
