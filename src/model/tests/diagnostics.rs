//! Controlled HTTP evidence; no authenticated model calls or user session files.
use super::agent_diagnostics::agent;
use super::*;
use serde_json::json;
use std::{fs, path::Path};

const RESPONSE: &str = r#"{"id":"response-1","model":"served/model","stop_reason":"end_turn","usage":{"input_tokens":12,"output_tokens":3},"content":[{"type":"text","text":"private assistant text"},{"type":"thinking","thinking":"private reasoning","signature":"private signature"},{"type":"future_private_tag","data":"private block"}]}"#;
const EMPTY: &str = r#"{"id":"empty-response","stop_reason":"end_turn","content":[]}"#;

pub(crate) fn log(root: &Path) -> Vec<Value> {
    let paths: Vec<_> = fs::read_dir(root.join("robbie/sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(paths.len(), 1);
    fs::read_to_string(&paths[0])
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(crate) fn events<'a>(records: &'a [Value], event: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|record| record["event"] == event)
        .collect()
}

#[tokio::test]
async fn success_records_metadata_and_opt_in_wire_bodies_without_headers() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![Reply::Http(200, "", RESPONSE)]);
        let model = server.model_with_recorder(recorder);
        let reply = model
            .complete(
                "main",
                "private system",
                &[Message::User("private prompt".into())],
                &[],
            )
            .await
            .unwrap();
        assert_eq!(reply.reply.text(), "private assistant text");
        let records = log(root.path());
        assert_eq!(
            records
                .iter()
                .map(|r| r["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "request_started",
                "attempt_started",
                "attempt_finished",
                "model_response",
                "request_finished"
            ]
        );
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record["request_id"], 1);
            assert_eq!(record["source"], "main");
            assert_eq!(record["sequence"], index + 1);
            assert_eq!(record["session_id"], records[0]["session_id"]);
        }
        assert_eq!(records[1]["attempt"], 1);
        assert_eq!(records[2]["status"], 200);
        assert_eq!(records[2]["outcome"]["kind"], "succeeded");
        assert_eq!(records[3]["response"]["response_id"], "response-1");
        assert_eq!(records[3]["response"]["response_model"], "served/model");
        assert_eq!(records[3]["response"]["stop_reason"], "end_turn");
        assert_eq!(records[3]["response"]["usage"]["output_tokens"], 3);
        assert_eq!(records[3]["response"]["blocks"]["unknown"], 1);
        assert_eq!(
            records[3]["response"]["text_bytes"],
            "private assistant text".len()
        );
        assert_eq!(records[4]["outcome"], "succeeded");
        if transcript {
            let requests = server.requests.lock().unwrap();
            let actual: Value =
                serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(records[0]["content"]["body"], actual);
            assert_eq!(actual["provider"]["only"], json!(["test-provider"]));
            assert_eq!(records[2]["content"]["body"], RESPONSE);
        } else {
            assert!(records.iter().all(|record| record.get("content").is_none()));
            assert!(!serde_json::to_string(&records).unwrap().contains("private"));
        }
        let serialized = serde_json::to_string(&records).unwrap();
        for forbidden in [
            "test-secret",
            "authorization",
            "x-openrouter-cache",
            "127.0.0.1",
        ] {
            assert!(!serialized.to_lowercase().contains(forbidden));
        }
    }
}

#[tokio::test]
async fn retries_keep_request_identity_attempt_numbers_and_then_allocate_a_new_request() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(503, "", "private error body"),
        Reply::Http(200, "", RESPONSE),
        Reply::Http(200, "", EMPTY),
    ]);
    let model = server.model_with_recorder(recorder);
    for _ in 0..2 {
        model
            .complete_with_policy("main", "private system", &[], &[], fast_policy())
            .await
            .unwrap();
    }
    let records = log(root.path());
    let attempts = events(&records, "attempt_finished");
    assert_eq!(
        attempts
            .iter()
            .map(|r| (
                r["request_id"].as_u64().unwrap(),
                r["attempt"].as_u64().unwrap(),
                r["status"].as_u64().unwrap()
            ))
            .collect::<Vec<_>>(),
        [(1, 1, 503), (1, 2, 200), (2, 1, 200)]
    );
    let retries = events(&records, "retry_scheduled");
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0]["request_id"], 1);
    assert_eq!(retries[0]["attempt"], 1);
    assert_eq!(retries[0]["delay_ms"], 1);
    let responses = events(&records, "model_response");
    assert_eq!(responses[1]["response"]["response_id"], "empty-response");
    assert_eq!(responses[1]["response"]["text_bytes"], 0);
    assert!(responses[1]["response"].get("usage").is_none());
    assert_eq!(events(&records, "request_finished").len(), 2);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[0], requests[1]);
}

