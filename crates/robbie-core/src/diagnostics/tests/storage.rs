use super::*;

#[test]
fn explicit_relative_storage_is_rejected_before_creating_directories() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let relative_state = PathBuf::from(root.path().file_name().unwrap()).join("state");
    assert!(
        Journal::open(
            Config {
                state: relative_state,
                transcript: false,
            },
            &workspace,
        )
        .is_err()
    );
    assert!(!root.path().join("state").exists());
}

#[test]
fn separate_sessions_do_not_overwrite_each_other() {
    let root = tempfile::tempdir().unwrap();
    let mut first = recorder(root.path(), false);
    let mut second = recorder(root.path(), false);
    assert_ne!(first.path(), second.path());
    started(&mut first, "first", context);
    started(&mut second, "second", context);
    assert_eq!(records(first.path().unwrap())[0]["model"], "first");
    assert_eq!(records(second.path().unwrap())[0]["model"], "second");
}

#[test]
fn directories_and_files_are_owner_only_even_when_directories_exist() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("state/robbie/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o755)).unwrap();
    let recorder = recorder(root.path(), false);
    for path in [sessions.as_path(), sessions.parent().unwrap()] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        fs::metadata(recorder.path().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn refuses_in_workspace_storage_including_symlinked_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&workspace, &alias).unwrap();
    for state in [workspace.clone(), alias] {
        assert!(
            Journal::open(
                Config {
                    state,
                    transcript: false
                },
                &workspace
            )
            .is_err()
        );
    }
    assert!(!workspace.join("robbie").exists());
}

#[test]
fn refusing_storage_does_not_change_workspace_permissions() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let workspace = state.join("robbie");
    fs::create_dir_all(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        Journal::open(
            Config {
                state,
                transcript: false
            },
            &workspace
        )
        .is_err()
    );
    assert_eq!(
        fs::metadata(&workspace).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(!workspace.join("sessions").exists());
}

#[test]
fn refuses_symlinks_in_robbie_owned_directories_without_chmod_target() {
    for suffix in ["robbie", "robbie/sessions"] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let state = root.path().join("state");
        let link = state.join(suffix);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            Journal::open(
                Config {
                    state,
                    transcript: false
                },
                &workspace
            )
            .is_err()
        );
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
