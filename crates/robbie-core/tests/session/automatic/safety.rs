use super::*;

#[tokio::test]
async fn failed_summary_does_not_send_an_ordinary_request_that_still_exceeds_budget() {
    let root = tempfile::tempdir().unwrap();
    let rejected = response(json!([{"type":"text","text":" "}]), 45000, "end_turn");
    let server = Server::new(vec![done(61000), rejected]);
    let (mut session, _) = connect(&server, root.path(), limits(), policy()).await;
    session.prepare_turn().run(&old_prompt()).await.unwrap();
    assert!(
        session
            .prepare_turn()
            .run(&new_prompt())
            .await
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
    assert_eq!(
        server.count(),
        2,
        "one ordinary request, one failed summary, no retry"
    );
    assert_eq!(session.statistics().context.unwrap().reported_tokens, 61010);
    let records = log(&root.path().join("state"));
    assert_eq!(
        events(&records, "compaction_finished")[0]["outcome"],
        "failed"
    );
    assert_eq!(
        events(&records, "agent_turn_finished")[1]["reason"],
        "context_budget_failed"
    );
    assert_eq!(
        events(&records, "agent_turn_finished")[1]["totals"]["model_calls"],
        0
    );
}

#[tokio::test]
async fn failed_summary_continues_if_safe_and_does_not_retry_during_the_tool_loop() {
    let root = tempfile::tempdir().unwrap();
    let rejected = response(json!([{"type":"text","text":" "}]), 45000, "end_turn");
    let server = Server::new(vec![done(47000), rejected, tool(47000), done(100)]);
    let (mut session, mut activity) = connect(&server, root.path(), limits(), policy()).await;
    session.prepare_turn().run(&old_prompt()).await.unwrap();
    session.prepare_turn().run(&new_prompt()).await.unwrap();
    let records = log(&root.path().join("state"));
    assert_eq!(events(&records, "compaction_started").len(), 1);
    assert_eq!(
        events(&records, "compaction_finished")[0]["outcome"],
        "failed"
    );
    assert_eq!(events(&records, "tool_execution_finished").len(), 1);
    let wire = bodies(&records);
    assert_eq!(wire.len(), 4);
    assert!(wire[2].to_string().contains("OLD_GOAL"));
    assert!(wire[3].to_string().contains("OLD_GOAL"));
    let mut warnings = 0;
    while let Ok(event) = activity.try_recv() {
        if matches!(event.kind, EventKind::Warning(_)) {
            warnings += 1;
        }
    }
    assert_eq!(warnings, 1);
}

#[tokio::test]
async fn oversized_input_or_fixed_prompt_sections_stop_before_any_inference() {
    for (selected, prompt) in [
        (limits(), "x".repeat(300000)),
        (
            ModelLimits {
                max_input_tokens: Some(100),
                ..limits()
            },
            "small user input".into(),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![]);
        let (mut session, mut activity) = connect(&server, root.path(), selected, policy()).await;
        let error = session
            .prepare_turn()
            .run(&prompt)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("budget"));
        assert_eq!(server.count(), 0);
        assert_eq!(session.statistics().totals.responses, 0);
        let records = log(&root.path().join("state"));
        assert_eq!(events(&records, "compaction_started").len(), 1);
        assert_eq!(
            events(&records, "agent_turn_finished")[0]["reason"],
            "context_budget_failed"
        );
        assert!(events(&records, "request_started").is_empty());
        while let Ok(event) = activity.try_recv() {
            assert!(!matches!(
                event.kind,
                EventKind::ModelStarted | EventKind::AssistantText(..)
            ));
        }
    }
}

#[tokio::test]
async fn no_useful_cut_is_attempted_once_then_ordinary_tool_continuation_remains_safe() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![tool(47000), done(100)]);
    let (mut session, _) = connect(
        &server,
        root.path(),
        limits(),
        CompactionPolicy {
            reserve_tokens: 22000,
            ..policy()
        },
    )
    .await;
    session
        .prepare_turn()
        .run(&"x".repeat(180000))
        .await
        .unwrap();
    let records = log(&root.path().join("state"));
    assert_eq!(server.count(), 2);
    assert_eq!(events(&records, "compaction_started").len(), 1);
    assert_eq!(
        events(&records, "compaction_finished")[0]["outcome"],
        "unchanged"
    );
    assert_eq!(events(&records, "tool_execution_finished").len(), 1);
}

#[tokio::test]
async fn opt_out_preserves_ordinary_requests_and_cancellation_preserves_the_old_baseline() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![done(100)]);
    let (mut session, _) = connect(
        &server,
        root.path(),
        limits(),
        CompactionPolicy {
            enabled: false,
            ..policy()
        },
    )
    .await;
    session
        .prepare_turn()
        .run(&"x".repeat(300000))
        .await
        .unwrap();
    assert_eq!(server.count(), 1);
    assert!(!session.statistics().estimate.unwrap().automatic);
    assert!(events(&log(&root.path().join("state")), "compaction_started").is_empty());

    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![done(47000), Reply::Stall]);
    let (mut session, _) = connect(&server, root.path(), limits(), policy()).await;
    session.prepare_turn().run(&old_prompt()).await.unwrap();
    let prompt = new_prompt();
    let mut pending = Box::pin(session.prepare_turn().run(&prompt));
    let observed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while server.count() < 2 {
            tokio::task::yield_now().await;
        }
    });
    tokio::select! {
        result = pending.as_mut() => panic!("unexpected completion: {result:?}"),
        result = observed => result.unwrap(),
    }
    drop(pending);
    assert_eq!(session.statistics().totals.responses, 1);
    assert_eq!(session.statistics().context.unwrap().reported_tokens, 47010);
    assert_eq!(session.statistics().context.unwrap().trailing_messages, 1);
    let records = log(&root.path().join("state"));
    assert_eq!(
        events(&records, "compaction_finished")[0]["outcome"],
        "cancelled"
    );
    assert_eq!(
        events(&records, "agent_turn_finished")[1]["reason"],
        "cancelled"
    );
}

#[tokio::test]
async fn tools_requested_by_a_summary_are_rejected_not_executed() {
    let root = tempfile::tempdir().unwrap();
    let malicious = response(
        json!([{"type":"tool_use","id":"forbidden","name":"write_file","input":{"path":"forbidden.txt","content":"no"}}]),
        45000,
        "end_turn",
    );
    let server = Server::new(vec![done(47000), malicious, done(100)]);
    let (mut session, _) = connect(&server, root.path(), limits(), policy()).await;
    session.prepare_turn().run(&old_prompt()).await.unwrap();
    session.prepare_turn().run(&new_prompt()).await.unwrap();
    assert!(!root.path().join("workspace/forbidden.txt").exists());
    let records = log(&root.path().join("state"));
    assert!(events(&records, "tool_execution_started").is_empty());
    assert_eq!(
        events(&records, "compaction_finished")[0]["outcome"],
        "failed"
    );
}
