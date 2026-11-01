//! Animated mascot: a 16×16 terminal image or a 12×12 tiled fallback.
//! Animation sampling is time-based; drawing never sleeps or writes ANSI.
use std::time::{Duration, Instant};

use self::sprite::{BodyPose as P, Expression as E, Palette};
use ratatui::{
    buffer::Buffer,
    layout::{Rect, Size},
    widgets::Widget,
};
use ratatui_image::{Image, protocol::Protocol};

mod graphics;
mod sprite;
#[cfg(test)]
mod tests;
mod tiles;

const IMAGE_SIZE: Size = Size::new(6, 3);
pub(super) const GUTTER: u16 = 2;
pub(super) const MIN_PANEL_WIDTH: u16 = 40;
pub(crate) const TICK: Duration = Duration::from_millis(50);

const PALETTES: [Palette; 6] = [
    Palette::MINT,
    Palette::BLUE,
    Palette::LILAC,
    Palette::AMBER,
    Palette::PINK,
    Palette::LIME,
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Activity {
    #[default]
    Idle,
    Thinking,
    Working,
    Success,
    Error,
}

#[derive(Clone, Copy)]
struct Moment {
    expression: E,
    hop: usize,
    pose: P,
    ms: u64,
}

const fn m(expression: E, hop: usize, pose: P, ms: u64) -> Moment {
    Moment {
        expression,
        hop,
        pose,
        ms,
    }
}

// Timings and poses from robbie-squish's demo. Completion scenes play once;
// idle, thinking, and working loop without resetting on repeated events.
const IDLE: &[Moment] = &[
    m(E::Idle, 0, P::Neutral, 600),
    m(E::LookLeft, 0, P::Neutral, 350),
    m(E::Blink, 0, P::Neutral, 100),
    m(E::Idle, 0, P::Neutral, 480),
    m(E::LookRight, 0, P::Neutral, 350),
];
const THINKING: &[Moment] = &[
    m(E::Thinking, 0, P::Neutral, 480),
    m(E::Thinking, 0, P::Squish, 170),
    m(E::Thinking, 0, P::Neutral, 530),
    m(E::Blink, 0, P::Neutral, 100),
    m(E::Thinking, 0, P::Neutral, 440),
];
const WORKING: &[Moment] = &[
    m(E::Focused, 0, P::Neutral, 440),
    m(E::Focused, 1, P::Neutral, 120),
    m(E::Focused, 0, P::Neutral, 440),
];
const SUCCESS: &[Moment] = &[
    m(E::Idle, 0, P::Neutral, 220),
    m(E::Happy, 0, P::Squish, 90),
    m(E::Happy, 2, P::Neutral, 140),
    m(E::Happy, 0, P::Squish, 100),
    m(E::Happy, 1, P::Neutral, 130),
    m(E::Happy, 0, P::Squish, 110),
    m(E::Happy, 0, P::Neutral, 360),
    m(E::Idle, 0, P::Neutral, 460),
];
const ERROR: &[Moment] = &[
    m(E::Surprised, 0, P::Neutral, 170),
    m(E::Surprised, 0, P::Squish, 120),
    m(E::Confused, 0, P::Neutral, 380),
    m(E::LookLeft, 0, P::Neutral, 350),
    m(E::Idle, 0, P::Neutral, 460),
];

pub(super) struct Mascot {
    activity: Activity,
    started: Instant,
    graphics: Option<graphics::Graphics>,
    palette_index: usize,
    hidden: bool,
    disabled: bool,
}

impl Default for Mascot {
    fn default() -> Self {
        Self {
            activity: Activity::Idle,
            started: Instant::now(),
            graphics: None,
            palette_index: 0,
            hidden: false,
            disabled: false,
        }
    }
}

impl Mascot {
    pub(super) fn set_activity(&mut self, activity: Activity) {
        if self.activity != activity {
            // Keep the old color through completion, including its settled
            // pose. An early new prompt also ends the previous completion.
            if matches!(self.activity, Activity::Success | Activity::Error) {
                self.palette_index = (self.palette_index + 1) % PALETTES.len();
            }
            self.activity = activity;
            self.started = Instant::now();
        }
    }

    pub(super) fn advance(&mut self) {
        let completion = match self.activity {
            Activity::Success => SUCCESS,
            Activity::Error => ERROR,
            _ => return,
        };
        if self.started.elapsed().as_millis() >= duration(completion) {
            self.set_activity(Activity::Idle);
        }
    }

    pub(super) fn detect_graphics(&mut self) {
        self.disabled = std::env::var("ROBBIE_MASCOT").as_deref() == Ok("off");
        self.graphics = if self.disabled {
            None
        } else {
            graphics::Graphics::detect()
        };
    }

    pub(super) fn is_visible(&self) -> bool {
        !self.disabled && !self.hidden
    }

    pub(super) fn toggle_visibility(&mut self) {
        if !self.disabled {
            self.hidden = !self.hidden;
        }
    }

    pub(super) fn size(&self) -> Size {
        if self.graphics.is_some() {
            IMAGE_SIZE
        } else {
            tiles::SIZE
        }
    }

    pub(super) fn top_rule_overlap(&self) -> u16 {
        u16::from(self.graphics.is_some())
    }

    pub(super) fn widget(&self) -> Option<impl Widget + '_> {
        if !self.is_visible() {
            return None;
        }
        let moment = moment_at(self.activity, self.started.elapsed());
        let palette = PALETTES[self.palette_index];
        Some(if let Some(graphics) = &self.graphics {
            MascotFrame::Image(graphics.frame(moment, palette)?)
        } else {
            MascotFrame::Tiles { moment, palette }
        })
    }
}

