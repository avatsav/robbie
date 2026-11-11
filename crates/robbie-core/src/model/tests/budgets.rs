use super::*;
use crate::{CompactionPolicy, ModelLimits};

#[tokio::test]
async fn policy_configuration_is_validated_once_without_inference_or_partial_changes() {
    let server = Server::new(vec![]);
    let model = server.model();
    assert!(
        model
            .configure_compaction(CompactionPolicy::default())
            .is_err()
    );
    model
        .resolve_limits(Some(ModelLimits {
            context_window: 64000,
            max_input_tokens: None,
            max_output_tokens: Some(8000),
        }))
        .await
        .unwrap();
    assert!(
        model
            .configure_compaction(CompactionPolicy {
                summary_output_tokens: 0,
                ..CompactionPolicy::default()
            })
            .is_err()
    );
    assert!(model.compaction_policy().is_none());
    model
        .configure_compaction(CompactionPolicy::default())
        .unwrap();
    model
        .configure_compaction(CompactionPolicy::default())
        .unwrap();
    assert!(
        model
            .configure_compaction(CompactionPolicy {
                reserve_tokens: 22000,
                ..CompactionPolicy::default()
            })
            .is_err()
    );
    assert_eq!(model.compaction_policy(), Some(CompactionPolicy::default()));
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn summary_input_and_output_are_checked_before_creating_a_model_request() {
    let server = Server::new(vec![]);
    let model = server.model();
    model
        .resolve_limits(Some(ModelLimits {
            context_window: 32000,
            max_input_tokens: None,
            max_output_tokens: Some(1000),
        }))
        .await
        .unwrap();
    let error = model
        .complete_summary("main", "\"".repeat(200000), 1000)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("known model budget"));
    assert!(
        model
            .complete_summary("main", "short summary input".into(), 1001)
            .await
            .is_err()
    );
    assert_eq!(server.count(), 0);
    assert_eq!(model.usage_totals().responses, 0);
}

#[tokio::test]
async fn ordinary_output_honors_advertised_limits_without_changing_stopping_policy() {
    let server = Server::new(vec![Reply::Http(200, "", SUCCESS)]);
    let model = server.model();
    model
        .resolve_limits(Some(ModelLimits {
            context_window: 64000,
            max_input_tokens: None,
            max_output_tokens: Some(100),
        }))
        .await
        .unwrap();
    model
        .configure_compaction(CompactionPolicy {
            enabled: false,
            ..CompactionPolicy::default()
        })
        .unwrap();
    let response = model
        .complete("main", "", &[Message::User("hello".into())], &[])
        .await
        .unwrap();
    assert_eq!(response.reply.text(), "done"); // Missing stop metadata remains valid for ordinary calls.
    let requests = server.requests.lock().unwrap();
    let body: Value = serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["max_tokens"], 100);
}
