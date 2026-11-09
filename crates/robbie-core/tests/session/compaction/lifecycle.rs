use super::*;

#[tokio::test]
async fn rejected_candidate_preserves_the_previous_summary_and_all_raw_history() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", TAIL),
        Reply::Http(200, "", SUMMARY),
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", BAD_SUMMARY),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, _, _) = session(&server, root.path(), CapturePolicy::Transcript).await;
    seed(&mut session).await;
    session.compact(options()).await.unwrap();
    session
        .prepare_turn()
        .run(&"PRESERVE_AFTER_FAILED_COMPACTION ".repeat(160))
        .await
        .unwrap();

    assert!(session.compact(options()).await.is_err());
    assert_eq!(
        session.prepare_turn().run("RECOVER_USER").await.unwrap(),
        "done"
    );

    let requests = request_bodies(root.path());
    assert_eq!(requests.len(), 6);
    let recovered = requests[5].to_string();
    assert_eq!(recovered.matches("FIRST_COMPACTION_SUMMARY").count(), 1);
    assert!(recovered.contains("PRESERVE_AFTER_FAILED_COMPACTION"));
    assert!(recovered.contains("RECOVER_USER"));
    assert_eq!(requests[5]["messages"].as_array().unwrap().len(), 5);
    let records = log(&root.path().join("state"));
    let end = events(&records, "compaction_finished");
    assert_eq!(end[0]["outcome"], "committed");
    assert_eq!(end[1]["outcome"], "failed");
    // Valid HTTP/JSON is not evidence that the candidate was accepted.
    let completed_requests = events(&records, "request_finished");
    assert_eq!(completed_requests[4]["outcome"], "succeeded");
}

#[tokio::test]
async fn dropping_pending_compaction_keeps_the_session_usable_and_records_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", TAIL),
        Reply::Http(429, "Retry-After: 1\r\n", "busy"),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, _, _) = session(&server, root.path(), CapturePolicy::Transcript).await;
    seed(&mut session).await;
    let mut pending = Box::pin(session.compact(options()));
    let received = tokio::time::timeout(Duration::from_secs(2), async {
        while server.count() < 3 {
            tokio::task::yield_now().await;
        }
    });
    tokio::select! {
        result = pending.as_mut() => panic!("compaction unexpectedly completed: {result:?}"),
        result = received => result.unwrap(),
    }
    drop(pending);

    assert_eq!(
        session.prepare_turn().run("AFTER_CANCEL").await.unwrap(),
        "done"
    );
    let requests = request_bodies(root.path());
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[3]["messages"].as_array().unwrap().len(), 5);
    assert!(requests[3].to_string().contains("PRIVATE_OLD_PROMPT"));
    assert!(requests[3].to_string().contains("AFTER_CANCEL"));
    assert!(!requests[3].to_string().contains("FIRST_COMPACTION_SUMMARY"));
    let records = log(&root.path().join("state"));
    let end = events(&records, "compaction_finished");
    assert_eq!(end.len(), 1);
    assert_eq!(end[0]["outcome"], "cancelled");
    assert_eq!(
        events(&records, "request_finished")[2]["outcome"],
        "cancelled"
    );
}

#[tokio::test]
async fn no_change_and_invalid_options_have_distinct_outcomes_without_requests() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![]);
    let (mut session, mut activities, _) =
        session(&server, root.path(), CapturePolicy::MetadataOnly).await;
    assert_eq!(
        session.compact(options()).await.unwrap(),
        CompactionOutcome::Unchanged
    );
    for invalid in [
        CompactionOptions {
            max_input_chars: 0,
            ..options()
        },
        CompactionOptions {
            max_output_tokens: 0,
            ..options()
        },
    ] {
        assert!(session.compact(invalid).await.is_err());
    }
    assert_eq!(server.count(), 0);
    assert!(activities.try_recv().is_err());
    let records = log(&root.path().join("state"));
    let started = events(&records, "compaction_started");
    assert_eq!(
        started
            .iter()
            .map(|event| event["compaction_id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let finished = events(&records, "compaction_finished");
    assert_eq!(
        finished
            .iter()
            .map(|event| event["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["unchanged", "failed", "failed"]
    );
    assert!(events(&records, "request_started").is_empty());
}
