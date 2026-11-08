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

fn is_mascot(symbol: &str) -> bool {
    symbol.contains('\u{10eeee}') || matches!(symbol, "▀" | "▄")
}

mod activity;

mod palette;

mod layout;
