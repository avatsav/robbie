use super::*;

#[tokio::test]
async fn compaction_lifecycle_is_metadata_only_and_does_not_invent_agent_or_frontend_turns() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", TAIL),
        Reply::Http(200, "", SUMMARY),
    ]);
    let (mut session, mut activities, _) =
        session(&server, root.path(), CapturePolicy::MetadataOnly).await;
    seed(&mut session).await;
    while activities.try_recv().is_ok() {}
    session.compact(options()).await.unwrap();
    assert!(activities.try_recv().is_err());

    let records = log(&root.path().join("state"));
    assert_eq!(events(&records, "agent_turn_started").len(), 2);
    assert!(events(&records, "ui_turn_started").is_empty());
    assert_eq!(events(&records, "compaction_started").len(), 1);
    assert_eq!(
        events(&records, "compaction_finished")[0]["outcome"],
        "committed"
    );
    assert!(records.iter().all(|record| record.get("content").is_none()));
    let serialized = serde_json::to_string(&records).unwrap();
    for forbidden in [
        "PRIVATE_OLD_PROMPT",
        "RECENT_GOAL",
        "FIRST_COMPACTION_SUMMARY",
        "fixture-secret",
        "TAIL_SIGNATURE",
        "authorization",
        "127.0.0.1",
    ] {
        assert!(!serialized.contains(forbidden));
    }
}

#[tokio::test]
async fn failed_recording_does_not_block_or_repeat_public_compaction() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", TAIL),
        Reply::Http(200, "", SUMMARY),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, _, control) =
        session(&server, root.path(), CapturePolicy::MetadataOnly).await;
    seed(&mut session).await;
    control.fail_writes();

    assert_eq!(
        session.compact(options()).await.unwrap(),
        CompactionOutcome::Compacted {
            retired_messages: 3,
            retained_messages: 1,
        }
    );
    assert_eq!(server.count(), 3);
    assert!(
        session
            .recording()
            .take_warning()
            .unwrap()
            .contains("disabled")
    );
    assert!(session.recording().take_warning().is_none());
    assert_eq!(
        session
            .prepare_turn()
            .run("AFTER_COMPACTION")
            .await
            .unwrap(),
        "done"
    );
    assert_eq!(server.count(), 4);
}
