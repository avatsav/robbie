use super::*;

#[tokio::test]
async fn success_uses_one_attempt_through_the_production_interface() {
    let server = Server::new(vec![Reply::Http(200, "", SUCCESS)]);
    let body = send(server.request()).await.unwrap();
    assert_eq!(body["content"][0]["text"], "done");
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn prompt_cache_rejection_does_not_retry_or_silently_change_policy() {
    let server = Server::new(vec![Reply::Http(400, "", "unsupported cache control")]);
    let model = server.model();
    let error = model
        .complete_with_policy(
            "main",
            "instructions",
            &[Message::User("task".into())],
            &[serde_json::json!({"name":"fixture", "input_schema":{"type":"object"}})],
            fast_policy(),
        )
        .await
        .err()
        .unwrap();
    assert!(format!("{error:#}").contains("400"));
    assert_eq!(server.count(), 1, "no hidden retry with caching disabled");
    let requests = server.requests.lock().unwrap();
    let body: Value = serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_prompt_cache_anchors(&body, PromptCache::Short);
}

#[tokio::test]
async fn malformed_protocol_reply_never_retries_or_executes_partial_tool_calls() {
    use crate::{Agent, ToolRegistry, events::EventKind};
    let server = Server::new(vec![Reply::Http(
        200,
        "",
        r#"{"content":[{"type":"tool_use","id":"valid","name":"bash","input":{"command":"printf should-not-run"}},{"type":"tool_use","name":"bash","input":{}}]}"#,
    )]);
    let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut agent = Agent::new(server.model(), ToolRegistry::new(), events, "main");
    assert!(
        agent
            .run("test malformed response")
            .await
            .unwrap_err()
            .to_string()
            .contains("no ID")
    );
    assert_eq!(server.count(), 1);
    assert_eq!(agent.history().len(), 1);
    while let Ok(event) = receiver.try_recv() {
        assert!(!matches!(event.kind, EventKind::ToolStarted { .. }));
    }
}

#[tokio::test]
async fn structurally_invalid_blocks_reject_the_whole_reply_before_tool_execution() {
    use crate::{Agent, ToolRegistry, events::EventKind};
    macro_rules! response {
            ($block:literal) => {
                concat!(r#"{"content":[{"type":"tool_use","id":"valid","name":"bash","input":{"command":"printf should-not-run"}},"#, $block, "]}")
            };
        }
    for body in [
        response!("null"),
        response!("42"),
        response!("[]"),
        response!("{}"),
        response!(r#"{"type":42}"#),
    ] {
        let server = Server::new(vec![Reply::Http(200, "", body)]);
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut agent = Agent::new(server.model(), ToolRegistry::new(), events, "main");
        assert!(
            agent.run("malformed blocks").await.is_err(),
            "accepted {body}"
        );
        assert_eq!(server.count(), 1);
        assert_eq!(agent.history().len(), 1);
        while let Ok(event) = receiver.try_recv() {
            assert!(!matches!(event.kind, EventKind::ToolStarted { .. }));
        }
    }
}
