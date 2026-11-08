use super::*;
use serde_json::Value;
use std::{env, fs};

const WARNING: &str = "Diagnostics recording failed and is disabled for this session. Agent execution is unaffected; a partial final record may remain.";

fn records(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn configured_child(root: &Path, opt_in: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new(env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::configuration::configured_start_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("XDG_STATE_HOME", root)
        .env("HOME", "relative-invalid-home")
        .env("RECORDER_TEST_SECRET", "fixture environment secret")
        .env_remove("ROBBIE_TRANSCRIPT");
    if let Some(value) = opt_in {
        command.env("ROBBIE_TRANSCRIPT", value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    output
}

#[test]
fn state_location_requires_absolute_paths_and_never_uses_workspace() {
    assert_eq!(
        state_home(Some(Path::new("/state")), Some(Path::new("/home/me"))).unwrap(),
        Path::new("/state")
    );
    for xdg in [None, Some(Path::new("")), Some(Path::new("relative"))] {
        assert_eq!(
            state_home(xdg, Some(Path::new("/home/me"))).unwrap(),
            Path::new("/home/me/.local/state")
        );
    }
    assert!(state_home(None, None).is_err());
    assert!(state_home(Some(Path::new("relative")), Some(Path::new("relative"))).is_err());
}

// Child-only fixture: changing environment in the parent would race other
// tests (and is unsafe in Rust 2024). No credentials or model calls here.
#[test]
#[ignore = "subprocess fixture for recorder configuration"]
fn configured_start_fixture() {
    run_with_runtime(async {
        let workspace = tempfile::tempdir().unwrap();
        let skills_directory = workspace.path().join("skills");
        fs::create_dir(&skills_directory)?;
        fs::write(
            skills_directory.join("SKILL.md"),
            "---\nname: fixture\ndescription: fixture sensitive system\n---\nfixture instructions",
        )?;
        let (session, mut activities) = Session::new(SessionConfig {
            workspace: workspace.path().to_owned(),
            skills_directory,
            model: ModelSettings {
                api_key: "fixture-key".into(),
                model: "fixture/model".into(),
                upstream: None,
                prompt_cache: session::PromptCache::default(),
            },
            recording: recording_settings_from_env(),
        })
        .await?;
        assert!(activities.try_recv().is_err());
        let recording = session.recording();
        if let Some(warning) = recording.take_warning() {
            println!("{warning}");
        } else {
            assert!(recording.description().is_some());
        }
        Ok(())
    })
    .unwrap();
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