enum MascotFrame<'a> {
    Image(&'a Protocol),
    Tiles { moment: Moment, palette: Palette },
}

impl Widget for MascotFrame<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Image(image) => Image::new(image).render(area, buf),
            Self::Tiles { moment, palette } => tiles::render(moment, palette, area, buf),
        }
    }
}

fn duration(sequence: &[Moment]) -> u128 {
    sequence.iter().map(|moment| u128::from(moment.ms)).sum()
}

fn moment_at(activity: Activity, elapsed: Duration) -> Moment {
    let mut sequence = match activity {
        Activity::Idle => IDLE,
        Activity::Thinking => THINKING,
        Activity::Working => WORKING,
        Activity::Success => SUCCESS,
        Activity::Error => ERROR,
    };
    let mut ms = elapsed.as_millis();
    let total = duration(sequence);
    if matches!(activity, Activity::Success | Activity::Error) && ms >= total {
        ms -= total;
        sequence = IDLE;
    }
    ms %= duration(sequence);
    for moment in sequence {
        if ms < u128::from(moment.ms) {
            return *moment;
        }
        ms -= u128::from(moment.ms);
    }
    unreachable!("nonempty animation sequences cover their complete duration")
}

#[cfg(test)]
mod animation_tests {
    use super::sprite::{Sprite, sprite_with_pose};
    use super::*;

    fn sample(activity: Activity, elapsed: Duration) -> Sprite {
        let moment = moment_at(activity, elapsed);
        sprite_with_pose(moment.expression, moment.hop, moment.pose, Palette::MINT)
    }

    #[test]
    fn original_squish_hops_and_scene_timing_are_preserved() {
        for (activity, sequence) in [
            (Activity::Idle, IDLE),
            (Activity::Thinking, THINKING),
            (Activity::Working, WORKING),
            (Activity::Success, SUCCESS),
            (Activity::Error, ERROR),
        ] {
            let mut elapsed = Duration::ZERO;
            for moment in sequence {
                let expected =
                    sprite_with_pose(moment.expression, moment.hop, moment.pose, Palette::MINT);
                assert_eq!(sample(activity, elapsed), expected);
                elapsed += Duration::from_millis(moment.ms);
                assert_eq!(
                    sample(activity, elapsed - Duration::from_millis(1)),
                    expected
                );
            }
            if matches!(activity, Activity::Success | Activity::Error) {
                assert_eq!(
                    sample(activity, elapsed),
                    sample(Activity::Idle, Duration::ZERO)
                );
                assert_eq!(
                    sample(activity, elapsed + Duration::from_millis(700)),
                    sample(Activity::Idle, Duration::from_millis(700))
                );
            } else {
                assert_eq!(sample(activity, elapsed), sample(activity, Duration::ZERO));
            }
        }
    }

    #[test]
    fn repeated_activity_does_not_restart_the_animation() {
        let mut mascot = Mascot::default();
        mascot.set_activity(Activity::Working);
        let started = mascot.started;
        mascot.set_activity(Activity::Working);
        assert_eq!(mascot.started, started);
        mascot.set_activity(Activity::Thinking);
        assert_eq!(mascot.activity, Activity::Thinking);
    }

    #[test]
    fn agent_events_select_scenes_and_keep_parent_tools_working() {
        use crate::events::{AgentEvent, EventKind};
        use serde_json::json;

        let mut app = crate::ui::App::default();
        assert_eq!(app.mascot.activity, Activity::Idle);
        app.submit();
        assert_eq!(app.mascot.activity, Activity::Thinking);
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolStarted {
                name: "sub_agent".into(),
                input: json!({}),
            },
        });
        assert_eq!(app.mascot.activity, Activity::Working);
        let started = app.mascot.started;
        for kind in [
            EventKind::ModelStarted,
            EventKind::ToolStarted {
                name: "bash".into(),
                input: json!({}),
            },
            EventKind::ToolFinished {
                name: "bash".into(),
                output: "done".into(),
                is_error: false,
            },
        ] {
            app.activity(AgentEvent {
                source: "child",
                kind,
            });
            assert_eq!(app.mascot.activity, Activity::Working);
            assert_eq!(app.mascot.started, started);
        }
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolFinished {
                name: "sub_agent".into(),
                output: "done".into(),
                is_error: false,
            },
        });
        assert_eq!(app.mascot.activity, Activity::Thinking);
        app.finish_turn(Ok("done".into()));
        assert_eq!(app.mascot.activity, Activity::Success);
        app.submit();
        assert_eq!(app.mascot.activity, Activity::Thinking);
        app.finish_turn(Err(anyhow::anyhow!("request failed")));
        assert_eq!(app.mascot.activity, Activity::Error);
    }
}
