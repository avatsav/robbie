//! The approved playground poses, including one-shot sleep/wake transitions.
use super::{E, IDLE, Moment, P, m};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scene {
    Original,
    DoubleBlink,
    LookAround,
    Curious,
    Wink,
    Daydream,
    Sleepy,
    Startle,
    HappyBounce,
    Content,
    Skeptical,
    Peek,
    Sleep,
    FallAsleep,
    WakeUp,
    WakeDrowsy,
}

impl Scene {
    pub(super) const ALL: [Self; 16] = [
        Self::Original,
        Self::DoubleBlink,
        Self::LookAround,
        Self::Curious,
        Self::Wink,
        Self::Daydream,
        Self::Sleepy,
        Self::Startle,
        Self::HappyBounce,
        Self::Content,
        Self::Skeptical,
        Self::Peek,
        Self::Sleep,
        Self::FallAsleep,
        Self::WakeUp,
        Self::WakeDrowsy,
    ];

    pub(super) fn sequence(self) -> &'static [Moment] {
        // Explicit const blocks give these frame arrays static storage rather
        // than returning references to temporaries constructed by m().
        match self {
            Self::Original => IDLE,
            Self::DoubleBlink => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 800),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 150),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 1600),
                    ]
                }
            }
            Self::LookAround => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 650),
                        m(E::LookLeft, 0, P::Neutral, 600),
                        m(E::Idle, 0, P::Neutral, 300),
                        m(E::LookRight, 0, P::Neutral, 650),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 1100),
                    ]
                }
            }
            Self::Curious => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 700),
                        m(E::Confused, 0, P::Neutral, 450),
                        m(E::Thinking, 0, P::Neutral, 650),
                        m(E::Confused, 0, P::Neutral, 300),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 1100),
                    ]
                }
            }
            Self::Wink => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 800),
                        m(E::Wink, 0, P::Neutral, 260),
                        m(E::Idle, 0, P::Neutral, 200),
                        m(E::Happy, 0, P::Neutral, 400),
                        m(E::Idle, 0, P::Neutral, 1200),
                    ]
                }
            }
            Self::Daydream => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 800),
                        m(E::Thinking, 0, P::Neutral, 1000),
                        m(E::LookRight, 0, P::Neutral, 700),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 1300),
                    ]
                }
            }
            Self::Sleepy => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 900),
                        m(E::Sleepy, 0, P::Neutral, 800),
                        m(E::Blink, 0, P::Squish, 650),
                        m(E::Sleepy, 0, P::Neutral, 350),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 150),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 1400),
                    ]
                }
            }
            Self::Startle => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 800),
                        m(E::Surprised, 0, P::Squish, 150),
                        m(E::Surprised, 1, P::Neutral, 150),
                        m(E::Idle, 0, P::Squish, 120),
                        m(E::Confused, 0, P::Neutral, 400),
                        m(E::Idle, 0, P::Neutral, 1300),
                    ]
                }
            }
            Self::HappyBounce => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 850),
                        m(E::Happy, 0, P::Squish, 120),
                        m(E::Happy, 2, P::Neutral, 180),
                        m(E::Happy, 0, P::Squish, 130),
                        m(E::Happy, 0, P::Neutral, 450),
                        m(E::Idle, 0, P::Neutral, 1400),
                    ]
                }
            }
            Self::Content => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 900),
                        m(E::Happy, 0, P::Neutral, 450),
                        m(E::Happy, 0, P::Squish, 200),
                        m(E::Happy, 0, P::Neutral, 500),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 1500),
                    ]
                }
            }
            Self::Skeptical => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 750),
                        m(E::Skeptical, 0, P::Neutral, 650),
                        m(E::LookLeft, 0, P::Neutral, 450),
                        m(E::Skeptical, 0, P::Neutral, 350),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 1200),
                    ]
                }
            }
            Self::Peek => {
                const {
                    &[
                        m(E::Idle, 0, P::Neutral, 800),
                        m(E::LookDown, 0, P::Neutral, 450),
                        m(E::LookDown, 0, P::Squish, 350),
                        m(E::Wink, 0, P::Squish, 220),
                        m(E::Idle, 0, P::Neutral, 400),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 1300),
                    ]
                }
            }
            Self::Sleep => {
                const {
                    &[
                        m(E::Sleep, 0, P::Sleep, 2700),
                        m(E::Sleep, 0, P::SleepBreath, 1100),
                        m(E::Sleep, 0, P::Sleep, 2200),
                    ]
                }
            }
            Self::FallAsleep => {
                const {
                    &[
                        m(E::Sleepy, 0, P::Neutral, 550),
                        m(E::Blink, 0, P::Squish, 550),
                        m(E::Sleep, 0, P::Sleep, 1200),
                    ]
                }
            }
            Self::WakeUp => {
                const {
                    &[
                        m(E::Sleep, 0, P::Sleep, 140),
                        m(E::Sleepy, 0, P::SleepBreath, 180),
                        m(E::Sleepy, 0, P::Rising, 200),
                        m(E::Blink, 0, P::Squish, 140),
                        m(E::Sleepy, 0, P::Neutral, 200),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 220),
                    ]
                }
            }
            Self::WakeDrowsy => {
                const {
                    &[
                        m(E::Sleepy, 0, P::Neutral, 200),
                        m(E::Blink, 0, P::Neutral, 100),
                        m(E::Idle, 0, P::Neutral, 220),
                    ]
                }
            }
        }
    }

    pub(super) fn is_hop(self) -> bool {
        matches!(self, Self::Startle | Self::HappyBounce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::mascot::{NEUTRAL, duration, sequence_moment};
    use std::time::Duration;

    #[test]
    fn approved_scene_durations_and_every_pose_boundary_are_preserved() {
        let durations = [
            1880, 2750, 3400, 3300, 2860, 3900, 4450, 2920, 3130, 3650, 3500, 3620, 6000, 2300,
            1180, 520,
        ];
        for (scene, expected_duration) in Scene::ALL.into_iter().zip(durations) {
            let sequence = scene.sequence();
            assert_eq!(duration(sequence), expected_duration, "{scene:?}");
            let mut elapsed = Duration::ZERO;
            for expected in sequence {
                assert!(expected.ms > 0);
                assert!(expected.hop <= 2);
                assert_eq!(sequence_moment(sequence, elapsed), *expected);
                elapsed += Duration::from_millis(expected.ms);
                assert_eq!(
                    sequence_moment(sequence, elapsed - Duration::from_millis(1)),
                    *expected
                );
            }
        }
        assert_eq!(
            Scene::WakeUp.sequence().last().unwrap().expression,
            NEUTRAL.expression
        );
        assert!(
            Scene::Sleep
                .sequence()
                .iter()
                .all(|m| m.expression == E::Sleep && m.hop == 0)
        );
    }
}
