use super::*;

#[tokio::test]
async fn new_prompt_triggers_one_correlated_summary_before_the_next_ordinary_request() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![done(47000), summary(), done(100)]);
    let (mut session, _) = connect(&server, root.path(), limits(), policy()).await;
    session.prepare_turn().run(&old_prompt()).await.unwrap();
    assert_eq!(server.count(), 1);
    session.prepare_turn().run(&new_prompt()).await.unwrap();
    assert_eq!(server.count(), 3);
    assert_eq!(session.statistics().totals.responses, 3);
    assert!(session.statistics().estimate.unwrap().automatic);
    let records = log(&root.path().join("state"));
    let requests = events(&records, "request_started");
    let compact = events(&records, "compaction_started");
    assert_eq!(compact.len(), 1);
    assert_eq!(compact[0]["turn_id"], requests[1]["turn_id"]);
    assert_eq!(requests[1]["turn_id"], requests[2]["turn_id"]);
    assert_eq!(compact[0]["compaction_id"], requests[1]["compaction_id"]);
    assert_eq!(
        events(&records, "compaction_finished")[0]["outcome"],
        "committed"
    );
    let wire = bodies(&records);
    assert_eq!(wire[1]["tools"], json!([]));
    assert!(wire[1].to_string().contains("OLD_GOAL"));
    assert!(!wire[1].to_string().contains("NEW_GOAL"));
    assert!(wire[2].to_string().contains("AUTO_SUMMARY"));
    assert!(wire[2].to_string().contains("NEW_GOAL"));
    assert!(!wire[2].to_string().contains("OLD_GOAL"));
    assert_eq!(wire[0]["system"], wire[2]["system"]);
    assert_eq!(wire[0]["tools"], wire[2]["tools"]);
    assert_eq!(
        events(&records, "agent_turn_finished")[1]["totals"]["model_calls"],
        1
    );
}

#[tokio::test]
async fn complete_tool_batches_are_retained_verbatim_and_side_effects_are_not_replayed() {
    let root = tempfile::tempdir().unwrap();
    let blocks = json!([
        {"type":"thinking","thinking":"private","signature":"signed-tail"},
        {"type":"future_block","preserve":{"nested":true}},
        {"type":"tool_use","id":"read","name":"read_file","input":{"path":"large.txt"}},
        {"type":"tool_use","id":"write","name":"write_file","input":{"path":"marker.txt","content":"written once"}}
    ]);
    let server = Server::new(vec![
        response(blocks.clone(), 46000, "tool_use"),
        summary(),
        done(100),
    ]);
    let (mut session, _) = connect(&server, root.path(), limits(), policy()).await;
    assert_eq!(
        session.prepare_turn().run(&old_prompt()).await.unwrap(),
        "done"
    );
    let records = log(&root.path().join("state"));
    let tools = events(&records, "tool_execution_finished");
    let started = events(&records, "compaction_started");
    assert_eq!(tools.len(), 2);
    assert_eq!(started.len(), 1);
    assert!(
        tools
            .iter()
            .all(|tool| tool["sequence"].as_u64().unwrap()
                < started[0]["sequence"].as_u64().unwrap())
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("workspace/marker.txt")).unwrap(),
        "written once"
    );
    let wire = bodies(&records);
    assert_eq!(wire.len(), 3);
    assert_eq!(wire[2]["messages"][1]["content"], blocks);
    let results = wire[2]["messages"][2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "read");
    assert_eq!(results[1]["tool_use_id"], "write");
}

#[tokio::test]
async fn children_inherit_policy_but_only_the_childs_conversation_is_compacted() {
    let root = tempfile::tempdir().unwrap();
    let delegate = response(
        json!([{"type":"tool_use","id":"delegate","name":"sub_agent","input":{"prompt":"CHILD_GOAL ".repeat(4000)}}]),
        100,
        "tool_use",
    );
    let server = Server::new(vec![delegate, tool(46000), summary(), done(100), done(100)]);
    let (mut session, _) = connect(&server, root.path(), limits(), policy()).await;
    session
        .prepare_turn()
        .run("delegate to child")
        .await
        .unwrap();
    let records = log(&root.path().join("state"));
    let started = events(&records, "compaction_started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["source"], "subagent");
    let wire = bodies(&records);
    assert_eq!(wire.len(), 5);
    assert!(wire[3].to_string().contains("AUTO_SUMMARY"));
    assert!(!wire[4].to_string().contains("AUTO_SUMMARY"));
    assert_eq!(session.statistics().context.unwrap().reported_tokens, 110);
    assert_eq!(session.statistics().totals.responses, 5);
}