#[tokio::test]
async fn terminal_http_json_and_protocol_errors_are_distinct_and_sanitized() {
    let cases = [
        (
            vec![Reply::Http(401, "", "private provider failure")],
            "http_error",
            "exchange_failed",
            Some("permanent_failure"),
        ),
        (
            vec![Reply::Http(
                429,
                "Retry-After: 60\r\n",
                "private provider failure",
            )],
            "http_error",
            "exchange_failed",
            Some("delay_limit"),
        ),
        (
            (0..4)
                .map(|_| Reply::Http(503, "", "private provider failure"))
                .collect(),
            "http_error",
            "exchange_failed",
            Some("budget_exhausted"),
        ),
        (
            vec![Reply::Http(200, "", "private malformed JSON")],
            "invalid_json",
            "exchange_failed",
            None,
        ),
        (
            vec![Reply::Http(
                200,
                "",
                r#"{"content":[{"type":"tool_use","name":"private_tool","input":{}}]}"#,
            )],
            "succeeded",
            "protocol_decode_failed",
            None,
        ),
    ];
    for (replies, attempt_kind, request_outcome, retry_stop) in cases {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let expected_attempts = replies.len();
        let server = Server::new(replies);
        let model = server.model_with_recorder(recorder);
        assert!(
            model
                .complete_with_policy("main", "", &[], &[], fast_policy())
                .await
                .is_err()
        );
        let records = log(root.path());
        let attempts = events(&records, "attempt_finished");
        assert_eq!(attempts.len(), expected_attempts);
        assert_eq!(attempts.last().unwrap()["outcome"]["kind"], attempt_kind);
        let finishes = events(&records, "request_finished");
        assert_eq!(finishes.len(), 1);
        assert_eq!(finishes[0]["outcome"], request_outcome);
        let stops = events(&records, "retry_stopped");
        if let Some(reason) = retry_stop {
            assert_eq!(stops.len(), 1);
            assert_eq!(stops[0]["reason"], reason);
        } else {
            assert!(stops.is_empty());
        }
        assert_eq!(
            events(&records, "model_response").len(),
            usize::from(request_outcome == "protocol_decode_failed")
        );
        let serialized = serde_json::to_string(&records).unwrap();
        for forbidden in ["private", "test-secret", "127.0.0.1"] {
            assert!(!serialized.contains(forbidden));
        }
        assert_eq!(server.count(), expected_attempts);
    }
}

