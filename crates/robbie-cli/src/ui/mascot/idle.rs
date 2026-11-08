//! Quiet-company scheduling. Callers supply monotonic time and a seed; drawing
//! only samples state. No timers, terminal I/O, or randomness in the draw path.
use std::time::{Duration, Instant};

use super::{Moment, NEUTRAL, P, duration, scenes::Scene, sequence_moment};

const QUIET: Duration = Duration::from_secs(3);
const SLEEPY_AFTER: Duration = Duration::from_secs(60);
const SLEEP_AFTER: Duration = Duration::from_secs(90);
const HOP_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mood {
    Awake,
    Sleepy,
    Asleep,
}

#[derive(Clone, Copy)]
enum Group {
    Everyday,
    Playful,
    Calm,
}

impl Group {
    fn scenes(self) -> &'static [Scene] {
        use Scene::*;
        match self {
            Self::Everyday => &[
                Original,
                DoubleBlink,
                LookAround,
                Curious,
                Daydream,
                Content,
            ],
            Self::Playful => &[Wink, Skeptical, Peek, HappyBounce, Startle],
            Self::Calm => &[Original, DoubleBlink, LookAround],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Rest(Instant),
    Quiet(Instant),
    Playing { scene: Scene, started: Instant },
}

impl Phase {
    fn deadline(self) -> Option<Instant> {
        match self {
            Self::Rest(until) | Self::Quiet(until) => Some(until),
            Self::Playing {
                scene: Scene::Sleep,
                ..
            } => None,
            Self::Playing { scene, started } => Some(started + scene_duration(scene)),
        }
    }
}

fn scene_duration(scene: Scene) -> Duration {
    Duration::from_millis(duration(scene.sequence()) as u64)
}

pub(super) struct Idle {
    phase: Phase,
    inactive_since: Instant,
    mood: Mood,
    last: Option<Scene>,
    last_hop: Option<Instant>,
    bags: [Vec<Scene>; 3],
    // Cosmetic randomness only: the same LCG as the approved playground.
    random: u32,
}

impl Idle {
    pub(super) fn new(now: Instant, seed: u32) -> Self {
        let mut idle = Self {
            phase: Phase::Rest(now),
            inactive_since: now,
            mood: Mood::Awake,
            last: None,
            last_hop: None,
            bags: std::array::from_fn(|_| Vec::new()),
            random: seed,
        };
        idle.rest(now);
        idle
    }

    // A long task must not make Robbie sleepy as soon as it finishes. Preserve
    // repertoire/cooldown history, but begin a fresh idle episode after settling.
    pub(super) fn reset(&mut self, now: Instant) {
        self.mood = Mood::Awake;
        self.inactive_since = now;
        self.rest(now);
    }

    pub(super) fn interact(&mut self, now: Instant) {
        // An input event can arrive before the next animation tick.
        self.advance(now);
        self.inactive_since = now;
        if matches!(
            self.phase,
            Phase::Playing {
                scene: Scene::WakeUp | Scene::WakeDrowsy,
                ..
            }
        ) {
            return; // More typing extends quiet time, never restarts the wake.
        }
        if self.mood != Mood::Awake {
            let lying_down = matches!(self.moment(now).pose, P::Sleep | P::SleepBreath);
            let scene = if lying_down {
                Scene::WakeUp
            } else {
                Scene::WakeDrowsy
            };
            self.mood = Mood::Awake;
            self.phase = Phase::Playing {
                scene,
                started: now,
            };
        } else {
            self.phase = Phase::Quiet(now + QUIET);
        }
    }

    pub(super) fn advance(&mut self, now: Instant) {
        loop {
            let mood_at = self.mood_deadline();
            let next = mood_at.into_iter().chain(self.phase.deadline()).min();
            let Some(deadline) = next.filter(|deadline| *deadline <= now) else {
                break;
            };
            if mood_at == Some(deadline) {
                match self.mood {
                    Mood::Awake => {
                        self.mood = Mood::Sleepy;
                        self.last = Some(Scene::Sleepy);
                        self.phase = Phase::Playing {
                            scene: Scene::Sleepy,
                            started: deadline,
                        };
                    }
                    Mood::Sleepy => {
                        self.mood = Mood::Asleep;
                        self.phase = Phase::Playing {
                            scene: Scene::FallAsleep,
                            started: deadline,
                        };
                    }
                    Mood::Asleep => unreachable!("asleep has no mood deadline"),
                }
                continue;
            }
            match self.phase {
                Phase::Playing {
                    scene: Scene::FallAsleep,
                    ..
                } => {
                    self.phase = Phase::Playing {
                        scene: Scene::Sleep,
                        started: deadline,
                    };
                }
                Phase::Playing {
                    scene: Scene::WakeUp | Scene::WakeDrowsy,
                    ..
                } => {
                    self.phase = Phase::Quiet((self.inactive_since + QUIET).max(deadline));
                }
                Phase::Playing { .. } => self.rest(deadline),
                Phase::Rest(_) | Phase::Quiet(_) => {
                    let scene = self.choose(deadline);
                    self.last = Some(scene);
                    if scene.is_hop() {
                        self.last_hop = Some(deadline);
                    }
                    self.phase = Phase::Playing {
                        scene,
                        started: deadline,
                    };
                }
            }
        }
    }

