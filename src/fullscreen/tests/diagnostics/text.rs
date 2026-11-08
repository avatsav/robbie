use super::*;

#[tokio::test]
async fn text_is_correlated_received_and_appended_with_private_opt_in_content() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![Reply::Http(200, "", TEXT)]);
        let mut fixture = Fixture::new(&server, &recorder);
        let result = fixture.run().await;
        // Agent completion alone does not acknowledge frontend consumption.
        let before = log(root.path());
        assert_eq!(events(&before, "agent_text_emitted").len(), 2);
        assert!(events(&before, "ui_text_received").is_empty());
        assert!(events(&before, "ui_turn_finished").is_empty());
        fixture.consume(false);
        assert_eq!(fixture.finish(result).unwrap().len(), 21);
        assert!(fixture.screen().contains("é猫"));
        let records = log(root.path());
        let start = one(&records, "ui_turn_started");
        let end = one(&records, "ui_turn_finished");
        assert_eq!(
            start["turn_id"],
            one(&records, "agent_turn_started")["turn_id"]
        );
        assert_eq!(end["turn_id"], start["turn_id"]);
        assert_eq!(end["outcome"], "completed");
        assert_eq!(end["returned_text_bytes"], 21);
        assert_eq!(end["totals"]["root"]["received_blocks"], 2);
        assert_eq!(end["totals"]["root"]["received_bytes"], 20);
        assert_eq!(end["totals"]["root"]["appended_blocks"], 2);
        assert_eq!(end["totals"]["root"]["sanitized_bytes"], 18);
        assert_eq!(end["totals"]["root"]["non_whitespace_chars"], 12);
        let emitted = events(&records, "agent_text_emitted");
        let received = events(&records, "ui_text_received");
        let appended = events(&records, "ui_text_appended");
        for i in 0..2 {
            for field in ["turn_id", "request_id", "text_sequence", "source"] {
                assert_eq!(emitted[i][field], received[i][field]);
                assert_eq!(received[i][field], appended[i][field]);
            }
            assert_eq!(emitted[i]["delivery"], "enqueued");
            assert_eq!(received[i]["text_sequence"], i + 1);
            assert_eq!(received[i]["ui_turn"]["turn_id"], start["turn_id"]);
            assert!(received[i]["sequence"].as_u64() < appended[i]["sequence"].as_u64());
            assert_eq!(received[i].get("content").is_some(), transcript);
            assert!(appended[i].get("content").is_none());
            if transcript {
                assert_eq!(received[i]["content"]["text"], "**é猫**\u{1b}");
            }
        }
        if transcript {
            assert_eq!(start["content"]["text"], PROMPT);
        } else {
            assert!(records.iter().all(|r| r.get("content").is_none()));
            let serialized = serde_json::to_string(&records).unwrap();
            assert!(!serialized.contains(PROMPT));
            assert!(!serialized.contains("é猫"));
        }
        let serialized = serde_json::to_string(&records).unwrap();
        for secret in [
            "test-secret",
            "authorization",
            "http://",
            "x-openrouter-cache",
        ] {
            assert!(!serialized.to_lowercase().contains(secret));
        }
        assert_eq!(server.count(), 1);
        drop(fixture);
        assert_eq!(events(&log(root.path()), "ui_turn_finished").len(), 1);
    }
}

#[tokio::test]
async fn empty_and_whitespace_blocks_are_appends_not_a_claim_of_visible_text() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![Reply::Http(
        200,
        "",
        r#"{"content":[{"type":"text","text":""},{"type":"text","text":" \n\t"}]}"#,
    )]);
    let mut fixture = Fixture::new(&server, &recorder);
    let result = fixture.run().await;
    fixture.consume(false);
    assert_eq!(fixture.finish(result).unwrap().len(), 4);
    let records = log(root.path());
    let counts = &one(&records, "ui_turn_finished")["totals"]["root"];
    assert_eq!(counts["received_blocks"], 2);
    assert_eq!(counts["appended_blocks"], 2);
    assert_eq!(counts["received_bytes"], 3);
    assert_eq!(counts["sanitized_bytes"], 6); // Tabs expand to four spaces.
    assert_eq!(counts["non_whitespace_chars"], 0);
}

#[tokio::test]
async fn empty_follow_up_after_tools_is_not_downstream_text_loss() {
    for (first, preface_bytes) in [(TOOL, 0), (PREFACE_TOOL, 7)] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![
            Reply::Http(200, "", first),
            Reply::Http(200, "", EMPTY),
        ]);
        let mut fixture = Fixture::new(&server, &recorder);
        let result = fixture.run().await;
        fixture.consume(false);
        assert_eq!(fixture.finish(result).unwrap(), "");
        let records = log(root.path());
        assert_eq!(server.count(), 2);
        assert_eq!(
            one(&records, "tool_execution_finished")["outcome"],
            "succeeded"
        );
        assert_eq!(
            events(&records, "agent_decision")[0]["decision"],
            "continue_after_tool_results"
        );
        assert_eq!(
            events(&records, "agent_decision")[1]["decision"],
            "stop_no_tool_results"
        );
        assert_eq!(
            events(&records, "model_response")[1]["response"]["text_bytes"],
            0
        );
        let end = one(&records, "ui_turn_finished");
        assert_eq!(end["returned_text_bytes"], 0);
        assert_eq!(end["totals"]["root"]["received_bytes"], preface_bytes);
        assert_eq!(end["totals"]["root"]["sanitized_bytes"], preface_bytes);
        assert_eq!(end["outcome"], "completed");
    }
}