#[tokio::test]
async fn malformed_blocks_preserve_available_metadata_without_executing_tools() {
    for transcript in [false, true] {
        for (body, text_bytes, text_blocks, tool_blocks) in [
            (
                r#"{"id":"response-invalid","model":"served/model","stop_reason":"max_tokens","usage":{"input_tokens":12,"output_tokens":3,"cache_read_input_tokens":7},"content":[{"type":"tool_use","id":"private-valid-call","name":"bash","input":{"command":"printf private-output"}},{"type":"text","text":"private text"},{"type":"text","text":42}]}"#,
                None,
                2,
                1,
            ),
            (
                r#"{"id":"response-invalid","model":"served/model","stop_reason":"max_tokens","usage":{"input_tokens":12,"output_tokens":3,"cache_read_input_tokens":7},"content":[{"type":"tool_use","id":"private-valid-call","name":"bash","input":{"command":"printf private-output"}},{"type":"text","text":"private text"},{"type":"tool_use","id":"private-bad-call","name":"bash","input":42}]}"#,
                Some(12),
                1,
                2,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let recorder = Recorder::for_test(root.path(), transcript);
            // A valid action before the bad block must not be partially executed.
            let server = Server::new(vec![Reply::Http(200, "", body)]);
            let mut agent = agent(server.model_with_recorder(recorder));
            assert!(agent.run("private prompt").await.is_err());
            assert_eq!(
                agent.messages.len(),
                1,
                "only the submitted user message survives"
            );
            assert_eq!(server.count(), 1);
            let records = log(root.path());
            let responses = events(&records, "model_response");
            assert_eq!(
                responses.len(),
                1,
                "valid diagnostic fields survive malformed executable blocks"
            );
            let response = &responses[0]["response"];
            assert_eq!(response["response_id"], "response-invalid");
            assert_eq!(response["response_model"], "served/model");
            assert_eq!(response["stop_reason"], "max_tokens");
            assert_eq!(response["usage"]["input_tokens"], 12);
            assert_eq!(response["usage"]["output_tokens"], 3);
            assert_eq!(response["usage"]["cache_read_input_tokens"], 7);
            assert_eq!(response["blocks"]["tool_use"], tool_blocks);
            assert_eq!(response["blocks"]["text"], text_blocks);
            if let Some(bytes) = text_bytes {
                assert_eq!(response["text_bytes"], bytes);
            } else {
                assert!(
                    response.get("text_bytes").is_none(),
                    "malformed text does not invent a byte count"
                );
            }
            assert_eq!(
                events(&records, "request_finished")[0]["outcome"],
                "protocol_decode_failed"
            );
            assert_eq!(
                events(&records, "agent_turn_finished")[0]["reason"],
                "model_failed"
            );
            for absent in [
                "tool_execution_started",
                "agent_decision",
                "agent_text_emitted",
                "retry_scheduled",
            ] {
                assert!(events(&records, absent).is_empty(), "{absent}");
            }
            assert!(responses[0].get("content").is_none());
            if transcript {
                assert_eq!(
                    events(&records, "attempt_finished")[0]["content"]["body"],
                    body
                );
            } else {
                assert!(records.iter().all(|r| r.get("content").is_none()));
                let serialized = serde_json::to_string(&records).unwrap();
                assert!(!serialized.contains("private"));
            }
        }
    }
}

#[tokio::test]
async fn unavailable_content_counts_are_absent_while_top_level_facts_survive() {
    for body in [
        r#"{"id":"response-invalid","stop_reason":"max_tokens","usage":{"output_tokens":3}}"#,
        r#"{"id":"response-invalid","stop_reason":"max_tokens","usage":{"output_tokens":3},"content":null}"#,
        r#"{"id":"response-invalid","stop_reason":"max_tokens","usage":{"output_tokens":3},"content":{"private":"secret"}}"#,
    ] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![Reply::Http(200, "", body)]);
        let model = server.model_with_recorder(recorder);
        assert!(model.complete("main", "", &[], &[]).await.is_err());
        let records = log(root.path());
        let responses = events(&records, "model_response");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            responses[0]["response"],
            json!({
                "response_id": "response-invalid", "stop_reason": "max_tokens",
                "usage": {"output_tokens": 3}
            })
        );
        assert_eq!(
            events(&records, "request_finished")[0]["outcome"],
            "protocol_decode_failed"
        );
        assert!(events(&records, "retry_scheduled").is_empty());
        assert!(records.iter().all(|r| r.get("content").is_none()));
        assert!(!serde_json::to_string(&records).unwrap().contains("private"));
        assert_eq!(server.count(), 1);
    }
}

#[tokio::test]
async fn malformed_success_body_is_captured_only_when_opted_in() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), true);
    let server = Server::new(vec![Reply::Http(200, "", "private malformed JSON")]);
    let model = server.model_with_recorder(recorder);
    assert!(model.complete("main", "", &[], &[]).await.is_err());
    let records = log(root.path());
    assert_eq!(
        events(&records, "attempt_finished")[0]["content"]["body"],
        "private malformed JSON"
    );
}