    pub(super) fn moment(&self, now: Instant) -> Moment {
        if let Phase::Playing { scene, started } = self.phase {
            let elapsed = now.saturating_duration_since(started);
            if scene == Scene::Sleep || elapsed < scene_duration(scene) {
                return sequence_moment(scene.sequence(), elapsed);
            }
            // A delayed tick must not briefly stand up a bot that just tucked in.
            if scene == Scene::FallAsleep {
                return Scene::Sleep.sequence()[0];
            }
        }
        NEUTRAL
    }

    fn mood_deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Quiet(_)
            | Phase::Playing {
                scene: Scene::Sleep | Scene::FallAsleep | Scene::WakeUp | Scene::WakeDrowsy,
                ..
            } => return None,
            _ => {}
        }
        match self.mood {
            Mood::Awake => Some(self.inactive_since + SLEEPY_AFTER),
            Mood::Sleepy => Some(self.inactive_since + SLEEP_AFTER),
            Mood::Asleep => None,
        }
    }

    fn random_below(&mut self, limit: u32) -> u32 {
        self.random = self.random.wrapping_mul(1664525).wrapping_add(1013904223);
        // Multiply-high avoids the LCG's weak low bits for small ranges.
        ((u64::from(self.random) * u64::from(limit)) >> 32) as u32
    }

    fn rest(&mut self, now: Instant) {
        let (min, spread) = if self.mood == Mood::Sleepy {
            (15000, 10001)
        } else {
            (6000, 6001)
        };
        let ms = min + self.random_below(spread);
        self.phase = Phase::Rest(now + Duration::from_millis(u64::from(ms)));
    }

    fn choose(&mut self, now: Instant) -> Scene {
        if self.mood == Mood::Sleepy {
            if self.random_below(10) < 6 && self.last != Some(Scene::Sleepy) {
                return Scene::Sleepy;
            }
            return self.take(Group::Calm, now);
        }
        let group = if self.random_below(10) < 8 {
            Group::Everyday
        } else {
            Group::Playful
        };
        self.take(group, now)
    }

    fn take(&mut self, group: Group, now: Instant) -> Scene {
        let eligible: Vec<_> = group
            .scenes()
            .iter()
            .copied()
            .filter(|scene| {
                self.last != Some(*scene)
                    && (!scene.is_hop()
                        || self
                            .last_hop
                            .is_none_or(|last| now.saturating_duration_since(last) >= HOP_COOLDOWN))
            })
            .collect();
        let mut bag = std::mem::take(&mut self.bags[group as usize]);
        bag.retain(|scene| eligible.contains(scene));
        if bag.is_empty() {
            bag = eligible;
            for i in (1..bag.len()).rev() {
                let j = self.random_below((i + 1) as u32) as usize;
                bag.swap(i, j);
            }
        }
        // Each group has at least three non-hopping scenes, so excluding the
        // previous scene and cooling hops cannot empty the eligible pool.
        let scene = bag.remove(0);
        self.bags[group as usize] = bag;
        scene
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start() -> (Idle, Instant) {
        let now = Instant::now();
        (Idle::new(now, 42), now)
    }

    #[test]
    fn startup_and_post_emote_rests_are_six_to_twelve_seconds() {
        let (mut idle, now) = start();
        let first = idle.phase.deadline().unwrap();
        assert!((Duration::from_secs(6)..=Duration::from_secs(12)).contains(&(first - now)));
        idle.advance(first - Duration::from_millis(1));
        assert_eq!(idle.moment(first), NEUTRAL);
        idle.advance(first);
        assert!(matches!(idle.phase, Phase::Playing { .. }));
        let end = idle.phase.deadline().unwrap();
        idle.advance(end);
        assert_eq!(idle.moment(end), NEUTRAL);
        assert!(
            (Duration::from_secs(6)..=Duration::from_secs(12))
                .contains(&(idle.phase.deadline().unwrap() - end))
        );
    }

    #[test]
    fn awake_typing_interrupts_and_restarts_the_three_second_quiet_window() {
        let (mut idle, now) = start();
        let typing = now + Duration::from_secs(10);
        idle.interact(typing);
        assert_eq!(idle.moment(typing), NEUTRAL);
        idle.advance(typing + QUIET - Duration::from_millis(1));
        assert!(matches!(idle.phase, Phase::Quiet(_)));
        idle.interact(typing + Duration::from_secs(2));
        idle.advance(typing + Duration::from_secs(4));
        assert!(matches!(idle.phase, Phase::Quiet(_)));
        idle.advance(typing + Duration::from_secs(5));
        assert!(matches!(idle.phase, Phase::Playing { .. }));
    }

    #[test]
    fn idle_becomes_sleepy_at_sixty_then_tucks_in_at_ninety() {
        let (mut idle, now) = start();
        idle.advance(now + SLEEPY_AFTER - Duration::from_millis(1));
        assert_eq!(idle.mood, Mood::Awake);
        idle.advance(now + SLEEPY_AFTER);
        assert_eq!(idle.mood, Mood::Sleepy);
        assert!(matches!(
            idle.phase,
            Phase::Playing {
                scene: Scene::Sleepy,
                ..
            }
        ));
        idle.advance(now + SLEEP_AFTER - Duration::from_millis(1));
        assert_eq!(idle.mood, Mood::Sleepy);
        idle.advance(now + SLEEP_AFTER);
        assert!(matches!(
            idle.phase,
            Phase::Playing {
                scene: Scene::FallAsleep,
                ..
            }
        ));
        let asleep = now + SLEEP_AFTER + scene_duration(Scene::FallAsleep);
        idle.advance(asleep);
        assert!(matches!(
            idle.phase,
            Phase::Playing {
                scene: Scene::Sleep,
                ..
            }
        ));
        assert_eq!(idle.moment(asleep).pose, P::Sleep);
        idle.advance(asleep + Duration::from_secs(3600));
        assert_eq!(
            idle.moment(asleep + Duration::from_secs(3600)).pose,
            P::Sleep
        );
        assert_eq!(idle.phase.deadline(), None);
    }

    #[test]
    fn sleepy_choices_are_calm_and_rests_are_fifteen_to_twenty_five_seconds() {
        let (mut idle, now) = start();
        for ms in (60000..90000).step_by(50) {
            idle.advance(now + Duration::from_millis(ms));
            match idle.phase {
                Phase::Playing { scene, .. } => {
                    assert!(scene == Scene::Sleepy || Group::Calm.scenes().contains(&scene))
                }
                Phase::Rest(until) => assert!(until > now + Duration::from_millis(ms)),
                _ => panic!("unexpected sleepy phase"),
            }
        }
        idle.mood = Mood::Sleepy;
        idle.rest(now);
        let rest = idle.phase.deadline().unwrap() - now;
        assert!((Duration::from_secs(15)..=Duration::from_secs(25)).contains(&rest));
    }

    #[test]
    fn sleeping_only_breathes_with_a_fixed_baseline_and_no_hops() {
        let (mut idle, now) = start();
        idle.advance(now + Duration::from_secs(100));
        let Phase::Playing {
            scene: Scene::Sleep,
            started,
        } = idle.phase
        else {
            panic!("not asleep");
        };
        for (ms, pose) in [
            (0, P::Sleep),
            (2699, P::Sleep),
            (2700, P::SleepBreath),
            (3799, P::SleepBreath),
            (3800, P::Sleep),
            (6000, P::Sleep),
        ] {
            let moment = idle.moment(started + Duration::from_millis(ms));
            assert_eq!(moment.pose, pose);
            assert_eq!(moment.expression, super::super::E::Sleep);
            assert_eq!(moment.hop, 0);
        }
    }

    #[test]
    fn wake_plays_once_despite_repeated_typing_then_observes_remaining_quiet_time() {
        let (mut idle, now) = start();
        let waking = now + Duration::from_secs(100);
        idle.interact(waking); // Also accounts for ticks delayed past sleep time.
        assert_eq!(
            idle.phase,
            Phase::Playing {
                scene: Scene::WakeUp,
                started: waking
            }
        );
        let mut elapsed = Duration::ZERO;
        for expected in Scene::WakeUp.sequence() {
            assert_eq!(idle.moment(waking + elapsed), *expected);
            elapsed += Duration::from_millis(expected.ms);
        }
        let more_typing = waking + Duration::from_millis(400);
        idle.interact(more_typing);
        assert_eq!(
            idle.phase,
            Phase::Playing {
                scene: Scene::WakeUp,
                started: waking
            }
        );
        let end = waking + scene_duration(Scene::WakeUp);
        idle.advance(end);
        assert_eq!(idle.moment(end), NEUTRAL);
        assert_eq!(idle.phase, Phase::Quiet(more_typing + QUIET));
        idle.advance(more_typing + QUIET - Duration::from_millis(1));
        assert!(matches!(idle.phase, Phase::Quiet(_)));
        idle.advance(more_typing + QUIET);
        assert!(matches!(idle.phase, Phase::Playing { .. }));
    }

    #[test]
    fn waking_while_drowsy_or_tucking_in_uses_the_current_body_pose() {
        for (ms, expected) in [
            (61500, Scene::WakeDrowsy),
            (90500, Scene::WakeDrowsy),
            (91500, Scene::WakeUp),
        ] {
            let (mut idle, now) = start();
            idle.interact(now + Duration::from_millis(ms));
            assert!(matches!(idle.phase, Phase::Playing { scene, .. } if scene == expected));
        }
    }

    #[test]
    fn a_new_idle_episode_resets_sleep_time_but_keeps_hop_history() {
        let (mut idle, now) = start();
        idle.last_hop = Some(now);
        idle.advance(now + Duration::from_secs(3600));
        let fresh = now + Duration::from_secs(3600);
        idle.reset(fresh);
        assert_eq!(idle.mood, Mood::Awake);
        assert_eq!(idle.inactive_since, fresh);
        assert_eq!(idle.last_hop, Some(now));
        assert_eq!(idle.moment(fresh), NEUTRAL);
        idle.advance(fresh + SLEEPY_AFTER - Duration::from_millis(1));
        assert_eq!(idle.mood, Mood::Awake);
    }

    #[test]
    fn shuffled_groups_cover_all_awake_scenes_without_repeats_or_early_hops() {
        let (mut idle, now) = start();
        let mut seen = Vec::new();
        let mut last = None;
        let mut hop = None;
        let mut everyday = 0;
        let mut playful = 0;
        for second in 0..3600 {
            let tick = now + Duration::from_secs(second);
            idle.advance(tick);
            if let Phase::Playing { scene, started } = idle.phase
                && last != Some(started)
            {
                assert_ne!(seen.last(), Some(&scene));
                if scene.is_hop() {
                    if let Some(previous) = hop {
                        assert!(started - previous >= HOP_COOLDOWN);
                    }
                    hop = Some(started);
                }
                if Group::Everyday.scenes().contains(&scene) {
                    everyday += 1;
                } else {
                    playful += 1;
                }
                seen.push(scene);
                last = Some(started);
            }
            // Keep normal active use awake, without bypassing the quiet window.
            if second % 45 == 44 {
                idle.interact(tick);
            }
        }
        for scene in Group::Everyday
            .scenes()
            .iter()
            .chain(Group::Playful.scenes())
        {
            assert!(seen.contains(scene), "missing {scene:?}");
        }
        let share = everyday as f64 / (everyday + playful) as f64;
        assert!((0.7..0.9).contains(&share), "everyday share {share}");
    }

    #[test]
    fn an_everyday_bag_visits_each_entry_before_refilling() {
        let (mut idle, now) = start();
        let mut seen = Vec::new();
        for _ in Group::Everyday.scenes() {
            let scene = idle.take(Group::Everyday, now);
            assert!(!seen.contains(&scene));
            idle.last = Some(scene);
            seen.push(scene);
        }
    }

    #[test]
    fn delayed_ticks_match_frequent_ticks_and_sampling_is_pure() {
        let (mut frequent, now) = start();
        let mut delayed = Idle::new(now, 42);
        for ms in (0..=180000).step_by(50) {
            frequent.advance(now + Duration::from_millis(ms));
            let before = (frequent.random, frequent.phase);
            frequent.moment(now + Duration::from_millis(ms));
            assert_eq!(before, (frequent.random, frequent.phase));
        }
        delayed.advance(now + Duration::from_secs(180));
        assert_eq!(frequent.random, delayed.random);
        assert_eq!(frequent.phase, delayed.phase);
        assert_eq!(frequent.last, delayed.last);
    }
}
