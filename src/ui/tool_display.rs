use serde_json::Value;

// UI policy only: these names never change the model's schemas or tool routing.
// Customize a tool's label and primary argument together in this table.
const PRESENTATIONS: &[(&str, ToolPresentation<'static>)] = &[
    (
        "bash",
        ToolPresentation {
            label: "shell",
            summary_field: Some("command"),
        },
    ),
    (
        "read_file",
        ToolPresentation {
            label: "read",
            summary_field: Some("path"),
        },
    ),
    (
        "write_file",
        ToolPresentation {
            label: "write",
            summary_field: Some("path"),
        },
    ),
    (
        "edit_file",
        ToolPresentation {
            label: "edit",
            summary_field: Some("path"),
        },
    ),
    (
        "todo",
        ToolPresentation {
            label: "todo",
            summary_field: Some("action"),
        },
    ),
    (
        "sub_agent",
        ToolPresentation {
            label: "sub agent",
            summary_field: Some("prompt"),
        },
    ),
    (
        "load_skill",
        ToolPresentation {
            label: "skill",
            summary_field: Some("name"),
        },
    ),
];

#[derive(Clone, Copy)]
pub(super) struct ToolPresentation<'a> {
    pub(super) label: &'a str,
    summary_field: Option<&'a str>,
}

pub(super) fn presentation(name: &str) -> ToolPresentation<'_> {
    PRESENTATIONS
        .iter()
        .find(|(id, _)| *id == name)
        .map(|(_, display)| *display)
        .unwrap_or(ToolPresentation {
            label: name,
            summary_field: None,
        })
}

impl ToolPresentation<'_> {
    pub(super) fn summary(&self, input: &Value) -> String {
        match self.summary_field {
            Some(field) => input[field]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("(missing {field})")),
            // A new tool without a presentation must not dump arbitrary payloads
            // (file contents, prompts, etc.) into a compact header.
            None => "(details unavailable)".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_registered_tool_has_an_explicit_presentation() {
        for schema in crate::tools::ToolRegistry::new().schemas() {
            let name = schema["name"].as_str().unwrap();
            assert!(
                PRESENTATIONS.iter().any(|(id, _)| *id == name),
                "missing presentation: {name}"
            );
        }
        assert!(PRESENTATIONS.iter().any(|(id, _)| *id == "sub_agent"));
        for (id, display) in PRESENTATIONS {
            assert_eq!(presentation(id).label, display.label);
            assert!(display.summary_field.is_some());
        }
    }

    #[test]
    fn known_tools_show_their_primary_argument_not_their_payload() {
        let input = json!({
            "command": "cargo test", "path": "src/main.rs", "action": "list",
            "prompt": "Inspect the parser", "name": "rust-for-kotlin", "content": "PRIVATE BODY"
        });
        for (id, label, summary) in [
            ("bash", "shell", "cargo test"),
            ("read_file", "read", "src/main.rs"),
            ("write_file", "write", "src/main.rs"),
            ("edit_file", "edit", "src/main.rs"),
            ("todo", "todo", "list"),
            ("sub_agent", "sub agent", "Inspect the parser"),
            ("load_skill", "skill", "rust-for-kotlin"),
        ] {
            let display = presentation(id);
            assert_eq!(display.label, label);
            assert_eq!(display.summary(&input), summary);
        }
    }

    #[test]
    fn presentation_can_change_without_renaming_the_tool() {
        let custom = ToolPresentation {
            label: "instructions",
            summary_field: Some("name"),
        };
        assert_eq!(custom.label, "instructions");
        assert_eq!(custom.summary(&json!({"name": "example"})), "example");
        assert_eq!(presentation("load_skill").label, "skill");
    }

    #[test]
    fn missing_invalid_and_unknown_arguments_do_not_fall_back_to_json() {
        let display = presentation("load_skill");
        for input in [
            json!({}),
            json!({"name": 42}),
            json!({"name": {"secret": "body"}}),
        ] {
            assert_eq!(display.summary(&input), "(missing name)");
        }
        assert_eq!(
            presentation("future_tool").summary(&json!({"secret": "body"})),
            "(details unavailable)"
        );
    }
}
