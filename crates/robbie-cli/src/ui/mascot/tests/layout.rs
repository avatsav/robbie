use super::*;

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
