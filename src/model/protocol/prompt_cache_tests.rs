//! Wire serialization only: these tests cannot establish remote cache hits.
use super::*;
use crate::model::ToolResult;

fn tools() -> Vec<Value> {
    vec![
        json!({"name":"first", "description":"a", "input_schema":{"type":"object"}}),
        json!({"name":"last", "description":"b", "input_schema":{"type":"object"}}),
    ]
}

fn encode(system: &str, messages: &[Message], tools: &[Value], policy: PromptCache) -> Value {
    AnthropicMessages
        .encode("any/model", system, messages, tools, policy)
        .body
}

fn markers(body: &Value) -> usize {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(body["system"].as_array().into_iter().flatten())
        .chain(
            body["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|message| message["content"].as_array().into_iter().flatten()),
        )
        .filter(|block| block.get("cache_control").is_some())
        .count()
}

#[test]
fn short_cache_marks_three_anchors_without_mutating_inputs_or_accumulating_markers() {
    let tools = tools();
    let original_tools = tools.clone();
    let messages = vec![Message::User("first".into()), Message::User("last".into())];
    let body = encode("instructions", &messages, &tools, PromptCache::Short);
    assert_eq!(markers(&body), 3);
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(
        body["tools"][1]["cache_control"],
        json!({"type":"ephemeral"})
    );
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type":"ephemeral"})
    );
    assert_eq!(body["messages"][0]["content"], "first");
    assert_eq!(
        body["messages"][1]["content"][0]["cache_control"],
        json!({"type":"ephemeral"})
    );
    assert_eq!(tools, original_tools);
    assert_eq!(
        body,
        encode("instructions", &messages, &tools, PromptCache::Short)
    );
    assert!(
        body.get("cache_control").is_none(),
        "no root automatic breakpoint"
    );
    assert!(
        !body.to_string().contains("\"ttl\""),
        "Robbie leaves the TTL to the provider"
    );
}

#[test]
fn off_preserves_the_original_request_shapes() {
    let tools = tools();
    let messages = vec![Message::User("task".into())];
    let body = encode("instructions", &messages, &tools, PromptCache::Off);
    assert_eq!(
        body,
        json!({
            "model":"any/model", "max_tokens":8000, "system":"instructions", "tools":tools,
            "messages":[{"role":"user", "content":"task"}]
        })
    );
    assert_eq!(markers(&body), 0);
}

#[test]
fn empty_inputs_do_not_gain_invalid_cache_blocks() {
    for messages in [
        vec![],
        vec![Message::User(String::new())],
        vec![Message::ToolResults(vec![])],
    ] {
        let short = encode("", &messages, &[], PromptCache::Short);
        assert_eq!(markers(&short), 0);
        assert_eq!(short, encode("", &messages, &[], PromptCache::Off));
    }
}

#[test]
fn only_the_last_tool_result_is_marked_and_result_errors_survive() {
    let messages = vec![Message::ToolResults(vec![
        ToolResult {
            tool_call_id: "a".into(),
            content: "ok".into(),
            is_error: false,
        },
        ToolResult {
            tool_call_id: "b".into(),
            content: "denied".into(),
            is_error: true,
        },
    ])];
    let body = encode("", &messages, &[], PromptCache::Short);
    assert_eq!(markers(&body), 1);
    let results = &body["messages"][0]["content"];
    assert!(results[0].get("cache_control").is_none());
    assert_eq!(results[1]["cache_control"], json!({"type":"ephemeral"}));
    assert_eq!(results[1]["tool_use_id"], "b");
    assert_eq!(results[1]["is_error"], true);
    assert_eq!(results[1]["content"], "denied");
    assert_eq!(markers(&encode("", &messages, &[], PromptCache::Off)), 0);
}

