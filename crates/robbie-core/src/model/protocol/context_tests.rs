use super::*;
use crate::model::{ToolResult, summary_message};

fn messages() -> Vec<Message> {
    vec![
        Message::User("goal 🦀".into()),
        Message::Assistant(
            AnthropicMessages
                .decode(json!({"content":[
                    {"type":"thinking","thinking":"private reasoning","signature":"signed"},
                    {"type":"future_block","nested":{"preserve":true}},
                    {"type":"tool_use","id":"A","name":"read_file","input":{"path":"file.rs"}}
                ]}))
                .unwrap(),
        ),
        Message::ToolResults(vec![ToolResult {
            tool_call_id: "A".into(),
            content: "failed result 🦀".into(),
            is_error: true,
        }]),
    ]
}

#[test]
fn summary_context_matches_normal_encoding_without_modifying_the_tail() {
    let messages = messages();
    let before = AnthropicMessages
        .encode("test", "system", &messages, &[], PromptCache::Off)
        .body;
    let tools = [json!({"name":"fixture","input_schema":{"type":"object"}})];
    let mut expected_messages = vec![summary_message("earlier summary")];
    expected_messages.extend(messages.clone());
    for cache in [PromptCache::Short, PromptCache::Off] {
        let actual = AnthropicMessages.encode_with_summary(
            "test",
            "system",
            "earlier summary",
            &messages,
            &tools,
            cache,
        );
        let expected =
            AnthropicMessages.encode("test", "system", &expected_messages, &tools, cache);
        assert_eq!(actual.body, expected.body);
        assert_eq!(actual.headers, expected.headers);
        assert_eq!(actual.path, expected.path);
    }
    assert_eq!(
        AnthropicMessages
            .encode("test", "system", &messages, &[], PromptCache::Off)
            .body,
        before
    );
}

#[test]
fn summary_only_context_gets_the_normal_final_user_cache_anchor() {
    let encoded = AnthropicMessages.encode_with_summary(
        "test",
        "",
        "earlier summary",
        &[],
        &[],
        PromptCache::Short,
    );
    assert_eq!(encoded.body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(encoded.body["messages"][0]["role"], "user");
    assert_eq!(
        encoded.body["messages"][0]["content"][0]["cache_control"],
        json!({"type":"ephemeral"})
    );
}

#[test]
fn size_heuristic_counts_native_wire_content_including_opaque_blocks_and_unicode() {
    let messages = messages();
    for message in &messages {
        let encoded = AnthropicMessages.encode(
            "test",
            "",
            std::slice::from_ref(message),
            &[],
            PromptCache::Off,
        );
        let chars = encoded.body["messages"][0].to_string().chars().count();
        assert_eq!(
            AnthropicMessages.estimate_message_tokens(message),
            chars.div_ceil(4)
        );
    }
    let opaque = Message::Assistant(
        AnthropicMessages
            .decode(json!({"content":[
                {"type":"text","text":"short"},
                {"type":"thinking","thinking":"🦀".repeat(400),"signature":"signature"},
                {"type":"future_block","payload":"x".repeat(400)}
            ]}))
            .unwrap(),
    );
    assert!(AnthropicMessages.estimate_message_tokens(&opaque) > 200);
}
