use super::*;

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
