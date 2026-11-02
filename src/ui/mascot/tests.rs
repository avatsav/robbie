use super::*;
use crate::ui::{App, InputAction};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use ratatui_image::picker::{Picker, ProtocolType};

fn mascot(images: bool) -> Mascot {
    let graphics = images.then(|| {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        graphics::Graphics::from_picker(&picker).unwrap()
    });
    Mascot {
        graphics,
        ..Mascot::default()
    }
}

fn finish_animation(mascot: &mut Mascot, completion: Activity) {
    let sequence = match completion {
        Activity::Success => SUCCESS,
        Activity::Error => ERROR,
        _ => panic!("not a completion scene"),
    };
    mascot.started = Instant::now() - Duration::from_millis(duration(sequence) as u64);
    mascot.advance();
}

fn sleeping_mascot(images: bool) -> Mascot {
    let mut mascot = mascot(images);
    let now = Instant::now();
    mascot.started = now - Duration::from_secs(100);
    mascot.idle = idle::Idle::new(mascot.started, 42);
    mascot.advance_at(now);
    assert_eq!(mascot.moment(now).expression, E::Sleep);
    mascot
}

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

#[test]
fn palette_changes_once_after_success_or_error_settles_for_both_renderers() {
    for images in [false, true] {
        let mut mascot = mascot(images);
        for completion in [Activity::Success, Activity::Error] {
            mascot.set_activity(Activity::Thinking);
            let original = mascot.palette_index;
            mascot.set_activity(completion);
            mascot.advance();
            assert_eq!(mascot.palette_index, original);
            assert_eq!(mascot.activity, completion);
            // Sampling the final settled pose does not itself advance color.
            let sequence = if completion == Activity::Success {
                SUCCESS
            } else {
                ERROR
            };
            let settled = moment_at(
                completion,
                Duration::from_millis(duration(sequence) as u64 - 1),
            );
            assert_eq!(settled.expression, E::Idle);
            assert_eq!(settled.hop, 0);
            assert_eq!(settled.pose, P::Neutral);
            assert_eq!(mascot.palette_index, original);
            finish_animation(&mut mascot, completion);
            assert_eq!(mascot.activity, Activity::Idle);
            assert_eq!(mascot.palette_index, (original + 1) % PALETTES.len());
            mascot.advance();
            assert_eq!(mascot.palette_index, (original + 1) % PALETTES.len());
        }
    }
}

#[test]
fn palettes_wrap_and_an_early_prompt_changes_color_only_once() {
    let mut mascot = mascot(false);
    for expected in 1..=PALETTES.len() {
        mascot.set_activity(Activity::Thinking);
        mascot.set_activity(Activity::Success);
        finish_animation(&mut mascot, Activity::Success);
        assert_eq!(mascot.palette_index, expected % PALETTES.len());
    }
    mascot.set_activity(Activity::Thinking);
    mascot.set_activity(Activity::Success);
    mascot.set_activity(Activity::Thinking);
    assert_eq!(mascot.palette_index, 1);
    mascot.set_activity(Activity::Working);
    mascot.set_activity(Activity::Thinking);
    mascot.advance();
    assert_eq!(mascot.palette_index, 1);
}

#[test]
fn ctrl_g_toggles_both_renderers_idle_and_busy_without_changing_the_draft_or_animation() {
    for images in [false, true] {
        let mut app = App {
            mascot: mascot(images),
            input: "keep this draft".into(),
            ..App::default()
        };
        for busy in [false, true] {
            if busy {
                app.submit();
                app.input = "keep this draft".into();
            }
            let started = app.mascot.started;
            let activity = app.mascot.activity;
            let palette = app.mascot.palette_index;
            let size = app.mascot.size();
            assert!(app.mascot.widget().is_some());
            assert_eq!(
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('g'),
                    KeyModifiers::CONTROL
                ))),
                InputAction::None
            );
            assert!(!app.mascot.is_visible());
            assert!(app.mascot.widget().is_none());
            assert_eq!(app.live_height(80, 24), 5);
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('g'),
                KeyModifiers::CONTROL,
            )));
            assert!(app.mascot.is_visible());
            assert!(app.mascot.widget().is_some());
            assert_eq!(app.mascot.started, started);
            assert_eq!(app.mascot.activity, activity);
            assert_eq!(app.mascot.palette_index, palette);
            assert_eq!(app.mascot.size(), size);
            assert_eq!(app.input, "keep this draft");
            assert_eq!(app.working, busy);
        }
    }
}

