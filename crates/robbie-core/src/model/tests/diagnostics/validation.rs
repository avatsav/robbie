use super::*;

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
                agent.history().len(),
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
