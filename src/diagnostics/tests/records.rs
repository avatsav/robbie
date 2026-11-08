use super::*;

#[test]
fn metadata_only_never_evaluates_or_serializes_content() {
    let root = tempfile::tempdir().unwrap();
    let mut recorder = recorder(root.path(), false);
    started(&mut recorder, "test/model", || {
        panic!("content must not be assembled")
    });
    let records = records(recorder.path().unwrap());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "session_started");
    assert_eq!(records[0]["transcript_enabled"], false);
    assert!(records[0].get("content").is_none());
    let mut keys: Vec<_> = records[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "event",
            "model",
            "sequence",
            "session_id",
            "timestamp_ms",
            "transcript_enabled",
            "version"
        ]
    );
}

#[test]
fn opt_in_records_content_as_one_line_without_modifying_it() {
    let root = tempfile::tempdir().unwrap();
    let mut recorder = recorder(root.path(), true);
    started(&mut recorder, "test/model", context);
    let records = records(recorder.path().unwrap());
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0]["content"]["system"],
        "sensitive system\nsecond line"
    );
    assert_eq!(records[0]["content"]["tools"][0]["name"], "bash");
    assert_eq!(records[0]["transcript_enabled"], true);
}

#[test]
fn appends_are_visible_while_open_and_have_stable_identity_and_sequence() {
    let root = tempfile::tempdir().unwrap();
    let mut recorder = recorder(root.path(), false);
    let path = recorder.path().unwrap().to_owned();
    started(&mut recorder, "first", context);
    assert_eq!(records(&path).len(), 1);
    let prefix = fs::read(&path).unwrap();
    started(&mut recorder, "second", context);
    assert!(fs::read(&path).unwrap().starts_with(&prefix));
    let records = records(&path);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["sequence"], 1);
    assert_eq!(records[1]["sequence"], 2);
    assert_eq!(records[0]["session_id"], records[1]["session_id"]);
    assert!(records[0]["timestamp_ms"].as_u64().unwrap() > 0);
    assert_eq!(records[0]["version"], 1);
    assert!(fs::read(&path).unwrap().ends_with(b"\n"));
}

#[test]
fn clones_share_one_sequence_and_unique_request_ids_under_concurrent_writes() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let recorder = recorder.clone();
            std::thread::spawn(move || {
                for _ in 0..8 {
                    let mut request = recorder.model_request("test", "test/model", &Value::Null);
                    request.finish(model::RequestOutcome::Succeeded);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let records = records(recorder.lock().path().unwrap());
    assert_eq!(records.len(), 64);
    let mut ids = std::collections::HashSet::new();
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record["sequence"], index + 1);
        assert_eq!(record["session_id"], records[0]["session_id"]);
        if record["event"] == "request_started" {
            assert!(ids.insert(record["request_id"].as_u64().unwrap()));
        }
    }
    assert_eq!(ids.len(), 32);
}

#[test]
fn opt_in_preserves_non_utf8_response_bytes_without_loss() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), true);
    let mut request = recorder.model_request("main", "test/model", &Value::Null);
    let mut attempt = request.attempt(1);
    attempt.status(200);
    attempt.finish(model::AttemptOutcome::InvalidJson, Some(&[255, 13, 10]));
    request.finish(model::RequestOutcome::ExchangeFailed);
    let records = records(recorder.lock().path().unwrap());
    assert_eq!(records[2]["content"]["body"], json!([255, 13, 10]));
}
