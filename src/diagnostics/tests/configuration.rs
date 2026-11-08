use super::*;

// Child-only fixture: changing environment in the parent would race other
// tests (and is unsafe in Rust 2024). No credentials or model calls here.
#[test]
#[ignore = "subprocess fixture for recorder configuration"]
fn configured_start_fixture() {
    let recorder = Recorder::start("fixture/model", &env::current_dir().unwrap());
    recorder.session_context(|| TranscriptContext {
        system: "fixture sensitive system".into(),
        tools: vec![json!({"name": "fixture_tool"})],
    });
    if let Some(warning) = recorder.take_warning() {
        println!("{warning}");
    } else {
        assert!(recorder.description().is_some());
    }
}

#[test]
fn real_start_reads_configuration_and_only_exact_one_enables_content() {
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("1 "),
        Some("1"),
    ] {
        let root = tempfile::tempdir().unwrap();
        configured_child(root.path(), value);
        let paths: Vec<_> = fs::read_dir(root.path().join("robbie/sessions"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(paths.len(), 1);
        let lines = records(&paths[0]);
        let opt_in = value == Some("1");
        assert_eq!(lines.len(), if opt_in { 2 } else { 1 });
        assert_eq!(lines[0]["transcript_enabled"], opt_in);
        assert!(lines[0].get("content").is_none());
        if opt_in {
            assert_eq!(lines[1]["event"], "session_context");
            assert!(lines[1].get("content").is_some());
        }
        let bytes = fs::read_to_string(&paths[0]).unwrap();
        assert!(!bytes.contains("fixture environment secret"));
        assert_eq!(bytes.contains("fixture sensitive system"), opt_in);
    }
}

#[test]
fn real_start_failure_returns_normally_with_sanitized_warning() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("secret-location");
    fs::write(&blocked, "not a directory").unwrap();
    let output = configured_child(&blocked, Some("1"));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(WARNING));
    assert!(!stdout.contains("secret-location"));
    assert!(!stdout.contains("fixture sensitive system"));
    assert!(!stdout.contains("fixture environment secret"));
    assert_eq!(fs::read_to_string(&blocked).unwrap(), "not a directory");
}
