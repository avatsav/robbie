use super::*;
use robbie_core::{LimitSource, ModelLimits};

async fn connect(
    server: &Server,
    root: &std::path::Path,
    upstream: Option<&str>,
) -> robbie_core::Session {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    server
        .session(SessionConfig {
            workspace,
            skills_directory: "skills".into(),
            model: ModelSettings {
                api_key: "metadata-secret".into(),
                model: "fixture/model".into(),
                upstream: upstream.map(str::to_owned),
                prompt_cache: PromptCache::Off,
            },
            recording: RecordingSettings {
                state_directory: Some(root.join("state")),
                capture: CapturePolicy::Transcript,
            },
        })
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn cancelling_metadata_initialization_leaves_no_partial_cached_limits() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![Reply::Stall]);
    let mut session = connect(&server, root.path(), Some("fixture")).await;
    let mut pending = Box::pin(session.resolve_model_limits(None));
    let observed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while server.count() == 0 {
            tokio::task::yield_now().await;
        }
    });
    tokio::select! {
        result = pending.as_mut() => panic!("unexpected resolution: {result:?}"),
        result = observed => result.unwrap(),
    }
    drop(pending);
    assert!(session.model_limit_resolution().is_none());
    let configured = ModelLimits {
        context_window: 64000,
        max_input_tokens: None,
        max_output_tokens: None,
    };
    assert_eq!(
        session
            .resolve_model_limits(Some(configured))
            .await
            .unwrap()
            .limits,
        Some(configured)
    );
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn discovery_is_cached_and_never_counted_or_recorded_as_inference() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![Reply::Http(
        200,
        "",
        r#"{"data":[{"id":"fixture/model","context_length":128000,"top_provider":{"context_length":64000,"max_completion_tokens":4000}}]}"#,
    )]);
    let mut session = connect(&server, root.path(), None).await;
    let resolution = session.resolve_model_limits(None).await.unwrap();
    assert_eq!(resolution.source, LimitSource::OpenRouter);
    assert_eq!(resolution.limits.unwrap().context_window, 64000);
    assert_eq!(resolution.limits.unwrap().max_output_tokens, Some(4000));
    assert_eq!(
        session.resolve_model_limits(None).await.unwrap(),
        resolution
    );
    assert_eq!(server.count(), 1);
    assert_eq!(session.statistics().totals.responses, 0);
    let records = log(&root.path().join("state"));
    assert!(events(&records, "request_started").is_empty());
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("metadata-secret")
    );
}

#[tokio::test]
async fn pinned_unknown_routes_do_not_fall_back_to_an_unrelated_aggregate() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"data":{"context_length":1000000,"endpoints":[{"tag":"other","context_length":128000}]}}"#,
        ),
        Reply::Http(200, "", SUCCESS),
    ]);
    let mut session = connect(&server, root.path(), Some("missing")).await;
    let resolution = session.resolve_model_limits(None).await.unwrap();
    assert_eq!(resolution.source, LimitSource::Unknown);
    assert!(resolution.limits.is_none());
    assert!(resolution.warning().unwrap().contains("disabled"));
    session.resolve_model_limits(None).await.unwrap();
    assert_eq!(server.count(), 1);
    session
        .configure_auto_compaction(robbie_core::CompactionPolicy::default())
        .unwrap();
    session
        .prepare_turn()
        .run("no guessed window")
        .await
        .unwrap();
    assert_eq!(server.count(), 2);
    assert!(session.statistics().estimate.is_none());
    assert!(events(&log(&root.path().join("state")), "compaction_started").is_empty());
}

#[tokio::test]
async fn overrides_validate_without_network_and_conflicting_reconfiguration_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![]);
    let mut session = connect(&server, root.path(), None).await;
    let limits = ModelLimits {
        context_window: 100000,
        max_input_tokens: None,
        max_output_tokens: Some(8000),
    };
    assert!(
        session
            .resolve_model_limits(Some(ModelLimits {
                context_window: 0,
                ..limits
            }))
            .await
            .is_err()
    );
    assert_eq!(
        session
            .resolve_model_limits(Some(limits))
            .await
            .unwrap()
            .source,
        LimitSource::Configured
    );
    assert_eq!(
        session.resolve_model_limits(None).await.unwrap().limits,
        Some(limits)
    );
    assert!(
        session
            .resolve_model_limits(Some(ModelLimits {
                context_window: 200000,
                ..limits
            }))
            .await
            .is_err()
    );
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn unsupported_key_metadata_falls_back_to_public_model_metadata() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(404, "", "unsupported"),
        Reply::Http(
            200,
            "",
            r#"{"data":[{"id":"fixture/model","context_length":32000}]}"#,
        ),
    ]);
    let mut session = connect(&server, root.path(), None).await;
    assert_eq!(
        session
            .resolve_model_limits(None)
            .await
            .unwrap()
            .limits
            .unwrap()
            .context_window,
        32000
    );
    assert_eq!(server.count(), 2);
}