#[tokio::test]
async fn transport_failures_keep_known_status_and_retry_without_exposing_partial_bodies() {
    for failure in [Reply::Disconnect, Reply::TruncatedBody] {
        let had_status = matches!(failure, Reply::TruncatedBody);
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![failure, Reply::Http(200, "", SUCCESS)]);
        let model = server.model_with_recorder(recorder);
        model
            .complete_with_policy("main", "", &[], &[], fast_policy())
            .await
            .unwrap();
        let records = log(root.path());
        let attempts = events(&records, "attempt_finished");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["outcome"]["kind"], "transport_error");
        assert_eq!(attempts[0].get("status").is_some(), had_status);
        if had_status {
            assert_eq!(attempts[0]["status"], 200);
            assert_eq!(attempts[0]["outcome"]["reason"], "body");
        }
        assert!(attempts[0].get("content").is_none());
        assert_eq!(events(&records, "retry_scheduled").len(), 1);
    }
}

#[tokio::test]
async fn attempt_timeouts_are_recorded_as_failures_not_guard_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![Reply::Stall]);
    let model = server.model_with_recorder(recorder);
    let policy = Policy {
        attempt_timeout: Duration::from_millis(20),
        ..fast_policy()
    };
    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            model.complete_with_policy("main", "", &[], &[], policy)
        )
        .await
        .unwrap()
        .is_err()
    );
    let records = log(root.path());
    let attempts = events(&records, "attempt_finished");
    assert_eq!(attempts.len(), 4);
    for attempt in attempts {
        assert_eq!(attempt["outcome"]["kind"], "transport_error");
        assert_eq!(attempt["outcome"]["reason"], "timeout");
    }
    assert_eq!(
        events(&records, "retry_stopped")[0]["reason"],
        "budget_exhausted"
    );
    assert_eq!(
        events(&records, "request_finished")[0]["outcome"],
        "exchange_failed"
    );
}

#[tokio::test]
async fn starts_are_visible_before_completion_and_drop_records_request_and_attempt_cancellation() {
    for response in [Reply::Stall, Reply::StallBody] {
        let body_started = matches!(response, Reply::StallBody);
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![response]);
        let model = server.model_with_recorder(recorder);
        let mut future = Box::pin(model.complete("main", "", &[], &[]));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), future.as_mut())
                .await
                .is_err()
        );
        let before = log(root.path());
        assert_eq!(events(&before, "request_started").len(), 1);
        assert_eq!(events(&before, "attempt_started").len(), 1);
        assert!(events(&before, "request_finished").is_empty());
        drop(future);
        let after = log(root.path());
        let attempts = events(&after, "attempt_finished");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["outcome"]["kind"], "cancelled");
        assert_eq!(attempts[0].get("status").is_some(), body_started);
        assert_eq!(
            events(&after, "request_finished")[0]["outcome"],
            "cancelled"
        );
        assert!(events(&after, "retry_scheduled").is_empty());
    }
}

