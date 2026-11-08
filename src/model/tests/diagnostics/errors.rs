use super::*;

#[tokio::test]
async fn http_error_details_explain_rate_limits_without_leaking_provider_text() {
    const ERROR: &str = r#"{"error":{"code":429,"type":"rate_limit_error","message":"PRIVATE_PROVIDER_REASON","metadata":{"raw":"PRIVATE_UPSTREAM_DETAIL"}}}"#;
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![
            Reply::Http(
                429,
                "Retry-After: 0\r\nX-Private: PRIVATE_HEADER\r\n",
                ERROR,
            ),
            Reply::Http(200, "", EMPTY),
        ]);
        let model = server.model_with_recorder(recorder);
        model
            .complete_with_policy("main", "", &[], &[], fast_policy())
            .await
            .unwrap();
        let records = log(root.path());
        let attempts = events(&records, "attempt_finished");
        assert_eq!(attempts.len(), 2);
        let failure = attempts[0];
        assert_eq!(failure["outcome"]["kind"], "http_error");
        assert_eq!(failure["http_error"]["category"], "rate_limited");
        assert_eq!(failure["http_error"]["code"], 429);
        assert_eq!(failure["http_error"]["error_type"], "rate_limit_error");
        assert_eq!(failure["http_error"]["body_state"], "complete");
        assert_eq!(failure["http_error"]["captured_bytes"], ERROR.len());
        assert_eq!(
            failure["http_error"]["retry_after"],
            json!({"kind":"delay", "delay_ms":0})
        );
        let retries = events(&records, "retry_scheduled");
        assert_eq!(retries.len(), 1);
        assert_eq!(retries[0]["delay_source"], "backoff");
        assert_eq!(server.count(), 2);
        let serialized = serde_json::to_string(&records).unwrap();
        for secret in ["PRIVATE_HEADER", "test-secret", "127.0.0.1"] {
            assert!(!serialized.contains(secret));
        }
        if transcript {
            assert_eq!(failure["content"]["kind"], "http_error_response");
            assert_eq!(failure["content"]["body"], ERROR);
        } else {
            assert!(records.iter().all(|record| record.get("content").is_none()));
            assert!(!serialized.contains("PRIVATE_PROVIDER_REASON"));
            assert!(!serialized.contains("PRIVATE_UPSTREAM_DETAIL"));
        }
        assert!(attempts[1].get("http_error").is_none());
    }
}

#[tokio::test]
async fn error_categories_only_use_recognized_structured_codes_and_types() {
    for transcript in [false, true] {
        for (body, category, code) in [
            (
                "<html>PRIVATE_PROVIDER_REASON</html>",
                "unknown",
                Value::Null,
            ),
            (
                r#"{"error": "PRIVATE_PROVIDER_REASON"}"#,
                "unknown",
                Value::Null,
            ),
            (
                r#"{"error":{"message":"quota exceeded PRIVATE_PROVIDER_REASON"}}"#,
                "unknown",
                Value::Null,
            ),
            (
                r#"{"error":{"code":"PRIVATE_CODE","type":"PRIVATE_TYPE","message":"PRIVATE_PROVIDER_REASON"}}"#,
                "unknown",
                Value::Null,
            ),
            (
                r#"{"error":{"code":999999,"message":"PRIVATE_PROVIDER_REASON"}}"#,
                "unknown",
                Value::Null,
            ),
            (
                r#"{"error":{"code":"insufficient_quota","type":"rate_limit_error","message":"PRIVATE_PROVIDER_REASON"}}"#,
                "quota_exceeded",
                json!("insufficient_quota"),
            ),
            (
                r#"{"error":{"code":429,"type":"overloaded_error","message":"PRIVATE_PROVIDER_REASON"}}"#,
                "overloaded",
                json!(429),
            ),
            (
                r#"{"error":{"code":402,"message":"PRIVATE_PROVIDER_REASON"}}"#,
                "payment_required",
                json!(402),
            ),
            (
                r#"{"error":{"code":429,"message":"PRIVATE_PROVIDER_REASON"}}"#,
                "rate_limited",
                json!(429),
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let recorder = Recorder::for_test(root.path(), transcript);
            let server = Server::new(vec![Reply::Http(401, "", body)]);
            let model = server.model_with_recorder(recorder);
            let error = model.complete("main", "", &[], &[]).await.err().unwrap();
            assert!(!format!("{error:#}").contains("PRIVATE_"));
            let records = log(root.path());
            let attempt = events(&records, "attempt_finished")[0];
            assert_eq!(attempt["status"], 401);
            assert_eq!(attempt["outcome"]["kind"], "http_error");
            assert_eq!(attempt["http_error"]["category"], category);
            assert_eq!(attempt["http_error"]["code"], code);
            assert_eq!(attempt["http_error"]["body_state"], "complete");
            assert_eq!(attempt["http_error"]["retry_after"]["kind"], "missing");
            assert!(events(&records, "retry_scheduled").is_empty());
            assert_eq!(server.count(), 1, "body inspection cannot make a 401 retry");
            if transcript {
                assert_eq!(attempt["content"]["body"], body);
            } else {
                assert!(records.iter().all(|r| r.get("content").is_none()));
                assert!(
                    !serde_json::to_string(&records)
                        .unwrap()
                        .contains("PRIVATE_")
                );
            }
        }
    }
}

#[tokio::test]
async fn error_body_capture_has_a_strict_byte_cap_and_marks_truncation() {
    use crate::model::http_error::MAX_ERROR_BODY;
    for size in [
        MAX_ERROR_BODY - 1,
        MAX_ERROR_BODY,
        MAX_ERROR_BODY + 1,
        MAX_ERROR_BODY * 8,
    ] {
        let body: &'static str = Box::leak("x".repeat(size).into_boxed_str());
        for transcript in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let recorder = Recorder::for_test(root.path(), transcript);
            let server = Server::new(vec![Reply::Http(401, "", body)]);
            let model = server.model_with_recorder(recorder);
            assert!(model.complete("main", "", &[], &[]).await.is_err());
            let records = log(root.path());
            let attempt = events(&records, "attempt_finished")[0];
            let state = if size > MAX_ERROR_BODY {
                "truncated"
            } else {
                "complete"
            };
            assert_eq!(attempt["http_error"]["body_state"], state);
            assert_eq!(
                attempt["http_error"]["captured_bytes"],
                size.min(MAX_ERROR_BODY)
            );
            assert_eq!(attempt["http_error"]["category"], "unknown");
            if transcript {
                assert_eq!(
                    attempt["content"]["body"],
                    &body[..size.min(MAX_ERROR_BODY)]
                );
            } else {
                assert!(attempt.get("content").is_none());
            }
            assert_eq!(server.count(), 1);
        }
    }
}

