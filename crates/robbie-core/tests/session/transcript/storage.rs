use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn dumps_are_private_unique_workspace_files_even_without_recording() {
    for recording in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![]);
        let session = session(&server, root.path(), recording).await;
        let workspace = root.path().join("workspace");
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o755)).unwrap();
        let first = session.dump_transcript().unwrap();
        let first_bytes = fs::read(&first).unwrap();
        let second = session.dump_transcript().unwrap();
        let directory = fs::canonicalize(workspace.join(".robbie/transcripts")).unwrap();
        assert_eq!(first.parent().unwrap(), directory);
        assert_eq!(second.parent().unwrap(), directory);
        assert_ne!(first, second);
        assert_eq!(
            fs::read(&first).unwrap(),
            first_bytes,
            "never overwrite an earlier dump"
        );
        assert_eq!(mode(&first), 0o600);
        assert_eq!(mode(&second), 0o600);
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(&workspace.join(".robbie")), 0o700);
        assert_eq!(mode(&workspace), 0o755);
        assert!(!root.path().join("state/robbie/transcripts").exists());
        assert_eq!(server.count(), 0);
        if recording {
            let records = log(&root.path().join("state"));
            assert!(events(&records, "request_started").is_empty());
            assert!(records.iter().all(|record| record.get("content").is_none()));
        } else {
            assert!(!root.path().join("state").exists());
        }
    }
}

#[tokio::test]
async fn existing_workspace_directory_permissions_are_never_changed() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![]);
    let session = session(&server, root.path(), false).await;
    let workspace = root.path().join("workspace");
    let application = workspace.join(".robbie");
    let transcripts = application.join("transcripts");
    fs::create_dir_all(&transcripts).unwrap();
    for path in [&workspace, &application, &transcripts] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let file = session.dump_transcript().unwrap();
    for path in [&workspace, &application, &transcripts] {
        assert_eq!(mode(path), 0o755, "{}", path.display());
    }
    assert_eq!(mode(&file), 0o600);
}

#[tokio::test]
async fn symlinked_dump_directories_are_refused_without_writes_or_chmod() {
    for component in [".robbie", ".robbie/transcripts"] {
        for outside in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let server = Server::new(vec![]);
            let session = session(&server, root.path(), false).await;
            let workspace = root.path().join("workspace");
            let target = if outside {
                root.path().join("outside")
            } else {
                workspace.join("other")
            };
            fs::create_dir(&target).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
            let link = workspace.join(component);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(&target, &link).unwrap();
            assert!(session.dump_transcript().is_err(), "{component}");
            assert_eq!(mode(&target), 0o755);
            assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        }
    }
}

#[tokio::test]
async fn files_in_place_of_directories_are_refused_without_overwriting() {
    for component in [".robbie", ".robbie/transcripts"] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![]);
        let session = session(&server, root.path(), false).await;
        let path = root.path().join("workspace").join(component);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "KEEP").unwrap();
        assert!(session.dump_transcript().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "KEEP");
    }
}

#[tokio::test]
async fn a_workspace_at_the_old_export_destination_is_not_chmodded() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let workspace = state.join("robbie/transcripts");
    fs::create_dir_all(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o755)).unwrap();
    let mut settings = config(root.path(), true);
    settings.workspace = workspace.clone();
    settings.skills_directory = workspace.join("skills");
    let (session, _) = Session::new(settings).await.unwrap();
    let path = session.dump_transcript().unwrap();
    assert_eq!(
        path.parent().unwrap(),
        fs::canonicalize(workspace.join(".robbie/transcripts")).unwrap()
    );
    assert_eq!(mode(&workspace), 0o755);
}
