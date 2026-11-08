use super::*;

#[tokio::test]
async fn agent_retry_after_a_tool_does_not_reexecute_it_or_duplicate_history() {
    use crate::{Agent, ToolRegistry, events::EventKind};
    for prompt_cache in [PromptCache::Short, PromptCache::Off] {
        let server = Server::new(vec![
            Reply::Http(
                200,
                "",
                r#"{"content":[{"type":"tool_use","id":"one-call","name":"bash","input":{"command":"printf one-execution"}}]}"#,
            ),
            Reply::Http(503, "", "temporarily unavailable"),
            Reply::Http(200, "", SUCCESS),
        ]);
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut agent = Agent::new(
            server.model_with_prompt_cache(prompt_cache),
            ToolRegistry::new(),
            events,
            "main",
        );
        let text = tokio::time::timeout(
            Duration::from_secs(10),
            agent.run("use the tool then reply"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(text, "done");
        assert_eq!(agent.history().len(), 4);
        let Message::ToolResults(results) = &agent.history()[2] else {
            panic!("expected tool results");
        };
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_call_id, "one-call");
        assert_eq!(results[0].content, "one-execution\n[exit 0]");
        let mut started = 0;
        let mut finished = 0;
        let mut replies = 0;
        while let Ok(event) = receiver.try_recv() {
            match event.kind {
                EventKind::ToolStarted { .. } => started += 1,
                EventKind::ToolFinished { is_error, .. } => {
                    assert!(!is_error);
                    finished += 1;
                }
                EventKind::AssistantText(..) => replies += 1,
                _ => {}
            }
        }
        assert_eq!((started, finished, replies), (1, 1, 1));
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1], requests[2]);
        assert!(requests[1].contains("test-provider"));
        for request in requests.iter() {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("x-openrouter-cache: true")
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("anthropic-version: 2023-06-01")
            );
        }
        let body: Value =
            serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "one-call");
        assert_prompt_cache_anchors(&body, prompt_cache);
        // Generated markers never enter the agent's durable user/tool history.
        assert!(
            matches!(&agent.history()[0], Message::User(text) if text == "use the tool then reply")
        );
    }
}

#[tokio::test]
async fn parent_and_real_child_share_provider_protocol_and_cache_but_not_history() {
    use crate::{Agent, ToolRegistry, events::EventKind};
    for prompt_cache in [PromptCache::Short, PromptCache::Off] {
        let server = Server::new(vec![
            Reply::Http(200, "", SUCCESS),
            Reply::Http(
                200,
                "",
                r#"{"content":[{"type":"tool_use","id":"child-call","name":"bash","input":{"command":"printf child-execution"}}]}"#,
            ),
            Reply::Http(200, "", SUCCESS),
        ]);
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut parent = Agent::new(
            server.model_with_prompt_cache(prompt_cache),
            ToolRegistry::new(),
            events,
            "main",
        );
        parent.enable_sub_agent();
        assert_eq!(parent.run("parent-only-secret").await.unwrap(), "done");
        let (text, is_error) = tokio::time::timeout(
            Duration::from_secs(10),
            parent.tools_for_test().execute(
                "sub_agent",
                &serde_json::json!({"prompt": "child-only-task"}),
            ),
        )
        .await
        .unwrap();
        assert!(!is_error, "{text}");
        assert_eq!(text, "done");
        assert_eq!(parent.history().len(), 2);
        let mut child_tool_calls = 0;
        while let Ok(event) = receiver.try_recv() {
            if matches!(event.kind, EventKind::ToolStarted { .. }) {
                assert_eq!(event.source, "subagent");
                child_tool_calls += 1;
            }
        }
        assert_eq!(child_tool_calls, 1);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for request in requests.iter() {
            let headers = request
                .split_once("\r\n\r\n")
                .unwrap()
                .0
                .to_ascii_lowercase();
            assert!(headers.contains("authorization: bearer test-secret"));
            assert!(headers.contains("x-openrouter-cache: true"));
            assert!(headers.contains("anthropic-version: 2023-06-01"));
            assert!(request.contains("test-provider"));
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_prompt_cache_anchors(&body, prompt_cache);
        }
        assert!(requests[0].contains("parent-only-secret"));
        assert!(!requests[1].contains("parent-only-secret"));
        assert!(requests[1].contains("child-only-task"));
        assert!(requests[2].contains("child-call"));
        assert!(requests[2].contains("child-execution"));
    }
}
