use super::*;
use crate::test_support::{Reply, SUCCESS, Server};

fn fast_policy() -> Policy {
    Policy {
        backoff: Duration::from_millis(1),
        attempt_timeout: Duration::from_secs(2),
    }
}

// All callers here use a nonempty system, tools, and user/tool-result tail.
// This asserts the wire request, not a remote cache hit.
fn assert_prompt_cache_anchors(body: &Value, prompt_cache: PromptCache) {
    let system = &body["system"];
    let tool = body["tools"].as_array().unwrap().last().unwrap();
    let tail = &body["messages"].as_array().unwrap().last().unwrap()["content"];
    match prompt_cache {
        PromptCache::Short => {
            let marker = serde_json::json!({"type":"ephemeral"});
            assert_eq!(system[0]["cache_control"], marker);
            assert_eq!(tool["cache_control"], marker);
            assert_eq!(
                tail.as_array().unwrap().last().unwrap()["cache_control"],
                marker
            );
        }
        PromptCache::Off => {
            assert!(system.is_string());
            assert!(tool.get("cache_control").is_none());
            if let Some(blocks) = tail.as_array() {
                assert!(
                    blocks
                        .iter()
                        .all(|block| block.get("cache_control").is_none())
                );
            } else {
                assert!(tail.is_string());
            }
        }
    }
    assert!(body.get("cache_control").is_none());
}

#[cfg(unix)]
mod agent_diagnostics;
#[cfg(unix)]
mod diagnostics;
#[cfg(unix)]
mod tool_diagnostics;

#[tokio::test]
async fn success_uses_one_attempt_through_the_production_interface() {
    let server = Server::new(vec![Reply::Http(200, "", SUCCESS)]);
    let body = send(server.request()).await.unwrap();
    assert_eq!(body["content"][0]["text"], "done");
    assert_eq!(server.count(), 1);
}

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

#[tokio::test]
async fn retries_transient_statuses_even_with_non_json_error_bodies() {
    for status in [408, 429, 500, 502, 503, 504, 529] {
        let server = Server::new(vec![
            Reply::Http(status, "", "<html>busy</html>"),
            Reply::Http(200, "", SUCCESS),
        ]);
        let body = send_with_policy(server.request(), fast_policy())
            .await
            .unwrap();
        assert_eq!(body["content"][0]["text"], "done");
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "status {status}");
        assert_eq!(
            requests[0], requests[1],
            "must replay the exact buffered request"
        );
        assert!(requests[0].contains("already-executed"));
    }
}

#[tokio::test]
async fn permanent_statuses_and_invalid_success_json_do_not_retry() {
    for status in [400, 401, 403, 404, 413, 422, 501, 505] {
        let server = Server::new(vec![Reply::Http(status, "", "secret error payload")]);
        let error = send_with_policy(server.request(), fast_policy())
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(&status.to_string()));
        assert!(message.contains("1 attempt(s)"));
        assert!(!message.contains("secret"));
        assert_eq!(server.count(), 1);
    }
    let server = Server::new(vec![Reply::Http(200, "", "not JSON")]);
    let error = send_with_policy(server.request(), fast_policy())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid model response"));
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn exhausted_budget_is_four_attempts_and_resets_for_the_next_request() {
    let server = Server::new((0..8).map(|_| Reply::Http(503, "", "busy")).collect());
    for expected in [4, 8] {
        let error = send_with_policy(server.request(), fast_policy())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("503"));
        assert!(error.to_string().contains("4 attempt(s)"));
        assert_eq!(server.count(), expected);
    }
}

#[tokio::test]
async fn disconnects_and_incomplete_bodies_retry_without_exposing_partial_output() {
    for reply in [Reply::Disconnect, Reply::TruncatedBody] {
        let server = Server::new(vec![reply, Reply::Http(200, "", SUCCESS)]);
        assert_eq!(
            send_with_policy(server.request(), fast_policy())
                .await
                .unwrap()["content"][0]["text"],
            "done"
        );
        assert_eq!(server.count(), 2);
    }
}

#[tokio::test]
async fn timeouts_are_bounded_and_report_the_attempt_count() {
    let server = Server::new(vec![Reply::Stall]);
    let policy = Policy {
        attempt_timeout: Duration::from_millis(20),
        ..fast_policy()
    };
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        send_with_policy(server.request(), policy),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("4 attempt(s)"));
    assert!(
        error.downcast_ref::<reqwest::Error>().unwrap().is_timeout(),
        "{error:#}"
    );
}

#[test]
fn backoff_and_retry_after_are_bounded_without_shortening_server_delays() {
    let policy = Policy::default();
    for (retry, seconds) in [(0, 1), (1, 2), (2, 4)] {
        assert_eq!(
            policy.delay(retry, None).unwrap(),
            Duration::from_secs(seconds)
        );
    }
    for (header, seconds) in [
        ("0", 4),
        (" 12 ", 12),
        ("30", 30),
        ("garbage", 4),
        ("-1", 4),
    ] {
        assert_eq!(
            policy.delay(2, Some(header)).unwrap(),
            Duration::from_secs(seconds)
        );
    }
    assert!(policy.delay(0, Some("31")).is_err());
    assert!(policy.delay(0, Some("18446744073709551615")).is_err());
    assert!(policy.delay(0, Some("99999999999999999999999")).is_err());
}

#[test]
fn retry_after_understands_http_dates_and_ignores_invalid_values() {
    let now = httpdate::parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
    assert_eq!(
        retry_after_delay("Wed, 21 Oct 2015 07:28:12 GMT", now).unwrap(),
        Some(Duration::from_secs(12))
    );
    assert_eq!(
        retry_after_delay("Wed, 21 Oct 2015 07:27:00 GMT", now).unwrap(),
        Some(Duration::ZERO)
    );
    assert_eq!(retry_after_delay("invalid", now).unwrap(), None);
    assert_eq!(retry_after_delay("", now).unwrap(), None);
}

#[tokio::test]
async fn excessive_retry_after_stops_instead_of_retrying_early() {
    let server = Server::new(vec![Reply::Http(429, "Retry-After: 60\r\n", "busy")]);
    let error = send_with_policy(server.request(), fast_policy())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("30-second"));
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn cancelling_during_backoff_leaves_no_background_retry() {
    let server = Server::new(vec![Reply::Http(429, "Retry-After: 1\r\n", "busy")]);
    let result = tokio::time::timeout(
        Duration::from_millis(100),
        send_with_policy(server.request(), fast_policy()),
    )
    .await;
    assert!(result.is_err(), "Retry-After must delay the second attempt");
    tokio::time::sleep(Duration::from_millis(1050)).await;
    assert_eq!(server.count(), 1);
}
