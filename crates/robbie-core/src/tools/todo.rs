use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::Tool;

#[derive(Default)]
pub(super) struct TodoTool {
    items: Vec<TodoItem>,
    last_id: u64,
}

struct TodoItem {
    id: u64,
    content: String,
    status: Status,
}

enum Status {
    Pending,
    InProgress,
    Completed,
}

impl Status {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            _ => bail!("status must be pending, in_progress, or completed"),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

impl TodoTool {
    fn apply(&mut self, input: &Value) -> Result<String> {
        let action = input["action"].as_str().context("action is required")?;
        match action {
            "add" => {
                let content = input["content"]
                    .as_str()
                    .context("content is required")?
                    .trim();
                if content.is_empty() {
                    bail!("content must not be empty");
                }
                self.last_id = self.last_id.checked_add(1).context("todo IDs exhausted")?;
                self.items.push(TodoItem {
                    id: self.last_id,
                    content: content.to_string(),
                    status: Status::Pending,
                });
            }
            "update" => {
                let id = input["id"]
                    .as_u64()
                    .context("id must be a non-negative integer")?;
                let status =
                    Status::parse(input["status"].as_str().context("status is required")?)?;
                let item = self
                    .items
                    .iter_mut()
                    .find(|item| item.id == id)
                    .context("todo not found")?;
                item.status = status;
            }
            "list" => {}
            "clear" => self.items.clear(),
            _ => bail!("unknown action: {action}"),
        }
        let items: Vec<Value> = self
            .items
            .iter()
            .map(|item| {
                json!({
                    "id": item.id, "content": item.content, "status": item.status.as_str()
                })
            })
            .collect();
        Ok(json!({"todos": items}).to_string())
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &'static str {
        "todo"
    }

    fn schema(&self) -> Value {
        json!({
            "name": self.name(),
            "description": "Maintain an in-memory todo list. Add, update, list, or clear items. Returns the current list.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["add", "update", "list", "clear"]},
                    "content": {"type": "string", "description": "Required for add."},
                    "id": {"type": "integer", "minimum": 1, "description": "Required for update."},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "Required for update."}
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&mut self, input: &Value) -> (String, bool) {
        match self.apply(input) {
            Ok(output) => (output, false),
            Err(error) => (format!("Error: {error:#}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRegistry;

    #[tokio::test]
    async fn todo_actions_preserve_state_in_registry() {
        let mut registry = ToolRegistry::new();
        assert!(
            registry
                .schemas()
                .iter()
                .any(|schema| schema["name"] == "todo")
        );
        let (output, error) = registry
            .execute(
                "todo",
                &json!({"action": "add", "content": "Inspect project"}),
            )
            .await;
        assert!(!error);
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["todos"][0]["status"],
            "pending"
        );
        let (output, error) = registry
            .execute(
                "todo",
                &json!({"action": "update", "id": 1, "status": "completed"}),
            )
            .await;
        assert!(!error);
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["todos"][0]["status"],
            "completed"
        );
        let (output, _) = registry.execute("todo", &json!({"action": "list"})).await;
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["todos"][0]["content"],
            "Inspect project"
        );
        let (output, _) = registry.execute("todo", &json!({"action": "clear"})).await;
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["todos"],
            json!([])
        );
        let (output, _) = registry
            .execute("todo", &json!({"action": "add", "content": "Next"}))
            .await;
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["todos"][0]["id"],
            2
        );
    }

    #[test]
    fn invalid_actions_do_not_change_state() {
        let mut tool = TodoTool::default();
        tool.apply(&json!({"action":"add", "content":"First"}))
            .unwrap();
        assert!(
            tool.apply(&json!({"action":"add", "content":"  "}))
                .is_err()
        );
        assert!(
            tool.apply(&json!({"action":"update", "id":1, "status":"invalid"}))
                .is_err()
        );
        assert!(
            tool.apply(&json!({"action":"update", "id":99, "status":"completed"}))
                .is_err()
        );
        assert!(tool.apply(&json!({"action":"unknown"})).is_err());
        let output = tool.apply(&json!({"action":"list"})).unwrap();
        let items: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(items["todos"].as_array().unwrap().len(), 1);
        assert_eq!(items["todos"][0]["status"], "pending");
    }
}
