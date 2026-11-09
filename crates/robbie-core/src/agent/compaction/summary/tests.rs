use super::*;
use crate::model::{AnthropicMessages, PromptCache, Protocol, ToolResult};
use serde_json::{Value, json};

fn assistant(blocks: Value) -> Message {
    Message::Assistant(
        AnthropicMessages
            .decode(json!({"content": blocks}))
            .unwrap(),
    )
}

fn tool_history(content: &str, is_error: bool) -> Vec<Message> {
    vec![
        Message::User("Fix the failing tests.".into()),
        assistant(json!([
            {"type": "text", "text": "Inspecting the file."},
            {"type": "tool_use", "id": "A", "name": "read_file", "input": {"path": "file.rs"}}
        ])),
        Message::ToolResults(vec![ToolResult {
            tool_call_id: "A".into(),
            content: content.into(),
            is_error,
        }]),
    ]
}

fn wire(messages: &[Message]) -> Value {
    AnthropicMessages
        .encode("test", "system", messages, &[], PromptCache::Off)
        .body
}

#[test]
fn includes_previous_checkpoint_roles_arguments_and_error_outcomes() {
    let messages = tool_history("file not found", true);
    let input = prepare_summary(
        &messages,
        Some("Keep the public interface unchanged."),
        20_000,
    )
    .unwrap();

    for expected in [
        "Do not continue the conversation",
        "## Goal",
        "## Constraints & Preferences",
        "## Progress",
        "## Key Decisions",
        "## Files",
        "## Next Steps",
        "## Critical Context",
        "[Previous checkpoint]\nKeep the public interface unchanged.",
        "[User]\nFix the failing tests.",
        "[Assistant]\nInspecting the file.",
        "[Assistant tool call]\nid: \"A\"\nname: \"read_file\"\narguments: {\"path\":\"file.rs\"}",
        "[Tool result]\nid: \"A\"\nstatus: error\ncontent:\nfile not found",
    ] {
        assert!(
            input.contains(expected),
            "missing expected section: {expected}"
        );
    }
    assert!(input.find("[Previous checkpoint]").unwrap() < input.find("[User]").unwrap());
}

#[test]
fn only_serializes_the_retired_prefix_and_never_changes_opaque_history_or_tail() {
    let mut messages = tool_history("OLD_RESULT", false);
    messages[1] = assistant(json!([
        {"type": "thinking", "thinking": "HIDDEN_THINKING", "signature": "HIDDEN_SIGNATURE"},
        {"type": "future_block", "payload": "HIDDEN_UNKNOWN"},
        {"type": "text", "text": "Visible progress."},
        {"type": "tool_use", "id": "A", "name": "read_file", "input": {"path": "file.rs"}}
    ]));
    messages.extend(tool_history("RETAINED_RESULT", false));
    messages[3] = Message::User("RETAINED_GOAL".into());
    let before = wire(&messages);
    let cut = super::super::find_cut(&messages, 3, |_| 1)
        .unwrap()
        .unwrap();
    assert_eq!(cut, 3);

    let input = prepare_summary(&messages[..cut], None, 20_000).unwrap();

    assert!(input.contains("OLD_RESULT"));
    assert!(input.contains("Visible progress."));
    for excluded in [
        "HIDDEN_THINKING",
        "HIDDEN_SIGNATURE",
        "HIDDEN_UNKNOWN",
        "RETAINED_GOAL",
        "RETAINED_RESULT",
        "[Previous checkpoint]",
    ] {
        assert!(!input.contains(excluded));
    }
    assert_eq!(wire(&messages), before);
}

#[test]
fn tool_excerpt_uses_unicode_boundaries_and_reports_exact_omitted_count() {
    let content = "🦀".repeat(TOOL_RESULT_MAX_CHARS + 17);
    let messages = tool_history(&content, false);
    let before = wire(&messages);

    let input = prepare_summary(&messages, None, 20_000).unwrap();

    assert_eq!(input.matches('🦀').count(), TOOL_RESULT_MAX_CHARS);
    assert!(input.contains("[... 17 characters omitted from tool result]"));
    assert!(input.contains("status: success"));
    assert_eq!(wire(&messages), before);
}

#[test]
fn a_tool_result_exactly_at_the_excerpt_limit_is_not_truncated() {
    let content = "é".repeat(TOOL_RESULT_MAX_CHARS);
    let input = prepare_summary(&tool_history(&content, false), None, 20_000).unwrap();

    assert!(input.contains(&content));
    assert!(!input.contains("characters omitted from tool result]"));
}

#[test]
fn overall_budget_counts_instructions_labels_checkpoint_and_unicode_text() {
    let messages = tool_history("résultat 🦀", true);
    let checkpoint = Some("Earlier constraints 🦀");
    let full = prepare_summary(&messages, checkpoint, 20_000).unwrap();
    let chars = full.chars().count();
    assert!(full.len() > chars);

    assert_eq!(prepare_summary(&messages, checkpoint, chars).unwrap(), full);
    for limit in [0, INSTRUCTIONS.chars().count(), chars - 1] {
        let error = prepare_summary(&messages, checkpoint, limit).unwrap_err();
        assert_eq!(error.to_string(), "summary input exceeds character budget");
    }
}

#[test]
fn non_tool_content_is_preserved_in_full_instead_of_silently_excerpted() {
    let checkpoint = "P".repeat(TOOL_RESULT_MAX_CHARS + 1);
    let user = "U".repeat(TOOL_RESULT_MAX_CHARS + 1);
    let text = "T".repeat(TOOL_RESULT_MAX_CHARS + 1);
    let path = "A".repeat(TOOL_RESULT_MAX_CHARS + 1);
    let mut messages = tool_history("short result", false);
    messages[0] = Message::User(user.clone());
    messages[1] = assistant(json!([
        {"type": "text", "text": text},
        {"type": "tool_use", "id": "A", "name": "read_file", "input": {"path": path}}
    ]));

    let input = prepare_summary(&messages, Some(&checkpoint), 20_000).unwrap();

    for preserved in [&checkpoint, &user, &text, &path] {
        assert!(input.contains(preserved));
    }
    assert!(!input.contains("characters omitted from tool result]"));
    assert!(prepare_summary(&messages, Some(&checkpoint), input.chars().count() - 1).is_err());
}

#[test]
fn an_incomplete_retired_prefix_is_rejected_not_repaired() {
    let mut messages = tool_history("result", false);
    messages.pop();
    let before = wire(&messages);

    let error = prepare_summary(&messages, None, 20_000).unwrap_err();

    assert_eq!(
        error.to_string(),
        "cannot compact an incomplete or mismatched tool exchange"
    );
    assert_eq!(wire(&messages), before);
}