#[tokio::test]
async fn token_limit_is_observed_without_automatic_continuation() {
    for (body, bytes) in [
        (r#"{"stop_reason":"max_tokens","content":[]}"#, 0),
        (
            r#"{"stop_reason":"max_tokens","content":[{"type":"text","text":"partial"}]}"#,
            7,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![Reply::Http(200, "", body)]);
        let mut fixture = Fixture::new(&server, &recorder);
        let result = fixture.run().await;
        fixture.consume(false);
        fixture.finish(result);
        let records = log(root.path());
        assert_eq!(server.count(), 1);
        assert_eq!(
            one(&records, "model_response")["response"]["stop_reason"],
            "max_tokens"
        );
        assert_eq!(
            one(&records, "agent_decision")["decision"],
            "stop_no_tool_results"
        );
        let end = one(&records, "ui_turn_finished");
        assert_eq!(end["outcome"], "completed");
        assert_eq!(end["returned_text_bytes"], bytes);
        assert_eq!(end["totals"]["root"]["received_bytes"], bytes);
    }
}

#[tokio::test]
async fn enqueued_but_discarded_text_stays_distinct_from_empty_model_output() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![Reply::Http(200, "", TEXT)]);
    let mut fixture = Fixture::new(&server, &recorder);
    let result = fixture.run().await;
    fixture.consume(true);
    assert_eq!(fixture.finish(result).unwrap().len(), 21);
    assert!(
        !fixture.screen().contains("é猫"),
        "do not silently repair the loss from the returned string"
    );
    let records = log(root.path());
    assert_eq!(
        one(&records, "model_response")["response"]["text_bytes"],
        20
    );
    assert_eq!(
        one(&records, "agent_turn_finished")["totals"]["assistant_text_bytes"],
        20
    );
    assert!(
        events(&records, "agent_text_emitted")
            .iter()
            .all(|r| r["delivery"] == "enqueued")
    );
    assert!(events(&records, "ui_text_received").is_empty());
    assert!(events(&records, "ui_text_appended").is_empty());
    let end = one(&records, "ui_turn_finished");
    assert_eq!(end["returned_text_bytes"], 21);
    assert_eq!(end["totals"]["root"]["received_blocks"], 0);
}

#[tokio::test]
async fn child_text_joins_the_root_ui_turn_without_losing_its_own_request_identity() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(503, "Retry-After: 0\r\n", "private unavailable body"),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"delegating"},{"type":"tool_use","id":"child","name":"sub_agent","input":{"prompt":"private child task"}}]}"#,
        ),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"CHILD_OK"}]}"#,
        ),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"PARENT_OK"}]}"#,
        ),
    ]);
    let mut fixture = Fixture::new(&server, &recorder);
    let result = fixture.run().await;
    fixture.consume(false);
    assert_eq!(fixture.finish(result).unwrap(), "PARENT_OK");
    let screen = fixture.screen();
    assert!(screen.contains("CHILD_OK") && screen.contains("PARENT_OK"));
    let records = log(root.path());
    let start = one(&records, "ui_turn_started");
    let turns = events(&records, "agent_turn_started");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1]["parent"]["turn_id"], start["turn_id"]);
    assert_eq!(events(&records, "attempt_started").len(), 4);
    assert_eq!(events(&records, "retry_scheduled").len(), 1);
    assert_eq!(events(&records, "tool_execution_started").len(), 1);
    let received = events(&records, "ui_text_received");
    assert_eq!(received.len(), 3);
    assert_eq!(received[0]["turn_id"], start["turn_id"]);
    assert_eq!(received[1]["turn_id"], turns[1]["turn_id"]);
    assert_eq!(received[2]["turn_id"], start["turn_id"]);
    assert_eq!(received[0]["text_sequence"], 1);
    assert_eq!(received[1]["text_sequence"], 1);
    assert_eq!(received[2]["text_sequence"], 2);
    assert_ne!(received[0]["request_id"], received[2]["request_id"]);
    assert_ne!(received[1]["request_id"], received[2]["request_id"]);
    assert!(
        received
            .iter()
            .all(|r| r["ui_turn"]["turn_id"] == start["turn_id"])
    );
    let end = one(&records, "ui_turn_finished");
    assert_eq!(end["totals"]["root"]["received_bytes"], 19);
    assert_eq!(end["totals"]["other_turns"]["received_bytes"], 8);
    assert_eq!(end["totals"]["root"]["appended_blocks"], 2);
    assert_eq!(end["totals"]["other_turns"]["appended_blocks"], 1);
}
