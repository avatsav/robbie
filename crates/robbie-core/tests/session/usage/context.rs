use super::*;

#[tokio::test]
async fn compaction_counts_summary_usage_but_only_a_commit_invalidates_context() {
    for (candidate, committed) in [(SUMMARY, true), (REJECTED, false)] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![
            Reply::Http(200, "", NORMAL),
            Reply::Http(200, "", NORMAL),
            Reply::Http(200, "", candidate),
            Reply::Http(200, "", NORMAL),
        ]);
        let (mut session, mut activity, _) = connect(&server, root.path(), false).await;
        seed(&mut session).await;
        let before = session.statistics();
        updates(&mut activity);
        assert_eq!(session.compact(options()).await.is_ok(), committed);
        let after = session.statistics();
        assert_eq!(after.totals.responses, 3);
        assert_eq!(
            after.totals.input,
            TokenTotal {
                reported: 1100,
                complete: true
            }
        );
        assert_eq!(after.totals.output.reported, 52);
        assert_eq!(after.totals.cache_read.reported, 60);
        assert_eq!(after.totals.cache_write.reported, 20);
        assert_eq!(after.context, if committed { None } else { before.context });
        let emitted = updates(&mut activity);
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0], ("main", after));
        session.prepare_turn().run("continue").await.unwrap();
        assert_eq!(session.statistics().totals.responses, 4);
        assert_eq!(
            session.statistics().context,
            Some(ContextUsage {
                reported_tokens: 160,
                trailing_messages: 0
            })
        );
    }
}

#[tokio::test]
async fn cancelled_summary_and_no_request_operations_preserve_statistics() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", NORMAL),
        Reply::Http(200, "", NORMAL),
        Reply::Stall,
    ]);
    let (mut session, mut activity, _) = connect(&server, root.path(), false).await;
    assert!(session.compact(options()).await.is_ok());
    assert!(
        session
            .compact(CompactionOptions {
                max_output_tokens: 0,
                ..options()
            })
            .await
            .is_err()
    );
    assert_eq!(session.statistics(), UsageSnapshot::default());
    assert!(activity.try_recv().is_err());
    seed(&mut session).await;
    let before = session.statistics();
    updates(&mut activity);
    let mut pending = Box::pin(session.compact(options()));
    let received = tokio::time::timeout(Duration::from_secs(2), async {
        while server.count() < 3 {
            tokio::task::yield_now().await;
        }
    });
    tokio::select! {
        result = pending.as_mut() => panic!("unexpected compaction completion: {result:?}"),
        result = received => result.unwrap(),
    }
    drop(pending);
    assert_eq!(session.statistics(), before);
    assert!(activity.try_recv().is_err());
}

#[tokio::test]
async fn activity_exposes_statistics_during_a_borrowed_turn_and_tracks_complete_result_batches() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"read","name":"read_file","input":{"path":"fixture.txt"}}],"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#,
        ),
        Reply::Stall,
    ]);
    let (mut session, mut activity, _) = connect(&server, root.path(), false).await;
    let mut pending = Box::pin(session.prepare_turn().run("read the fixture"));
    let observation = tokio::time::timeout(Duration::from_secs(2), async {
        let snapshot = loop {
            let event = activity.recv().await.unwrap();
            if let EventKind::UsageUpdated(snapshot) = event.kind
                && snapshot.context
                    == Some(ContextUsage {
                        reported_tokens: 12,
                        trailing_messages: 1,
                    })
            {
                assert_eq!(event.source, "main");
                break snapshot;
            }
        };
        while server.count() < 2 {
            tokio::task::yield_now().await;
        }
        snapshot
    });
    let observed = tokio::select! {
        result = pending.as_mut() => panic!("unexpected turn completion: {result:?}"),
        snapshot = observation => snapshot.unwrap(),
    };
    // No immutable Session borrow was needed while execution held &mut Session.
    drop(pending);
    assert_eq!(observed.totals.responses, 1);
    assert_eq!(session.statistics(), observed);
}
