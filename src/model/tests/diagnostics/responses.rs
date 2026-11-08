use super::*;

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
async fn real_children_use_the_same_journal_but_distinct_requests_and_source() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", SUCCESS),
    ]);
    let mut parent = agent(server.model_with_recorder(recorder));
    parent.enable_sub_agent();
    parent.run("private parent task").await.unwrap();
    assert!(
        !parent
            .tools_for_test()
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
