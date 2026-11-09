use super::*;

#[tokio::test]
async fn public_compaction_changes_later_parent_requests_but_children_stay_fresh() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", TAIL),
        Reply::Http(200, "", SUMMARY),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"delegate","name":"sub_agent","input":{"prompt":"CHILD_ONLY"}}]}"#,
        ),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"CHILD_OK"}]}"#,
        ),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"ROOT_DONE"}]}"#,
        ),
    ]);
    let (mut session, mut activity, _) =
        session(&server, root.path(), CapturePolicy::Transcript).await;
    seed(&mut session).await;
    while activity.try_recv().is_ok() {}
    assert_eq!(server.count(), 2, "nothing compacts automatically");

    assert_eq!(
        session.compact(options()).await.unwrap(),
        CompactionOutcome::Compacted {
            retired_messages: 3,
            retained_messages: 1,
        }
    );
    assert_eq!(server.count(), 3);
    assert!(
        activity.try_recv().is_err(),
        "a summary is not user-facing assistant text"
    );
    assert_eq!(
        session.prepare_turn().run("DELEGATE_NEXT").await.unwrap(),
        "ROOT_DONE"
    );

    let requests = request_bodies(root.path());
    assert_eq!(requests.len(), 6);
    let summary = &requests[2];
    assert_eq!(summary["max_tokens"], 512);
    assert_eq!(summary["tools"], json!([]));
    assert_eq!(summary["provider"]["only"], json!(["fixture-upstream"]));
    assert!(
        summary["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("PRIVATE_OLD_PROMPT")
    );
    let expected_tail: Value = serde_json::from_str(TAIL).unwrap();
    for index in [3, 5] {
        let parent = &requests[index];
        assert_eq!(parent["system"], requests[0]["system"]);
        assert_eq!(parent["tools"], requests[0]["tools"]);
        assert_eq!(parent["max_tokens"], 8000);
        assert_eq!(parent["messages"][1]["content"], expected_tail["content"]);
        assert_eq!(
            parent
                .to_string()
                .matches("FIRST_COMPACTION_SUMMARY")
                .count(),
            1
        );
        assert!(!parent.to_string().contains("PRIVATE_OLD_PROMPT"));
    }
    let child = &requests[4];
    assert_eq!(child["messages"].as_array().unwrap().len(), 1);
    assert_eq!(child["messages"][0]["content"], "CHILD_ONLY");
    assert!(!child.to_string().contains("FIRST_COMPACTION_SUMMARY"));
    assert!(
        child["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["name"] != "sub_agent")
    );
    assert_eq!(
        requests[5]["messages"][4]["content"][0]["content"],
        "CHILD_OK"
    );

    let records = log(&root.path().join("state"));
    let starts = events(&records, "compaction_started");
    let finishes = events(&records, "compaction_finished");
    assert_eq!((starts.len(), finishes.len()), (1, 1));
    assert_eq!(starts[0]["raw_messages"], 4);
    assert_eq!(starts[0]["had_previous_summary"], false);
    assert_eq!(finishes[0]["outcome"], "committed");
    assert_eq!(starts[0]["compaction_id"], finishes[0]["compaction_id"]);
    let model = events(&records, "request_started");
    assert_eq!(model[2]["compaction_id"], starts[0]["compaction_id"]);
    assert_eq!(model[2]["request_id"], finishes[0]["request_id"]);
    assert!(
        model[2].get("turn_id").is_none(),
        "manual compaction is not an agent turn"
    );
    assert!(model[3].get("compaction_id").is_none());
    assert!(starts[0]["sequence"].as_u64().unwrap() < model[2]["sequence"].as_u64().unwrap());
    assert!(model[2]["sequence"].as_u64().unwrap() < finishes[0]["sequence"].as_u64().unwrap());
    assert!(starts[0].get("content").is_none());
    assert!(finishes[0].get("content").is_none());
}

#[tokio::test]
async fn repeated_session_compaction_replaces_the_previous_summary_exactly_once() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", TAIL),
        Reply::Http(200, "", SUMMARY),
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", SECOND),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, _, _) = session(&server, root.path(), CapturePolicy::Transcript).await;
    seed(&mut session).await;
    session.compact(options()).await.unwrap();
    session
        .prepare_turn()
        .run(&"PRIVATE_AFTER_FIRST ".repeat(160))
        .await
        .unwrap();
    assert_eq!(
        session.compact(options()).await.unwrap(),
        CompactionOutcome::Compacted {
            retired_messages: 2,
            retained_messages: 1,
        }
    );
    session.prepare_turn().run("FINAL_USER").await.unwrap();

    let requests = request_bodies(root.path());
    let second_prompt = requests[4]["messages"][0]["content"].as_str().unwrap();
    assert_eq!(second_prompt.matches("FIRST_COMPACTION_SUMMARY").count(), 1);
    assert!(second_prompt.contains("PRIVATE_AFTER_FIRST"));
    assert!(!second_prompt.contains("PRIVATE_OLD_PROMPT"));
    let final_request = requests[5].to_string();
    assert_eq!(
        final_request.matches("SECOND_COMPACTION_SUMMARY").count(),
        1
    );
    assert!(!final_request.contains("FIRST_COMPACTION_SUMMARY"));
    assert!(!final_request.contains("PRIVATE_AFTER_FIRST"));
    let records = log(&root.path().join("state"));
    let starts = events(&records, "compaction_started");
    assert_ne!(starts[0]["compaction_id"], starts[1]["compaction_id"]);
    assert_eq!(starts[1]["had_previous_summary"], true);
    assert_eq!(
        events(&records, "compaction_finished")
            .iter()
            .filter(|end| end["outcome"] == "committed")
            .count(),
        2
    );
}
