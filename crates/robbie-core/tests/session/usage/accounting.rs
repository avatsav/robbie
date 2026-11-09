use super::*;

#[tokio::test]
async fn partial_zero_and_missing_usage_keep_known_totals_without_inventing_a_new_baseline() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", NORMAL),
        Reply::Http(200, "", PARTIAL),
        Reply::Http(200, "", ZERO),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, mut activity, _) = connect(&server, root.path(), false).await;
    assert_eq!(session.statistics(), UsageSnapshot::default());
    for (index, prompt) in ["first", "partial", "zero", "missing"]
        .into_iter()
        .enumerate()
    {
        session.prepare_turn().run(prompt).await.unwrap();
        let snapshot = session.statistics();
        assert_eq!(snapshot.totals.responses, index as u64 + 1);
        assert_eq!(
            snapshot.context,
            Some(ContextUsage {
                reported_tokens: 160,
                trailing_messages: index * 2
            })
        );
        assert_eq!(updates(&mut activity).last().unwrap().1, snapshot);
    }
    let totals = session.statistics().totals;
    assert_eq!(
        totals.input,
        TokenTotal {
            reported: 105,
            complete: false
        }
    );
    assert_eq!(
        totals.output,
        TokenTotal {
            reported: 22,
            complete: false
        }
    );
    assert_eq!(
        totals.cache_read,
        TokenTotal {
            reported: 30,
            complete: false
        }
    );
    assert_eq!(
        totals.cache_write,
        TokenTotal {
            reported: 10,
            complete: false
        }
    );
}

#[tokio::test]
async fn retries_count_one_response_and_rejected_protocol_data_counts_usage_not_context() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(503, "", "busy"),
        Reply::Http(200, "", NORMAL),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":5}],"usage":{"input_tokens":10,"output_tokens":3,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#,
        ),
    ]);
    let (mut session, mut activity, _) = connect(&server, root.path(), false).await;
    session.prepare_turn().run("retry").await.unwrap();
    assert_eq!(
        (server.count(), session.statistics().totals.responses),
        (2, 1)
    );
    assert!(session.prepare_turn().run("invalid reply").await.is_err());
    let snapshot = session.statistics();
    assert_eq!(snapshot.totals.responses, 2);
    assert_eq!(
        snapshot.totals.input,
        TokenTotal {
            reported: 110,
            complete: true
        }
    );
    assert_eq!(
        snapshot.totals.output,
        TokenTotal {
            reported: 23,
            complete: true
        }
    );
    assert_eq!(
        snapshot.context,
        Some(ContextUsage {
            reported_tokens: 160,
            trailing_messages: 1
        })
    );
    assert_eq!(updates(&mut activity).last().unwrap().1, snapshot);
}

#[tokio::test]
async fn children_share_session_totals_but_not_the_parents_context_measurement() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"delegate","name":"sub_agent","input":{"prompt":"CHILD"}}],"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}}"#,
        ),
        Reply::Http(200, "", NORMAL),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"done"}],"usage":{"input_tokens":50,"output_tokens":5,"cache_read_input_tokens":6,"cache_creation_input_tokens":7}}"#,
        ),
    ]);
    let (mut session, mut activity, _) = connect(&server, root.path(), false).await;
    session.prepare_turn().run("delegate").await.unwrap();
    let snapshot = session.statistics();
    assert_eq!(snapshot.totals.responses, 3);
    assert_eq!(
        snapshot.totals.input,
        TokenTotal {
            reported: 160,
            complete: true
        }
    );
    assert_eq!(snapshot.totals.output.reported, 27);
    assert_eq!(snapshot.totals.cache_read.reported, 39);
    assert_eq!(snapshot.totals.cache_write.reported, 21);
    assert_eq!(
        snapshot.context,
        Some(ContextUsage {
            reported_tokens: 68,
            trailing_messages: 0
        })
    );
    let snapshots = updates(&mut activity);
    assert!(
        snapshots
            .iter()
            .any(|(source, update)| *source == "subagent"
                && update.context
                    == Some(ContextUsage {
                        reported_tokens: 160,
                        trailing_messages: 0
                    }))
    );
    assert!(snapshots.iter().any(|(source, update)| *source == "main"
        && update.totals.responses == 2
        && update.context
            == Some(ContextUsage {
                reported_tokens: 19,
                trailing_messages: 1
            })));
    assert_eq!(snapshots.last().unwrap().1, snapshot);
}

#[tokio::test]
async fn accounting_survives_disabled_or_broken_recording_and_a_closed_activity_receiver() {
    for recording in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![Reply::Http(200, "", NORMAL)]);
        let (mut session, receiver, control) = connect(&server, root.path(), recording).await;
        if recording {
            control.fail_writes();
        }
        drop(receiver);
        assert_eq!(
            session.prepare_turn().run("PRIVATE_PROMPT").await.unwrap(),
            "done"
        );
        assert_eq!(session.statistics().totals.responses, 1);
        assert_eq!(
            session.statistics().totals.input,
            TokenTotal {
                reported: 100,
                complete: true
            }
        );
        assert_eq!(session.statistics().context.unwrap().reported_tokens, 160);
        assert!(
            session
                .recording()
                .take_warning()
                .unwrap()
                .contains("disabled")
        );
        assert_eq!(server.count(), 1);
    }
}

#[tokio::test]
async fn separate_sessions_do_not_share_totals_even_with_the_same_model_name() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let server = Server::new(vec![Reply::Http(200, "", NORMAL)]);
    let (mut one, _, _) = connect(&server, first.path(), false).await;
    let (two, _, _) = connect(&server, second.path(), false).await;
    one.prepare_turn().run("first session").await.unwrap();
    assert_eq!(one.statistics().totals.responses, 1);
    assert_eq!(two.statistics(), UsageSnapshot::default());
}
