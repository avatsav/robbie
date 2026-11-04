//! Tool execution and real delegation evidence, without live provider calls.
use super::agent_diagnostics::agent;
use super::diagnostics::{events, log};
use super::*;
use serde_json::json;

#[tokio::test]
async fn executions_have_local_ids_typed_outcomes_and_opt_in_exact_payloads() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let response = r#"{"content":[{"type":"tool_use","id":"private-duplicate","name":"bash","input":{"command":"printf private-result"}},{"type":"tool_use","id":"private-duplicate","name":"bash","input":{"command":"exit 7"}},{"type":"tool_use","id":"private-unknown","name":"private-missing-tool","input":{"private-key":"private-value"}},{"type":"tool_use","id":"private-invalid","name":"bash","input":{}}]}"#;
        let server = Server::new(vec![
            Reply::Http(200, "", response),
            Reply::Http(200, "", SUCCESS),
        ]);
        let mut agent = agent(server.model_with_recorder(recorder));
        assert_eq!(agent.run("private task").await.unwrap(), "done");
        let records = log(root.path());
        let starts = events(&records, "tool_execution_started");
        let ends = events(&records, "tool_execution_finished");
        let requests = events(&records, "request_started");
        assert_eq!(starts.len(), 4);
        assert_eq!(ends.len(), 4);
        assert_eq!(requests.len(), 2);
        let calls: Value = serde_json::from_str(response).unwrap();
        let wire = server.requests.lock().unwrap();
        let follow_up: Value =
            serde_json::from_str(wire[1].split_once("\r\n\r\n").unwrap().1).unwrap();
        let results = follow_up["messages"][2]["content"].as_array().unwrap();
        for i in 0..4 {
            let start = starts[i];
            let end = ends[i];
            assert_eq!(start["tool_execution_id"], i + 1);
            assert_eq!(start["tool_execution_id"], end["tool_execution_id"]);
            assert_eq!(start["turn_id"], requests[0]["turn_id"]);
            assert_eq!(end["turn_id"], start["turn_id"]);
            assert_eq!(start["request_id"], requests[0]["request_id"]);
            assert_eq!(end["request_id"], start["request_id"]);
            assert_eq!(start["source"], "main");
            assert_eq!(end["tool"], start["tool"]);
            assert_eq!(start["tool"], if i == 2 { "unknown" } else { "bash" });
            assert_eq!(
                end["outcome"],
                if i == 0 {
                    "succeeded"
                } else {
                    "reported_error"
                }
            );
            assert!(end["elapsed_ms"].is_u64());
            let output = results[i]["content"].as_str().unwrap();
            assert_eq!(end["output_bytes"], output.len());
            assert!(start["sequence"].as_u64().unwrap() < end["sequence"].as_u64().unwrap());
            if i < 3 {
                assert!(
                    end["sequence"].as_u64().unwrap() < starts[i + 1]["sequence"].as_u64().unwrap()
                );
            }
            if transcript {
                assert_eq!(
                    start["content"],
                    json!({"kind":"tool_call", "tool_call_id":calls["content"][i]["id"], "name":calls["content"][i]["name"], "arguments":calls["content"][i]["input"]})
                );
                assert_eq!(
                    end["content"],
                    json!({"kind":"tool_result", "output":output})
                );
            }
        }
        if !transcript {
            assert!(records.iter().all(|r| r.get("content").is_none()));
            assert!(!serde_json::to_string(&records).unwrap().contains("private"));
        }
        // Native duplicate IDs remain unchanged in conversation; local IDs do not leak upstream.
        assert_eq!(results[0]["tool_use_id"], "private-duplicate");
        assert_eq!(results[1]["tool_use_id"], "private-duplicate");
        assert!(!wire[1].contains("tool_execution_id"));
        assert_eq!(
            events(&records, "agent_turn_finished")[0]["totals"]["completed_tool_results"],
            4
        );
    }
}

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
        parent
            .tools
            .enable_sub_agent(Arc::clone(&parent.model), parent.events.clone());
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
    parent
        .tools
        .enable_sub_agent(Arc::clone(&parent.model), parent.events.clone());
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
        parent
            .tools
            .enable_sub_agent(Arc::clone(&parent.model), parent.events.clone());
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

#[test]
fn shared_journal_allocates_unique_tool_execution_ids_across_concurrent_turns() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let workers: Vec<_> = (0..16)
        .map(|_| {
            let recorder = recorder.clone();
            std::thread::spawn(move || {
                let mut turn = recorder.agent_turn("main", None);
                let mut request =
                    recorder.model_request(turn.model_call(), "test/model", &json!({}));
                let call = ToolCall {
                    id: "private-duplicate".into(),
                    name: "bash".into(),
                    arguments: json!({}),
                };
                let tool = turn.tool_execution(request.id(), &call);
                tool.finish("private result", false);
                request.finish(crate::diagnostics::model::RequestOutcome::Succeeded);
                turn.tool_result();
                turn.finish(crate::diagnostics::agent::TurnEnd::NoToolResults {
                    returned_text_bytes: 0,
                });
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let records = log(root.path());
    let starts = events(&records, "tool_execution_started");
    let ids: std::collections::BTreeSet<_> = starts
        .iter()
        .map(|r| r["tool_execution_id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids.len(), 16);
    assert_eq!(events(&records, "tool_execution_finished").len(), 16);
    assert_eq!(
        records
            .iter()
            .map(|r| r["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        (1..=records.len() as u64).collect::<Vec<_>>()
    );
    assert!(!serde_json::to_string(&records).unwrap().contains("private"));
}

#[test]
fn tool_start_or_finish_recording_failure_warns_without_throwing_or_retrying() {
    for fail_at_start in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), true);
        let mut turn = recorder.agent_turn("main", None);
        if fail_at_start {
            recorder.break_writer_for_test();
        }
        let call = ToolCall {
            id: "private-call".into(),
            name: "bash".into(),
            arguments: json!({}),
        };
        let tool = turn.tool_execution(1, &call);
        if !fail_at_start {
            recorder.break_writer_for_test();
        }
        tool.finish("private completed output", false);
        turn.tool_result();
        turn.finish(crate::diagnostics::agent::TurnEnd::NoToolResults {
            returned_text_bytes: 0,
        });
        assert!(recorder.take_warning().unwrap().contains("disabled"));
        assert!(recorder.take_warning().is_none());
        let records = log(root.path());
        assert!(events(&records, "tool_execution_finished").is_empty());
        assert_eq!(
            events(&records, "tool_execution_started").len(),
            usize::from(!fail_at_start)
        );
    }
}