#[tokio::test]
async fn backoff_cancellation_does_not_turn_a_completed_attempt_into_a_cancelled_attempt() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![Reply::Http(429, "Retry-After: 1\r\n", "private busy")]);
    let model = server.model_with_recorder(recorder);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            model.complete("main", "", &[], &[])
        )
        .await
        .is_err()
    );
    let records = log(root.path());
    let attempts = events(&records, "attempt_finished");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["outcome"]["kind"], "http_error");
    assert_eq!(events(&records, "retry_scheduled")[0]["delay_ms"], 1000);
    assert_eq!(
        events(&records, "request_finished")[0]["outcome"],
        "cancelled"
    );
    tokio::time::sleep(Duration::from_millis(1050)).await;
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn recorder_failure_does_not_fail_or_replay_tool_execution() {
    use crate::{Agent, ToolRegistry, events::EventKind};
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    recorder.break_writer_for_test();
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"one-call","name":"bash","input":{"command":"printf one-execution"}}]}"#,
        ),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut agent = Agent {
        model: server.model_with_recorder(recorder.clone()),
        tools: ToolRegistry::new(),
        events,
        source: "main",
        messages: Vec::new(),
    };
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), agent.run("do the tool"))
            .await
            .unwrap()
            .unwrap(),
        "done"
    );
    assert!(recorder.take_warning().unwrap().contains("disabled"));
    assert!(recorder.take_warning().is_none());
    let mut starts = 0;
    while let Ok(event) = receiver.try_recv() {
        starts += usize::from(matches!(event.kind, EventKind::ToolStarted { .. }));
    }
    assert_eq!(starts, 1);
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn token_limit_metadata_is_not_a_retry_or_automatic_continuation_policy() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![Reply::Http(
        200,
        "",
        r#"{"stop_reason":"max_tokens","content":[{"type":"text","text":"partial answer"}]}"#,
    )]);
    let mut agent = agent(server.model_with_recorder(recorder));
    assert_eq!(agent.run("test").await.unwrap(), "partial answer");
    let records = log(root.path());
    assert_eq!(
        events(&records, "model_response")[0]["response"]["stop_reason"],
        "max_tokens"
    );
    assert_eq!(
        events(&records, "request_finished")[0]["outcome"],
        "succeeded"
    );
    assert_eq!(
        events(&records, "agent_decision")[0]["decision"],
        "stop_no_tool_results"
    );
    assert_eq!(
        events(&records, "agent_turn_finished")[0]["reason"],
        "no_tool_results"
    );
    assert!(events(&records, "retry_scheduled").is_empty());
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn empty_success_after_a_tool_has_its_own_response_evidence() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"one-call","name":"bash","input":{"command":"printf one-execution"}}]}"#,
        ),
        Reply::Http(200, "", EMPTY),
    ]);
    let mut agent = agent(server.model_with_recorder(recorder));
    assert_eq!(agent.run("test").await.unwrap(), "");
    let records = log(root.path());
    let responses = events(&records, "model_response");
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["response"]["blocks"]["tool_use"], 1);
    assert_eq!(responses[1]["response"]["stop_reason"], "end_turn");
    assert_eq!(responses[1]["response"]["text_bytes"], 0);
    assert_eq!(
        responses[1]["response"]["blocks"],
        json!({"text":0,"thinking":0,"redacted_thinking":0,"tool_use":0,"unknown":0})
    );
    assert_ne!(responses[0]["request_id"], responses[1]["request_id"]);
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn opt_in_does_not_start_consuming_non_success_error_bodies() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), true);
    let server = Server::new(vec![Reply::Http(401, "", "private provider error body")]);
    let model = server.model_with_recorder(recorder);
    assert!(model.complete("main", "", &[], &[]).await.is_err());
    let records = log(root.path());
    let attempts = events(&records, "attempt_finished");
    assert_eq!(attempts[0]["status"], 401);
    assert!(attempts[0].get("content").is_none());
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("private provider error body")
    );
}

#[tokio::test]
async fn real_children_use_the_same_journal_but_distinct_requests_and_source() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", SUCCESS),
    ]);
    let mut parent = agent(server.model_with_recorder(recorder));
    parent
        .tools
        .enable_sub_agent(Arc::clone(&parent.model), parent.events.clone());
    parent.run("private parent task").await.unwrap();
    assert!(
        !parent
            .tools
            .execute("sub_agent", &json!({"prompt": "private child task"}))
            .await
            .1
    );
    let records = log(root.path());
    let starts = events(&records, "request_started");
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["source"], "main");
    assert_eq!(starts[1]["source"], "subagent");
    assert_ne!(starts[0]["request_id"], starts[1]["request_id"]);
    assert_eq!(starts[0]["session_id"], starts[1]["session_id"]);
    let turns = events(&records, "agent_turn_started");
    assert_eq!(turns.len(), 2);
    assert_ne!(turns[0]["turn_id"], turns[1]["turn_id"]);
    for (turn, request) in turns.iter().zip(&starts) {
        assert_eq!(turn["turn_id"], request["turn_id"]);
        assert_eq!(turn["source"], request["source"]);
    }
    assert_eq!(events(&records, "agent_turn_finished").len(), 2);
    assert!(!serde_json::to_string(&records).unwrap().contains("private"));
}
