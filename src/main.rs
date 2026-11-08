use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

mod agent;
mod diagnostics;
mod events;
mod fullscreen;
mod markdown;
mod model;
mod session;
mod skills;
mod terminal_text;
#[cfg(test)]
mod test_support;
mod tools;
mod ui;
#[cfg(test)]
use agent::Agent;
use session::{CapturePolicy, ModelSettings, RecordingSettings, Session, SessionConfig};
#[cfg(test)]
use tools::ToolRegistry;

fn validate_args(args: &[String]) -> Result<()> {
    if let Some(arg) = args.first() {
        bail!(
            "Unexpected argument {arg:?}. Run Robbie without arguments to start the full-screen UI."
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    validate_args(&std::env::args().skip(1).collect::<Vec<_>>())?;
    run_with_runtime(run())
}

fn run_with_runtime(future: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(future);
    // Running blocking tasks cannot be aborted. Bound shutdown rather than
    // trapping the terminal session forever; process exit ends remaining threads.
    runtime.shutdown_timeout(std::time::Duration::from_millis(100));
    result
}

async fn run() -> Result<()> {
    dotenvy::dotenv().ok();
    let prompt_cache =
        session::PromptCache::from_setting(std::env::var_os("ROBBIE_PROMPT_CACHE").as_deref())?;
    let workspace = std::env::current_dir()?;
    let api_key = std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is not set")?;
    let model_name = std::env::var("OPENROUTER_MODEL").context("OPENROUTER_MODEL is not set")?;
    let upstream = std::env::var("OPENROUTER_PROVIDER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let (session, receiver) = Session::new(SessionConfig {
        workspace: workspace.clone(),
        skills_directory: workspace.join("skills"),
        model: ModelSettings {
            api_key,
            model: model_name,
            upstream,
            prompt_cache,
        },
        recording: recording_settings_from_env(),
    })
    .await?;
    fullscreen::run(session, receiver, workspace).await
}

fn recording_settings_from_env() -> RecordingSettings {
    RecordingSettings {
        state_directory: state_home(
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .as_deref(),
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        )
        .ok(),
        // Only this exact value opts in; typos must not enable content.
        capture: if std::env::var_os("ROBBIE_TRANSCRIPT").is_some_and(|value| value == "1") {
            CapturePolicy::Transcript
        } else {
            CapturePolicy::MetadataOnly
        },
    }
}

fn state_home(xdg: Option<&Path>, home: Option<&Path>) -> std::io::Result<PathBuf> {
    if let Some(path) = xdg.filter(|path| path.is_absolute()) {
        return Ok(path.to_owned());
    }
    home.filter(|path| path.is_absolute())
        .map(|path| path.join(".local").join("state"))
        .ok_or_else(|| std::io::Error::other("no absolute state directory available"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    mod configuration;

    #[test]
    fn fullscreen_is_the_only_frontend_and_obsolete_flags_are_rejected() {
        assert!(validate_args(&[]).is_ok());
        for arg in ["--inline", "--console", "--fullscreen", "--unknown"] {
            let error = validate_args(&[arg.to_string()]).unwrap_err().to_string();
            assert!(error.contains(arg));
            assert!(error.contains("without arguments"));
        }
    }

    #[test]
    fn shutdown_does_not_wait_forever_for_blocking_work() {
        use std::time::{Duration, Instant};
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::blocked_shutdown_fixture",
                "--ignored",
                "--nocapture",
            ])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("process shutdown waited indefinitely for blocking work");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // Runs only in a child process: its deliberately stuck worker cannot be
    // cleaned up by the test runtime. The parent enforces a hard deadline.
    #[test]
    #[ignore = "subprocess fixture for shutdown regression"]
    fn blocked_shutdown_fixture() {
        run_with_runtime(async {
            let (started, ready) = tokio::sync::oneshot::channel();
            tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                loop {
                    std::thread::park();
                }
            });
            ready.await?;
            Ok(())
        })
        .unwrap();
    }
}
