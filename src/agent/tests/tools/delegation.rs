use super::*;

#[tokio::test]
async fn delegation_links_child_turn_to_parent_request_and_execution_even_when_child_fails() {
    for child_fails in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let mut replies = vec![Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"private-call","name":"sub_agent","input":{"prompt":"private child task"}}]}"#,
        )];
        if child_fails {
            replies.push(Reply::Http(401, "", "private provider failure"));
        } else {
            replies.push(Reply::Http(200, "", r#"{"content":[{"type":"tool_use","id":"private-call","name":"bash","input":{"command":"printf private-child-result"}}]}"#));
            replies.push(Reply::Http(200, "", SUCCESS));
        }
        replies.push(Reply::Http(200, "", SUCCESS));
        let server = Server::new(replies);
        let mut parent = agent(server.model_with_recorder(recorder));
        parent.enable_sub_agent();
        assert_eq!(parent.run("private parent task").await.unwrap(), "done");
        let records = log(root.path());
        let turns = events(&records, "agent_turn_started");
        let ends = events(&records, "agent_turn_finished");
        let tools = events(&records, "tool_execution_started");
        let tool_ends = events(&records, "tool_execution_finished");
        assert_eq!(turns.len(), 2);
        assert!(turns[0].get("parent").is_none());
        let link = &turns[1]["parent"];
        assert_eq!(
            link,
            &json!({"source":"main", "turn_id":turns[0]["turn_id"], "request_id":tools[0]["request_id"], "tool_execution_id":tools[0]["tool_execution_id"]})
        );
        assert_eq!(tools[0]["tool"], "sub_agent");
        assert_eq!(turns[1]["source"], "subagent");
        assert_eq!(ends[0]["turn_id"], turns[1]["turn_id"]);
        assert_eq!(ends[0]["parent"], *link);
        assert_eq!(
            ends[0]["reason"],
            if child_fails {
                "model_failed"
            } else {
                "no_tool_results"
            }
        );
        let delegation_end = tool_ends
            .iter()
            .find(|r| r["tool_execution_id"] == tools[0]["tool_execution_id"])
            .unwrap();
        assert_eq!(
            delegation_end["outcome"],
            if child_fails {
                "reported_error"
            } else {
                "succeeded"
            }
        );
        assert!(
            ends[0]["sequence"].as_u64().unwrap() < delegation_end["sequence"].as_u64().unwrap()
        );
        assert_eq!(ends[1]["reason"], "no_tool_results");
        assert_eq!(ends[1]["totals"]["completed_tool_results"], 1);
        let requests = events(&records, "request_started");
        assert_eq!(requests.last().unwrap()["turn_id"], turns[0]["turn_id"]);
        if !child_fails {
            assert_eq!(tools.len(), 2);
            assert_eq!(tools[1]["turn_id"], turns[1]["turn_id"]);
            assert_eq!(tools[1]["source"], "subagent");
            assert_ne!(tools[0]["tool_execution_id"], tools[1]["tool_execution_id"]);
            assert_eq!(ends[0]["totals"]["completed_tool_results"], 1);
        }
        assert!(!serde_json::to_string(&records).unwrap().contains("private"));
    }
}

#[tokio::test]
async fn invalid_delegation_reports_a_tool_error_without_starting_a_child() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"private-id","name":"sub_agent","input":{"prompt":" "}}]}"#,
        ),
        Reply::Http(200, "", SUCCESS),
    ]);
    let mut parent = agent(server.model_with_recorder(recorder));
    parent.enable_sub_agent();
    assert_eq!(parent.run("private task").await.unwrap(), "done");
    let records = log(root.path());
    assert_eq!(events(&records, "agent_turn_started").len(), 1);
    assert_eq!(
        events(&records, "tool_execution_finished")[0]["outcome"],
        "reported_error"
    );
    assert_eq!(
        events(&records, "agent_decision")[0]["decision"],
        "continue_after_tool_results"
    );
}

#[tokio::test]
async fn dropped_delegation_is_abandoned_with_unknown_outcome_not_a_fabricated_tool_result() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![
            Reply::Http(
                200,
                "",
                r#"{"content":[{"type":"tool_use","id":"private-id","name":"sub_agent","input":{"prompt":"private child task"}}]}"#,
            ),
            Reply::Stall,
        ]);
        let mut parent = agent(server.model_with_recorder(recorder));
        parent.enable_sub_agent();
        let mut run = Box::pin(parent.run("private parent task"));
        tokio::select! {
            result = run.as_mut() => panic!("unexpected completion: {result:?}"),
            ready = tokio::time::timeout(Duration::from_secs(5), async {
                while server.count() < 2 { tokio::time::sleep(Duration::from_millis(1)).await; }
            }) => ready.unwrap(),
        }
        let before = log(root.path());
        assert_eq!(events(&before, "tool_execution_started").len(), 1);
        assert_eq!(events(&before, "agent_turn_started").len(), 2);
        assert!(events(&before, "tool_execution_finished").is_empty());
        assert!(events(&before, "agent_turn_finished").is_empty());
        drop(run);
        let after = log(root.path());
        let abandoned = events(&after, "tool_execution_finished");
        assert_eq!(abandoned.len(), 1);
        assert_eq!(abandoned[0]["outcome"], "abandoned_outcome_unknown");
        assert!(abandoned[0].get("output_bytes").is_none());
        assert!(abandoned[0].get("content").is_none());
        let turns = events(&after, "agent_turn_finished");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["reason"], "cancelled");
        assert_eq!(turns[1]["reason"], "cancelled");
        assert_eq!(turns[1]["totals"]["completed_tool_results"], 0);
        assert!(events(&after, "agent_decision").is_empty());
        assert_eq!(
            events(&after, "request_finished")[1]["outcome"],
            "cancelled"
        );
        assert!(
            turns[0]["sequence"].as_u64().unwrap() < abandoned[0]["sequence"].as_u64().unwrap()
        );
        assert!(
            abandoned[0]["sequence"].as_u64().unwrap() < turns[1]["sequence"].as_u64().unwrap()
        );
        assert_eq!(server.count(), 2);
    }
}
