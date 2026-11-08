use super::*;

#[test]
fn typing_paste_empty_backspace_and_navigation_wake_both_renderers_without_losing_input() {
    for images in [false, true] {
        for (event, expected_draft) in [
            (Event::Paste("hello\nworld".into()), "hello world"),
            (
                Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
                "a",
            ),
            (
                Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
                "",
            ),
            (
                Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
                "",
            ),
            (
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                "",
            ),
        ] {
            let mut app = App {
                mascot: sleeping_mascot(images),
                ..App::default()
            };
            let palette = app.mascot.palette_index;
            let size = app.mascot.size();
            assert_eq!(app.handle_event(event), InputAction::None);
            let rising = Instant::now() + Duration::from_millis(400);
            assert_eq!(app.mascot.moment(rising).pose, P::Rising);
            assert_eq!(app.mascot.palette_index, palette);
            assert_eq!(app.mascot.size(), size);
            assert_eq!(app.input, expected_draft);
            assert!(app.mascot.widget().is_some());
        }
    }
}

#[test]
fn non_draft_events_and_key_releases_do_not_wake_a_sleeping_mascot() {
    use crossterm::event::KeyEventKind;
    for event in [
        Event::Resize(80, 24),
        Event::FocusGained,
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)),
    ] {
        let mut app = App {
            mascot: sleeping_mascot(false),
            ..App::default()
        };
        app.handle_event(event);
        assert_eq!(
            app.mascot
                .moment(Instant::now() + Duration::from_millis(400))
                .expression,
            E::Sleep
        );
    }
}

#[test]
fn submit_skips_sleep_and_wake_and_drafting_never_interrupts_activity_or_completion() {
    let mut app = App {
        mascot: sleeping_mascot(false),
        ..App::default()
    };
    app.handle_event(Event::Paste("next task".into()));
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))),
        InputAction::Submit
    );
    assert_eq!(app.submit(), "next task");
    assert_eq!(app.mascot.moment(Instant::now()).expression, E::Thinking);
    for activity in [
        Activity::Thinking,
        Activity::Working,
        Activity::Success,
        Activity::Error,
    ] {
        let now = Instant::now();
        app.mascot.set_activity_at(activity, now);
        let palette = app.mascot.palette_index;
        app.mascot.interact_at(now + Duration::from_millis(400));
        assert_eq!(
            app.mascot.moment(now + Duration::from_millis(400)),
            moment_at(activity, Duration::from_millis(400))
        );
        assert_eq!(app.mascot.palette_index, palette);
    }
    // A prompt that interrupts completion advances color once, not once per
    // keystroke, and goes straight to thinking rather than waking first.
    let palette = app.mascot.palette_index;
    app.submit();
    assert_eq!(app.mascot.palette_index, (palette + 1) % PALETTES.len());
    assert_eq!(app.mascot.moment(Instant::now()).expression, E::Thinking);
}

#[test]
fn a_long_turn_gets_a_fresh_idle_rest_and_sleep_deadline_after_completion() {
    let mut mascot = mascot(false);
    let start = Instant::now();
    mascot.set_activity_at(Activity::Thinking, start);
    let finished = start + Duration::from_secs(3600);
    mascot.set_activity_at(Activity::Success, finished);
    let idle_start = finished + Duration::from_millis(duration(SUCCESS) as u64);
    mascot.advance_at(idle_start);
    assert_eq!(mascot.activity, Activity::Idle);
    assert_eq!(mascot.moment(idle_start), NEUTRAL);
    mascot.advance_at(idle_start + Duration::from_secs(60));
    assert_eq!(
        mascot
            .moment(idle_start + Duration::from_secs(61))
            .expression,
        E::Sleepy
    );
}

#[test]
fn idle_sleep_and_wake_ticks_never_change_palette_and_rendering_never_advances_state() {
    let mut mascot = mascot(false);
    let now = mascot.started;
    mascot.idle = idle::Idle::new(now, 42);
    for ms in (0..=100000).step_by(50) {
        let tick = now + Duration::from_millis(ms);
        mascot.advance_at(tick);
        let first = mascot.moment(tick);
        assert_eq!(mascot.moment(tick), first);
        assert_eq!(mascot.palette_index, 0);
    }
    mascot.interact_at(now + Duration::from_secs(100));
    for ms in (100000..=105000).step_by(50) {
        mascot.advance_at(now + Duration::from_millis(ms));
        assert_eq!(mascot.palette_index, 0);
    }
}