#[tokio::test]
async fn invalid_utf8_error_bodies_preserve_bytes_only_with_opt_in() {
    const BODY: &[u8] = b"PRIVATE_PROVIDER_REASON\xff\x00";
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![Reply::HttpBytes(401, BODY)]);
        let model = server.model_with_recorder(recorder);
        assert!(model.complete("main", "", &[], &[]).await.is_err());
        let records = log(root.path());
        let attempt = events(&records, "attempt_finished")[0];
        assert_eq!(attempt["http_error"]["body_state"], "complete");
        assert_eq!(attempt["http_error"]["category"], "unknown");
        if transcript {
            assert_eq!(attempt["content"]["body"], json!(BODY));
        } else {
            assert!(attempt.get("content").is_none());
        }
    }
}

#[tokio::test]
async fn unreadable_error_bodies_preserve_http_status_retryability_and_private_prefixes() {
    for transcript in [false, true] {
        for status in [401, 429] {
            let root = tempfile::tempdir().unwrap();
            let recorder = Recorder::for_test(root.path(), transcript);
            let mut replies = vec![Reply::TruncatedHttpBody(status)];
            if status == 429 {
                replies.push(Reply::Http(200, "", EMPTY));
            }
            let server = Server::new(replies);
            let model = server.model_with_recorder(recorder);
            let result = model
                .complete_with_policy("main", "", &[], &[], fast_policy())
                .await;
            assert_eq!(result.is_ok(), status == 429);
            let records = log(root.path());
            let attempt = events(&records, "attempt_finished")[0];
            assert_eq!(attempt["status"], status);
            assert_eq!(attempt["outcome"]["kind"], "http_error");
            assert_eq!(attempt["http_error"]["body_state"], "read_failed");
            assert_eq!(attempt["http_error"]["category"], "unknown");
            assert_eq!(
                events(&records, "retry_scheduled").len(),
                usize::from(status == 429)
            );
            assert_eq!(server.count(), if status == 429 { 2 } else { 1 });
            assert_eq!(attempt.get("content").is_some(), transcript);
            if !transcript {
                assert!(
                    !serde_json::to_string(&records)
                        .unwrap()
                        .contains("PRIVATE_PREFIX")
                );
            }
        }
    }
}

#[tokio::test]
async fn stalled_error_bodies_obey_diagnostic_and_original_attempt_deadlines() {
    for attempt_timeout in [Duration::from_secs(5), Duration::from_millis(100)] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![Reply::StallHttpBody(401)]);
        let model = server.model_with_recorder(recorder);
        let result = tokio::time::timeout(
            Duration::from_millis(1500),
            model.complete_with_policy(
                "main",
                "",
                &[],
                &[],
                Policy {
                    attempt_timeout,
                    ..fast_policy()
                },
            ),
        )
        .await
        .expect("diagnostic reading exceeded its one-second budget");
        assert!(result.is_err());
        let records = log(root.path());
        let attempt = events(&records, "attempt_finished")[0];
        assert_eq!(attempt["status"], 401);
        assert_eq!(attempt["outcome"]["kind"], "http_error");
        assert_eq!(attempt["http_error"]["body_state"], "timed_out");
        assert!(attempt.get("content").is_none());
        assert_eq!(
            events(&records, "retry_stopped")[0]["reason"],
            "permanent_failure"
        );
        assert_eq!(server.count(), 1);
    }
}
