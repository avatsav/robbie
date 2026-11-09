use super::*;

#[tokio::test]
async fn commits_only_the_prefix_and_normal_requests_keep_their_original_policy() {
    let server = Server::new(vec![
        Reply::Http(200, "", SUMMARY),
        Reply::Http(200, "", SUCCESS),
    ]);
    let model = server.model();
    let mut messages = history();
    let old_tail = wire(&messages[3..]);
    let bounds = limits(&messages, 2);
    let mut previous = None;

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

    assert_eq!(committed.retired_messages, 3);
    assert_eq!(committed.retained_messages, 2);
    assert!(committed.request_id > 0);
    assert_eq!(committed.response.response_id.as_deref(), Some("summary-1"));
    assert_eq!(committed.response.usage.unwrap().input_tokens, Some(900));
    assert_eq!(wire(&messages), old_tail);
    assert_eq!(
        previous.as_deref(),
        Some("FIRST_SUMMARY: preserve the goal and continue.")
    );

    // Explicitly construct the future request shape; the running Agent is not wired yet.
    let mut context = vec![summary_message(previous.as_deref().unwrap())];
    context.extend(messages.clone());
    let tools = [json!({"name":"fixture","input_schema":{"type":"object"}})];
    let ordinary = model
        .complete("main", "normal-system", &context, &tools)
        .await
        .unwrap();
    assert_eq!(ordinary.reply.text(), "done");
    let requests = bodies(&server);
    assert_eq!(requests.len(), 2);
    let summary = &requests[0];
    assert_eq!(summary["model"], "test");
    assert_eq!(summary["max_tokens"], 512);
    assert_eq!(summary["provider"]["only"], json!(["test-provider"]));
    assert_eq!(summary["tools"], json!([]));
    assert!(
        summary["system"]
            .as_str()
            .unwrap()
            .contains("historical conversation")
    );
    assert_eq!(summary["messages"].as_array().unwrap().len(), 1);
    let prompt = summary["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains("PRIVATE_GOAL"));
    assert!(prompt.contains("PRIVATE_OLD_RESULT"));
    for excluded in [
        "PRIVATE_TAIL_RESULT",
        "PRIVATE_REASONING",
        "PRIVATE_SIGNATURE",
        "PRIVATE_UNKNOWN",
    ] {
        assert!(!prompt.contains(excluded));
    }
    assert!(!summary.to_string().contains("cache_control"));
    let raw = server.requests.lock().unwrap();
    let headers = raw[0].split_once("\r\n\r\n").unwrap().0.to_lowercase();
    assert!(headers.contains("x-openrouter-cache: true"));
    drop(raw);

    let mut expected = AnthropicMessages
        .encode(
            "test",
            "normal-system",
            &context,
            &tools,
            PromptCache::Short,
        )
        .body;
    expected["provider"] = json!({"only":["test-provider"]});
    assert_eq!(requests[1], expected);
    assert_eq!(requests[1]["max_tokens"], 8000);
    assert!(
        requests[1]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("FIRST_SUMMARY")
    );
    assert_eq!(
        requests[1]["messages"][1]["content"][0]["signature"],
        "TAIL_SIGNATURE"
    );
}

#[tokio::test]
async fn transient_summary_failure_retries_the_same_request_and_commits_once() {
    let server = Server::new(vec![
        Reply::Http(429, "", "busy"),
        Reply::Http(200, "", SUMMARY),
    ]);
    let model = server.model();
    let mut messages = history();
    let old_tail = wire(&messages[3..]);
    let bounds = limits(&messages, 2);
    let mut previous = None;

    let committed = tokio::time::timeout(
        Duration::from_secs(5),
        compact_history(
            &model,
            "compaction",
            &mut messages,
            &mut previous,
            bounds,
            estimate,
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();

    assert_eq!(committed.retired_messages, 3);
    assert_eq!(wire(&messages), old_tail);
    assert_eq!(
        previous.as_deref(),
        Some("FIRST_SUMMARY: preserve the goal and continue.")
    );
    let requests = bodies(&server);
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
}