#[test]
fn assistant_tail_and_opaque_continuation_are_never_rewritten_or_marked() {
    let blocks = json!([
        {"type":"thinking", "thinking":"private", "signature":"signed"},
        {"type":"text", "text":"preface"},
        {"type":"tool_use", "id":"native-id", "name":"bash", "input":{"command":"true", "cache_control":"an ordinary argument"}},
        {"type":"future", "extra":[1,2,3]},
        {"type":"redacted_thinking", "data":"opaque"},
    ]);
    let reply = AnthropicMessages.decode(json!({"content":blocks})).unwrap();
    let messages = vec![Message::User("earlier".into()), Message::Assistant(reply)];
    let short = encode("system", &messages, &tools(), PromptCache::Short);
    assert_eq!(
        short["messages"][0]["content"], "earlier",
        "do not search back from an assistant tail"
    );
    assert_eq!(short["messages"][1]["content"], blocks);
    assert_eq!(
        markers(&short),
        2,
        "only tools and system; assistant content stays unmarked"
    );
    let off = encode("system", &messages, &tools(), PromptCache::Off);
    assert_eq!(
        off["messages"][1]["content"], blocks,
        "off does not strip provider continuation"
    );
    assert_eq!(markers(&off), 0);
    assert_eq!(
        short,
        encode("system", &messages, &tools(), PromptCache::Short)
    );
}

#[test]
fn moving_tail_does_not_leave_old_generated_markers_in_history() {
    let mut messages = vec![Message::User("first".into())];
    let first = encode("system", &messages, &tools(), PromptCache::Short);
    messages.push(Message::Assistant(
        AnthropicMessages
            .decode(json!({"content":[{"type":"text", "text":"answer"}]}))
            .unwrap(),
    ));
    messages.push(Message::User("follow-up".into()));
    let next = encode("system", &messages, &tools(), PromptCache::Short);
    assert_eq!(markers(&first), 3);
    assert_eq!(markers(&next), 3);
    assert_eq!(next["messages"][0]["content"], "first");
    assert_eq!(
        next["messages"][1]["content"],
        json!([{"type":"text", "text":"answer"}])
    );
    assert_eq!(
        next["messages"][2]["content"][0]["cache_control"],
        json!({"type":"ephemeral"})
    );
}

#[test]
fn inherited_markers_are_preserved_without_exceeding_four_slots() {
    for (inherited, expected) in [3, 4, 4, 4, 4, 5].into_iter().enumerate() {
        let blocks: Vec<Value> = (0..inherited)
            .map(|i| json!({"type":"text", "text":format!("block {i}"), "cache_control":{"type":"ephemeral"}}))
            .collect();
        let reply = AnthropicMessages.decode(json!({"content":blocks})).unwrap();
        let messages = vec![Message::Assistant(reply), Message::User("next".into())];
        let body = encode("system", &messages, &tools(), PromptCache::Short);
        assert_eq!(body["messages"][0]["content"], json!(blocks));
        assert_eq!(markers(&body), expected);
        if inherited >= 4 {
            assert_eq!(
                body,
                encode("system", &messages, &tools(), PromptCache::Off)
            );
        }
    }
}

#[test]
fn existing_tool_markers_and_nested_schema_keys_do_not_consume_extra_slots() {
    let tools = vec![json!({
        "name":"last", "cache_control":{"type":"ephemeral"},
        "input_schema":{"type":"object", "properties":{
            "cache_control":{"type":"string"},
            "nested_a":{"type":"object", "properties":{"cache_control":{"type":"string"}}},
            "nested_b":{"type":"object", "properties":{"cache_control":{"type":"string"}}},
            "nested_c":{"type":"object", "properties":{"cache_control":{"type":"string"}}}
        }}
    })];
    let body = encode(
        "system",
        &[Message::User("task".into())],
        &tools,
        PromptCache::Short,
    );
    assert_eq!(markers(&body), 3);
    assert_eq!(body["tools"][0], tools[0]);
    assert_eq!(tools[0]["cache_control"], json!({"type":"ephemeral"}));
}
