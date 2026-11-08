use super::*;

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
