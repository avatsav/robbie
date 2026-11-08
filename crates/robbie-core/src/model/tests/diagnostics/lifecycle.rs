use super::*;

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
    let mut agent = Agent::new(
        server.model_with_recorder(recorder.clone()),
        ToolRegistry::new(),
        events,
        "main",
    );
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
