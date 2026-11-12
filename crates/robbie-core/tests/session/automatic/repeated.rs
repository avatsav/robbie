//! Synthetic replay of the journal's tool-output sizes, not its private content.
use super::*;
use std::collections::VecDeque;

// Batch boundaries and byte sizes from session 18e7691c0e4191f8-a8c1-0.
const FIRST_TURN: &[&[usize]] = &[
    &[1164, 2513],
    &[1769, 1118],
    &[4590, 28294],
    &[20, 39],
    &[2698],
    &[122, 2689],
    &[231],
    &[7209, 2138],
    &[92],
];
const SECOND_TURN: &[&[usize]] = &[
    &[655],
    &[7523],
    &[18668],
    &[877, 26694],
    &[20416, 20],
    &[194, 848],
    &[4311],
];

struct Observation {
    compactions: usize,
    retired: Vec<u64>,
    compaction_turns: Vec<u64>,
    ordinary_requests: usize,
    tool_results: usize,
}

async fn replay(window: u64, output_divisor: usize, summary_chars: usize) -> Observation {
    let root = tempfile::tempdir().unwrap();
    let mut script = VecDeque::from([
        json!([{"type":"text","text":"OK"}]),
        json!([{"type":"text","text":"intro ".repeat(55)}]),
    ]);
    let mut files = Vec::new();
    for (turn, batches) in [FIRST_TURN, SECOND_TURN].into_iter().enumerate() {
        for (batch, sizes) in batches.iter().enumerate() {
            let calls: Vec<_> = sizes
                .iter()
                .enumerate()
                .map(|(index, size)| {
                    let id = format!("read-{turn}-{batch}-{index}");
                    let path = format!("{id}.txt");
                    files.push((path.clone(), "x".repeat(size / output_divisor)));
                    json!({"type":"tool_use","id":id,"name":"read_file","input":{"path":path}})
                })
                .collect();
            script.push_back(json!(calls));
        }
        script.push_back(json!([{"type":"text","text":"work complete ".repeat(100)}]));
    }
    let server = Server::responding(move |request| {
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        let is_summary = body["tools"].as_array().unwrap().is_empty();
        let content = if is_summary {
            json!([{"type":"text","text":"S".repeat(summary_chars)}])
        } else {
            script.pop_front().expect("unexpected ordinary request")
        };
        // Report the same character/4 heuristic for each actual outgoing body.
        // This measures policy behavior, not real tokenizer counts or summary quality.
        let input = body.to_string().chars().count().div_ceil(4);
        let output = content.to_string().chars().count().div_ceil(4);
        let stop = if content[0]["type"] == "tool_use" {
            "tool_use"
        } else {
            "end_turn"
        };
        let reply = json!({
            "content":content,"stop_reason":stop,
            "usage":{"input_tokens":input,"output_tokens":output,
                "cache_read_input_tokens":0,"cache_creation_input_tokens":0}
        });
        Reply::Http(200, "", Box::leak(reply.to_string().into_boxed_str()))
    });
    let (mut session, mut activity) = connect(
        &server,
        root.path(),
        ModelLimits {
            context_window: window,
            max_input_tokens: None,
            max_output_tokens: Some(8000),
        },
        CompactionPolicy::default(),
    )
    .await;
    for (path, content) in files {
        std::fs::write(root.path().join("workspace").join(path), content).unwrap();
    }
    for prompt in [
        "Remember project Cedar, release Thursday, accent amber.",
        "Acknowledge the checkpoint.",
        "Run the first tool workload.",
        "Run the second tool workload.",
    ] {
        session.prepare_turn().run(prompt).await.unwrap();
    }
    while let Ok(event) = activity.try_recv() {
        assert!(!matches!(event.kind, EventKind::Warning(_)));
    }
    let records = log(&root.path().join("state"));
    let finished = events(&records, "compaction_finished");
    assert!(finished.iter().all(|event| event["outcome"] == "committed"));
    assert_eq!(events(&records, "agent_turn_finished").len(), 4);
    let tools = events(&records, "tool_execution_finished");
    assert!(tools.iter().all(|event| event["outcome"] == "succeeded"));
    let ordinary_requests = events(&records, "request_started")
        .iter()
        .filter(|event| event.get("compaction_id").is_none())
        .count();
    let observation = Observation {
        compactions: finished.len(),
        retired: finished
            .iter()
            .map(|event| event["retired_messages"].as_u64().unwrap())
            .collect(),
        compaction_turns: finished
            .iter()
            .map(|event| event["turn_id"].as_u64().unwrap())
            .collect(),
        ordinary_requests,
        tool_results: tools.len(),
    };
    assert_eq!(server.count(), ordinary_requests + observation.compactions);
    println!(
        "window={window}, output_divisor={output_divisor}, summary_chars={summary_chars}: {} compactions, turns={:?}, retired={:?}, {} ordinary requests, {} tool results",
        observation.compactions,
        observation.compaction_turns,
        observation.retired,
        ordinary_requests,
        observation.tool_results
    );
    observation
}

#[tokio::test]
async fn journal_sized_tool_batches_reproduce_repeated_successful_compaction_at_32k() {
    let observed = replay(32768, 1, 7000).await;
    assert_eq!(observed.compactions, 3);
    assert_eq!(observed.compaction_turns, [4, 4, 4]);
    assert_eq!(observed.ordinary_requests, 20);
    assert_eq!(observed.tool_results, 25, "tools must not be replayed");
}

#[tokio::test]
async fn larger_windows_avoid_repeated_compaction_for_the_same_tool_workload() {
    for window in [65536, 131072] {
        let observed = replay(window, 1, 7000).await;
        assert_eq!(observed.compactions, 0);
        assert_eq!(observed.ordinary_requests, 20);
        assert_eq!(observed.tool_results, 25);
    }
}

#[tokio::test]
async fn smaller_tool_results_avoid_compaction_without_enlarging_the_window() {
    let observed = replay(32768, 10, 7000).await;
    assert_eq!(observed.compactions, 0);
    assert_eq!(observed.ordinary_requests, 20);
    assert_eq!(observed.tool_results, 25);
}

#[tokio::test]
async fn shorter_summaries_still_recompact_under_the_same_large_tool_workload() {
    let observed = replay(32768, 1, 1000).await;
    assert_eq!(observed.compactions, 3);
    assert_eq!(observed.ordinary_requests, 20);
    assert_eq!(observed.tool_results, 25);
}
