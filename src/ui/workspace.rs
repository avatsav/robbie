use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::{process::Command, sync::watch, task::JoinHandle};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Workspace {
    pub directory: String,
    pub branch: Option<String>,
}

impl Workspace {
    fn at(directory: &Path, home: Option<&Path>) -> Self {
        let directory = match home.and_then(|home| directory.strip_prefix(home).ok()) {
            Some(relative) if relative.as_os_str().is_empty() => "~".into(),
            Some(relative) => format!("~/{}", relative.display()),
            None => directory.display().to_string(),
        };
        Self {
            directory: super::clean(&directory).replace('\n', " "),
            branch: None,
        }
    }

    async fn inspect(directory: &Path, home: Option<&Path>) -> Self {
        let mut workspace = Self::at(directory, home);
        workspace.branch = git(directory, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await;
        if workspace.branch.is_none() {
            workspace.branch = git(directory, &["rev-parse", "--short", "HEAD"])
                .await
                .map(|sha| format!("detached@{sha}"));
        }
        workspace
    }
}

async fn git(directory: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(directory)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_millis(500), command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = super::clean(String::from_utf8_lossy(&output.stdout).trim());
    (!text.is_empty()).then_some(text)
}

// The footer receives snapshots; drawing never starts a process. Dropping the
// monitor aborts the refresh task and any in-flight Git query.
pub(crate) struct WorkspaceMonitor {
    receiver: watch::Receiver<Workspace>,
    task: JoinHandle<()>,
}

impl WorkspaceMonitor {
    pub(crate) fn start(directory: PathBuf) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let (sender, receiver) = watch::channel(Workspace::at(&directory, home.as_deref()));
        let task = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(Duration::from_secs(2));
            loop {
                ticks.tick().await;
                if sender.is_closed() {
                    break;
                }
                let updated = Workspace::inspect(&directory, home.as_deref()).await;
                sender.send_if_modified(|current| {
                    if *current == updated {
                        return false;
                    }
                    *current = updated;
                    true
                });
            }
        });
        Self { receiver, task }
    }

    pub(crate) fn apply_current(&self, app: &mut super::App) {
        app.workspace = self.receiver.borrow().clone();
    }

    pub(crate) async fn changed(&mut self) -> Option<Workspace> {
        self.receiver.changed().await.ok()?;
        Some(self.receiver.borrow_and_update().clone())
    }
}

impl Drop for WorkspaceMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_abbreviation_respects_path_components() {
        let home = Path::new("/home/me");
        assert_eq!(Workspace::at(home, Some(home)).directory, "~");
        assert_eq!(
            Workspace::at(Path::new("/home/me/project"), Some(home)).directory,
            "~/project"
        );
        assert_eq!(
            Workspace::at(Path::new("/home/me-too"), Some(home)).directory,
            "/home/me-too"
        );
        assert_eq!(
            Workspace::at(Path::new("/tmp/猫\nproject"), None).directory,
            "/tmp/猫 project"
        );
    }

    async fn run_git(directory: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn detects_unborn_branch_detached_head_and_non_repository() {
        let root = tempfile::tempdir().unwrap();
        assert!(Workspace::inspect(root.path(), None).await.branch.is_none());
        run_git(root.path(), &["init", "-b", "topic"]).await;
        assert_eq!(
            Workspace::inspect(root.path(), None)
                .await
                .branch
                .as_deref(),
            Some("topic")
        );
        run_git(
            root.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
        )
        .await;
        run_git(root.path(), &["checkout", "--detach", "HEAD"]).await;
        assert!(
            Workspace::inspect(root.path(), None)
                .await
                .branch
                .unwrap()
                .starts_with("detached@")
        );
    }

    #[tokio::test]
    async fn dropping_monitor_aborts_its_task() {
        let monitor = WorkspaceMonitor::start(std::env::current_dir().unwrap());
        let handle = monitor.task.abort_handle();
        drop(monitor);
        tokio::task::yield_now().await;
        assert!(handle.is_finished());
    }
}
