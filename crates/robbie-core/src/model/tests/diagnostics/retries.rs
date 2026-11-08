use super::*;

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
async fn cancellation_during_error_body_read_closes_guards_without_retrying() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), true);
    let server = Server::new(vec![Reply::StallHttpBody(429)]);
    let model = server.model_with_recorder(recorder);
    let mut run = Box::pin(model.complete_with_policy("main", "", &[], &[], fast_policy()));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut run)
            .await
            .is_err()
    );
    assert!(events(&log(root.path()), "attempt_finished").is_empty());
    drop(run);
    let records = log(root.path());
    let attempt = events(&records, "attempt_finished")[0];
    assert_eq!(attempt["status"], 429);
    assert_eq!(attempt["outcome"]["kind"], "cancelled");
    assert!(attempt.get("content").is_none());
    assert_eq!(
        events(&records, "request_finished")[0]["outcome"],
        "cancelled"
    );
    assert!(events(&records, "retry_scheduled").is_empty());
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn recorder_failure_does_not_change_error_body_handling_or_retries() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), true);
    let server = Server::new(vec![
        Reply::Http(
            429,
            "",
            r#"{"error":{"code":429,"message":"PRIVATE_REASON"}}"#,
        ),
        Reply::Http(200, "", EMPTY),
    ]);
    let model = server.model_with_recorder(recorder.clone());
    recorder.break_writer_for_test();
    model
        .complete_with_policy("main", "", &[], &[], fast_policy())
        .await
        .unwrap();
    assert_eq!(server.count(), 2);
    assert!(recorder.take_warning().is_some());
}

#[tokio::test]
async fn retry_after_evidence_distinguishes_server_waits_invalid_values_and_delay_limits() {
    for (headers, state, expected_delay, source) in [
        (
            "Retry-After: 1\r\n",
            "delay",
            Some(1000),
            Some("retry_after"),
        ),
        (
            "Retry-After: PRIVATE_INVALID_HEADER\r\n",
            "invalid",
            None,
            Some("backoff"),
        ),
        ("Retry-After: 31\r\n", "delay", Some(31000), None),
        (
            "Retry-After: 99999999999999999999999\r\n",
            "too_large",
            None,
            None,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let mut replies = vec![Reply::Http(429, headers, "PRIVATE_PROVIDER_REASON")];
        if source.is_some() {
            replies.push(Reply::Http(200, "", EMPTY));
        }
        let server = Server::new(replies);
        let model = server.model_with_recorder(recorder);
        let result = model
            .complete_with_policy("main", "", &[], &[], fast_policy())
            .await;
        assert_eq!(result.is_ok(), source.is_some());
        let records = log(root.path());
        let attempt = events(&records, "attempt_finished")[0];
        let after = &attempt["http_error"]["retry_after"];
        assert_eq!(after["kind"], state);
        assert_eq!(after["delay_ms"], json!(expected_delay));
        if let Some(source) = source {
            let retry = events(&records, "retry_scheduled")[0];
            assert_eq!(retry["delay_source"], source);
            assert_eq!(retry["delay_ms"], expected_delay.unwrap_or(1));
            assert_eq!(server.count(), 2);
        } else {
            assert!(events(&records, "retry_scheduled").is_empty());
            assert_eq!(
                events(&records, "retry_stopped")[0]["reason"],
                "delay_limit"
            );
            assert_eq!(server.count(), 1);
        }
        assert!(
            !serde_json::to_string(&records)
                .unwrap()
                .contains("PRIVATE_")
        );
    }
}
