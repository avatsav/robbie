use super::*;

#[test]
fn child_statistics_update_totals_without_replacing_root_context() {
    use crate::session::{ContextUsage, UsageSnapshot};
    let mut app = App::default();
    let root = UsageSnapshot {
        context: Some(ContextUsage {
            reported_tokens: 100,
            trailing_messages: 0,
        }),
        ..UsageSnapshot::default()
    };
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::UsageUpdated(root),
    });
    let mut child = root;
    child.totals.responses = 2;
    child.context = Some(ContextUsage {
        reported_tokens: 999,
        trailing_messages: 0,
    });
    app.activity(AgentEvent {
        source: "subagent",
        kind: EventKind::UsageUpdated(child),
    });
    assert_eq!(app.statistics.totals, child.totals);
    assert_eq!(app.statistics.context, root.context);
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::UsageUpdated(UsageSnapshot {
            context: None,
            ..child
        }),
    });
    assert_eq!(app.statistics.context, None);
}

#[test]
fn statistics_are_observations_not_turn_activity_or_transcript_output() {
    for working in [false, true] {
        let mut app = App {
            working,
            failed: true,
            input: "unchanged draft".into(),
            ..App::default()
        };
        let receipt = app.activity(AgentEvent {
            source: "main",
            kind: EventKind::UsageUpdated(crate::session::UsageSnapshot::default()),
        });
        assert_eq!(app.working, working);
        assert!(app.failed);
        assert_eq!(app.input, "unchanged draft");
        assert!(receipt.assistant.is_none());
        assert!(app.take_output().is_empty());
    }
}
