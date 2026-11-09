use super::*;

#[tokio::test]
async fn repeated_compaction_merges_the_previous_summary_once_without_resending_retired_history() {
    let server = Server::new(vec![
        Reply::Http(200, "", SUMMARY),
        Reply::Http(200, "", SECOND),
    ]);
    let model = server.model();
    let mut messages = history();
    let mut previous = None;
    let bounds = limits(&messages, 2);
    compact_history(
        &model,
        "compaction",
        &mut messages,
        &mut previous,
        bounds,
        estimate,
    )
    .await
    .unwrap()
    .unwrap();

    messages.extend([
        Message::User("Continue the same task.".into()),
        assistant(
            json!([{"type":"tool_use","id":"C","name":"read_file","input":{"path":"next.rs"}}]),
        ),
        Message::ToolResults(vec![ToolResult {
            tool_call_id: "C".into(),
            content: "NEW_RETAINED_RESULT".into(),
            is_error: false,
        }]),
    ]);
    let new_tail = wire(&messages[2..]);
    let bounds = limits(&messages, 3);
    let committed = compact_history(
        &model,
        "compaction",
        &mut messages,
        &mut previous,
        bounds,
        estimate,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(committed.retired_messages, 2);
    assert_eq!(committed.retained_messages, 3);
    assert_eq!(
        previous.as_deref(),
        Some("SECOND_SUMMARY: earlier constraints plus new progress.")
    );
    assert_eq!(wire(&messages), new_tail);
    let requests = bodies(&server);
    let prompt = requests[1]["messages"][0]["content"].as_str().unwrap();
    assert_eq!(prompt.matches("FIRST_SUMMARY").count(), 1);
    assert!(prompt.contains("[Previous checkpoint]\nFIRST_SUMMARY"));
    assert!(prompt.contains("PRIVATE_TAIL_RESULT"));
    assert!(!prompt.contains("PRIVATE_OLD_RESULT"));
    assert!(!prompt.contains("NEW_RETAINED_RESULT"));
}

#[tokio::test]
async fn rejected_or_failed_responses_leave_both_history_and_previous_summary_unchanged() {
    for (status, body) in [
        (200, r#"{"stop_reason":"end_turn","content":[]}"#),
        (
            200,
            r#"{"stop_reason":"end_turn","content":[{"type":"text","text":" \n\t "}]}"#,
        ),
        (
            200,
            r#"{"stop_reason":"max_tokens","content":[{"type":"text","text":"partial"}]}"#,
        ),
        (
            200,
            r#"{"stop_reason":"refusal","content":[{"type":"text","text":"refused"}]}"#,
        ),
        (
            200,
            r#"{"stop_reason":"pause_turn","content":[{"type":"text","text":"paused"}]}"#,
        ),
        (
            200,
            r#"{"stop_reason":"future_stop","content":[{"type":"text","text":"unknown stop"}]}"#,
        ),
        (200, SUCCESS), // Normal requests accept absent stop metadata; summaries must not.
        (
            200,
            r#"{"stop_reason":"end_turn","content":[{"type":"text","text":"candidate"},{"type":"tool_use","id":"X","name":"bash","input":{"command":"printf should-not-run"}}]}"#,
        ),
        (
            200,
            r#"{"stop_reason":"end_turn","content":[{"type":"text","text":42}]}"#,
        ),
        (200, "not JSON"),
        (400, "bad request"),
    ] {
        let server = Server::new(vec![Reply::Http(status, "", body)]);
        let model = server.model();
        let mut messages = history();
        let before = wire(&messages);
        let mut previous = Some("EXISTING_SUMMARY".into());
        let bounds = limits(&messages, 2);

        assert!(
            compact_history(
                &model,
                "compaction",
                &mut messages,
                &mut previous,
                bounds,
                estimate
            )
            .await
            .is_err(),
            "accepted {body}"
        );

        assert_eq!(wire(&messages), before);
        assert_eq!(previous.as_deref(), Some("EXISTING_SUMMARY"));
        assert_eq!(
            server.count(),
            1,
            "candidate rejection must not retry generation"
        );
    }
}

#[tokio::test]
async fn a_nonshrinking_candidate_is_not_committed() {
    let server = Server::new(vec![Reply::Http(200, "", SUMMARY)]);
    let model = server.model();
    let mut messages = history();
    let before = wire(&messages);
    let mut previous = None;
    let bounds = Limits {
        keep_recent_tokens: 2,
        max_input_chars: 20_000,
        max_output_tokens: 512,
    };
    // Deliberately supplied fixture units: the proposed summary costs more.
    let oversized_summary = |message: &Message| match message {
        Message::User(text) if text.starts_with("The earlier conversation was compacted") => 100,
        _ => 1,
    };

    let error = compact_history(
        &model,
        "compaction",
        &mut messages,
        &mut previous,
        bounds,
        oversized_summary,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "compaction did not reduce estimated context size"
    );
    assert_eq!(wire(&messages), before);
    assert!(previous.is_none());
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn no_op_invalid_exchanges_and_bad_limits_never_send_a_request() {
    let server = Server::new(vec![]);
    let model = server.model();
    let mut empty = Vec::new();
    let mut previous = Some("EXISTING_SUMMARY".into());
    let bounds = Limits {
        keep_recent_tokens: 1,
        max_input_chars: 20_000,
        max_output_tokens: 512,
    };
    assert!(
        compact_history(
            &model,
            "compaction",
            &mut empty,
            &mut previous,
            bounds,
            estimate
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(previous.as_deref(), Some("EXISTING_SUMMARY"));

    for case in ["exchange", "input", "output"] {
        let mut messages = history();
        let mut bounds = limits(&messages, 2);
        match case {
            "exchange" => {
                messages.pop();
            }
            "input" => bounds.max_input_chars = 1,
            "output" => bounds.max_output_tokens = 0,
            _ => unreachable!(),
        }
        let before = wire(&messages);
        assert!(
            compact_history(
                &model,
                "compaction",
                &mut messages,
                &mut previous,
                bounds,
                estimate
            )
            .await
            .is_err()
        );
        assert_eq!(wire(&messages), before);
        assert_eq!(previous.as_deref(), Some("EXISTING_SUMMARY"));
        assert_eq!(server.count(), 0);
    }
}

#[tokio::test]
async fn dropping_a_pending_summary_preserves_history_and_previous_summary() {
    let server = Server::new(vec![Reply::Stall]);
    let model = server.model();
    let mut messages = history();
    let before = wire(&messages);
    let bounds = limits(&messages, 2);
    let mut previous = Some("EXISTING_SUMMARY".into());
    let mut pending = Box::pin(compact_history(
        &model,
        "compaction",
        &mut messages,
        &mut previous,
        bounds,
        estimate,
    ));
    let received = tokio::time::timeout(Duration::from_secs(2), async {
        while server.count() == 0 {
            tokio::task::yield_now().await;
        }
    });
    tokio::select! {
        result = pending.as_mut() => panic!("pending fixture unexpectedly completed: {result:?}"),
        result = received => result.unwrap(),
    }
    drop(pending);

    assert_eq!(wire(&messages), before);
    assert_eq!(previous.as_deref(), Some("EXISTING_SUMMARY"));
    assert_eq!(server.count(), 1);
}