#[test]
fn explicit_disable_cannot_be_overridden_by_the_shortcut() {
    for images in [false, true] {
        let mut app = App {
            mascot: mascot(images),
            ..App::default()
        };
        app.mascot.disabled = true;
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        assert!(!app.mascot.is_visible());
        assert!(app.mascot.widget().is_none());
        assert_eq!(app.live_height(80, 24), 5);
    }
}

fn is_mascot(symbol: &str) -> bool {
    symbol.contains('\u{10eeee}') || matches!(symbol, "▀" | "▄")
}

#[test]
fn both_renderers_leave_bottom_rule_transcript_draft_and_cursor_intact() {
    for images in [false, true] {
        let width = 80;
        let input_width = width
            - if images {
                IMAGE_SIZE.width
            } else {
                tiles::SIZE.width
            }
            - GUTTER;
        for input in [
            "draft".into(),
            "a".repeat(input_width as usize),
            "猫e\u{301} ".repeat(100),
        ] {
            let app = App {
                mascot: mascot(images),
                input,
                ..App::default()
            };
            let height = app.live_height(width, 24);
            let area = Rect::new(2, 2, width, height);
            let mut terminal = Terminal::new(TestBackend::new(width + 4, 24)).unwrap();
            terminal.draw(|frame| app.view_live(frame, area)).unwrap();
            let bottom = area.bottom() - 3;
            let cursor = terminal.get_cursor_position().unwrap();
            let buffer = terminal.backend().buffer();
            for x in area.x..area.right() {
                assert_eq!(buffer[(x, bottom)].symbol(), "─");
            }
            assert!(cursor.x >= area.x && cursor.x < area.x + input_width);
            assert!(cursor.y >= area.y && cursor.y < bottom);
            let size = app.mascot.size();
            let mascot_area = Rect::new(
                area.right() - size.width,
                bottom - size.height,
                size.width,
                size.height,
            );
            let mut occupied = 0;
            for y in 0..24 {
                for x in 0..width + 4 {
                    if is_mascot(buffer[(x, y)].symbol()) {
                        assert!(mascot_area.contains((x, y).into()));
                        occupied += 1;
                    }
                    if y < area.y {
                        assert_eq!(buffer[(x, y)].symbol(), " ");
                    }
                }
            }
            assert!(occupied > 0);
            let rows = crate::ui::footer::draft_rows(&app.input, input_width as usize);
            let typing_rows = if images { rows.len().min(4) } else { 6 };
            let visible = &rows[rows.len().saturating_sub(typing_rows)..];
            for (offset, text) in visible.iter().enumerate() {
                let y = bottom - typing_rows as u16 + offset as u16;
                let mut expected = Buffer::empty(Rect::new(0, 0, input_width, 1));
                ratatui::text::Line::from(text.as_str()).render(expected.area, &mut expected);
                for x in 0..input_width {
                    assert_eq!(buffer[(area.x + x, y)].symbol(), expected[(x, 0)].symbol());
                }
            }
        }
    }
}

#[test]
fn short_prompts_use_six_image_panel_rows_or_ten_tile_panel_rows() {
    for images in [false, true] {
        let app = App {
            mascot: mascot(images),
            ..App::default()
        };
        assert_eq!(app.live_height(80, 24), if images { 6 } else { 10 });
    }
}

#[test]
fn small_terminals_hide_both_renderers_and_return_the_space_to_the_draft() {
    for images in [false, true] {
        let app = App {
            mascot: mascot(images),
            ..App::default()
        };
        let minimum_height = if images { 10 } else { 14 };
        for (width, height) in [(39, 24), (80, minimum_height - 1)] {
            let panel = app.live_height(width, height);
            assert_eq!(panel, 5);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| app.view_live(frame, Rect::new(0, height - panel, width, panel)))
                .unwrap();
            assert!(
                !terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|cell| is_mascot(cell.symbol()))
            );
        }
    }
}
