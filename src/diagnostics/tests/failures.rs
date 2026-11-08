use super::*;

#[test]
fn write_and_flush_failure_warn_once_disable_recording_and_do_not_retry() {
    for fail_flush in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut recorder = recorder(root.path(), true);
        let writes = Arc::new(Mutex::new(0));
        recorder.writer = Some(Box::new(FailingWriter {
            writes: Arc::clone(&writes),
            fail_flush,
        }));
        started(&mut recorder, "test", context);
        let warning = recorder.take_warning().unwrap();
        assert_eq!(warning, WARNING);
        assert!(!warning.contains("secret"));
        assert!(!warning.contains("/private"));
        assert!(recorder.take_warning().is_none());
        started(&mut recorder, "ignored", || {
            panic!("disabled recorder must not evaluate content")
        });
        assert_eq!(*writes.lock().unwrap(), 1);
        assert_eq!(recorder.sequence, 0);
        assert!(recorder.writer.is_none());
        assert!(recorder.take_warning().is_none());
    }
}

#[test]
fn poisoned_writer_is_disabled_and_warning_is_shared_and_consumed_once() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let other = recorder.clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _journal = other.lock();
            panic!("simulated panic while holding the journal");
        }))
        .is_err()
    );
    assert_eq!(recorder.take_warning(), Some(WARNING));
    assert!(other.take_warning().is_none());
    assert!(other.description().is_none());
    other.session_context(|| panic!("disabled recorder must not assemble content"));
    assert!(recorder.take_warning().is_none());
}

#[test]
fn unavailable_recorder_warns_once_and_ignores_content() {
    let mut recorder = Journal::disabled();
    assert!(recorder.path().is_none());
    assert_eq!(recorder.take_warning(), Some(WARNING));
    assert!(recorder.take_warning().is_none());
    started(&mut recorder, "test", || {
        panic!("disabled recorder must not evaluate content")
    });
}
