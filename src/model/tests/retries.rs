use super::*;

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
