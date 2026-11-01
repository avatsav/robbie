//! Little bot's geometry, expressions, and palettes, owned by Robbie.
//! Expressions and shading adapted from the user-supplied robbie-squish library
//! (declared MIT); the bot silhouettes are hand-drawn for Robbie.

pub(super) const SIZE: usize = 16;
pub(super) const COMPACT_SIZE: usize = 12;
pub(super) const COMPACT_TERMINAL_ROWS: usize = COMPACT_SIZE / 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rgb(pub(super) u8, pub(super) u8, pub(super) u8);

pub(super) type Sprite = [[Option<Rgb>; SIZE]; SIZE];
pub(super) type CompactSprite = [[Option<Rgb>; COMPACT_SIZE]; COMPACT_SIZE];

// Two empty rows keep the aerial intact even at the highest two-pixel hop.
// Both poses share a foot baseline and leave the final row transparent.
const SHAPE: [&str; SIZE] = [
    "................",
    "................",
    ".......##.......",
    ".......##.......",
    "...##########...",
    "..############..",
    "..############..",
    "..############..",
    "..############..",
    "..############..",
    "..############..",
    "..############..",
    "...##########...",
    "....###..###....",
    "....###..###....",
    "................",
];

// Bottom-anchored squish: one pixel shorter, one pixel wider on each side.
const SQUISH_SHAPE: [&str; SIZE] = [
    "................",
    "................",
    "................",
    ".......##.......",
    ".......##.......",
    "..############..",
    ".##############.",
    ".##############.",
    ".##############.",
    ".##############.",
    ".##############.",
    ".##############.",
    "..############..",
    "...####..####...",
    "...####..####...",
    "................",
];

// Hand-drawn compact silhouette, not a rescaled 16x16 image.
const SHAPE_12: [&str; COMPACT_SIZE] = [
    "............",
    "............",
    ".....##.....",
    "..########..",
    ".##########.",
    ".##########.",
    ".##########.",
    ".##########.",
    ".##########.",
    "...##..##...",
    "...##..##...",
    "............",
];

const SQUISH_SHAPE_12: [&str; COMPACT_SIZE] = [
    "............",
    "............",
    "............",
    ".....##.....",
    ".##########.",
    "############",
    "############",
    "############",
    "############",
    "..###..###..",
    "..###..###..",
    "............",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BodyPose {
    Neutral,
    Squish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Palette {
    pub(super) highlight: Rgb,
    pub(super) body: Rgb,
    pub(super) shadow: Rgb,
    pub(super) eyes: Rgb,
}

impl Palette {
    pub(super) const MINT: Self = Self::new(
        Rgb(201, 255, 226),
        Rgb(109, 223, 181),
        Rgb(48, 155, 134),
        Rgb(24, 62, 64),
    );
    pub(super) const BLUE: Self = Self::new(
        Rgb(167, 229, 255),
        Rgb(84, 180, 239),
        Rgb(40, 119, 192),
        Rgb(23, 45, 72),
    );
    pub(super) const LILAC: Self = Self::new(
        Rgb(237, 206, 255),
        Rgb(180, 138, 236),
        Rgb(117, 86, 183),
        Rgb(48, 36, 71),
    );
    pub(super) const AMBER: Self = Self::new(
        Rgb(255, 227, 154),
        Rgb(255, 191, 86),
        Rgb(201, 122, 42),
        Rgb(86, 50, 31),
    );
    pub(super) const PINK: Self = Self::new(
        Rgb(255, 210, 226),
        Rgb(242, 140, 176),
        Rgb(187, 84, 125),
        Rgb(72, 39, 58),
    );
    pub(super) const LIME: Self = Self::new(
        Rgb(232, 255, 165),
        Rgb(182, 222, 96),
        Rgb(105, 154, 61),
        Rgb(48, 67, 37),
    );

    const fn new(highlight: Rgb, body: Rgb, shadow: Rgb, eyes: Rgb) -> Self {
        Self {
            highlight,
            body,
            shadow,
            eyes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Expression {
    Idle,
    LookLeft,
    LookRight,
    Thinking,
    Focused,
    Happy,
    Surprised,
    Confused,
    Blink,
}

fn eye_pixel<const N: usize>(
    pixels: &mut [[Option<Rgb>; N]; N],
    shape: &[&str; N],
    x: usize,
    y: usize,
    color: Rgb,
) {
    // Never draw an eye outside the body, including during a glance or squish.
    if x < N && y < N && shape[y].as_bytes()[x] == b'#' {
        pixels[y][x] = Some(color);
    }
}

fn eye_rect<const N: usize>(
    pixels: &mut [[Option<Rgb>; N]; N],
    shape: &[&str; N],
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    color: Rgb,
) {
    for yy in y..y + h {
        for xx in x..x + w {
            eye_pixel(pixels, shape, xx, yy, color);
        }
    }
}

fn sprite_grid<const N: usize>(
    shape: &[&str; N],
    expression: Expression,
    hop: usize,
    palette: Palette,
) -> [[Option<Rgb>; N]; N] {
    let mut pixels = [[None; N]; N];
    for (y, row) in pixels.iter_mut().enumerate() {
        let bytes = shape[y].as_bytes();
        for (x, pixel) in row.iter_mut().enumerate() {
            if bytes[x] != b'#' {
                continue;
            }
            let left_edge = x == 0 || bytes[x - 1] == b'.';
            let right_edge = x == N - 1 || bytes[x + 1] == b'.';
            *pixel = Some(if y >= N - 3 || (right_edge && y >= N / 2 - 1) {
                palette.shadow
            } else if (left_edge && y <= N - 5) || (y <= (if N == 16 { 4 } else { 3 }) && x < N / 2)
            {
                palette.highlight
            } else {
                palette.body
            });
        }
    }

    // Pixel-art decisions for each resolution, not rescaled vector geometry.
    // Raise the face above the feet; every expression uses these same anchors.
    let (lx, rx, ey) = if N == 16 { (5, 10, 9) } else { (3, 7, 6) };
    let eye = palette.eyes;
    match expression {
        Expression::Idle => {
            eye_rect(&mut pixels, shape, lx, ey, 2, 2, eye);
            eye_rect(&mut pixels, shape, rx, ey, 2, 2, eye);
        }
        Expression::LookLeft => {
            eye_rect(&mut pixels, shape, lx - 1, ey, 2, 2, eye);
            eye_rect(&mut pixels, shape, rx - 1, ey, 2, 2, eye);
        }
        Expression::LookRight => {
            eye_rect(&mut pixels, shape, lx + 1, ey, 2, 2, eye);
            eye_rect(&mut pixels, shape, rx + 1, ey, 2, 2, eye);
        }
        Expression::Thinking => {
            eye_rect(&mut pixels, shape, lx, ey - 1, 2, 2, eye);
            eye_rect(&mut pixels, shape, rx, ey - 1, 2, 2, eye);
        }
        Expression::Focused | Expression::Blink => {
            eye_rect(&mut pixels, shape, lx, ey + 1, 2, 1, eye);
            eye_rect(&mut pixels, shape, rx, ey + 1, 2, 1, eye);
        }
        Expression::Happy => {
            for x in [lx, rx] {
                eye_pixel(&mut pixels, shape, x - 1, ey + 1, eye);
                eye_pixel(&mut pixels, shape, x, ey, eye);
                eye_pixel(&mut pixels, shape, x + 1, ey, eye);
                eye_pixel(&mut pixels, shape, x + 2, ey + 1, eye);
            }
        }
        Expression::Surprised => {
            eye_rect(&mut pixels, shape, lx, ey - 1, 2, 3, eye);
            eye_rect(&mut pixels, shape, rx, ey - 1, 2, 3, eye);
        }
        Expression::Confused => {
            eye_rect(&mut pixels, shape, lx, ey, 2, 2, eye);
            eye_rect(&mut pixels, shape, rx, ey - 1, 2, 2, eye);
        }
    }

    // Lift silhouette and face together, clamping the original two-pixel hop.
    let mut lifted = [[None; N]; N];
    let hop = hop.min(2);
    lifted[..N - hop].copy_from_slice(&pixels[hop..]);
    lifted
}

pub(super) fn sprite_with_pose(
    expression: Expression,
    hop: usize,
    pose: BodyPose,
    palette: Palette,
) -> Sprite {
    let shape = match pose {
        BodyPose::Neutral => &SHAPE,
        BodyPose::Squish => &SQUISH_SHAPE,
    };
    sprite_grid(shape, expression, hop, palette)
}

pub(super) fn sprite_12_with_pose(
    expression: Expression,
    hop: usize,
    pose: BodyPose,
    palette: Palette,
) -> CompactSprite {
    let shape = match pose {
        BodyPose::Neutral => &SHAPE_12,
        BodyPose::Squish => &SQUISH_SHAPE_12,
    };
    sprite_grid(shape, expression, hop, palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPRESSIONS: [Expression; 9] = [
        Expression::Idle,
        Expression::LookLeft,
        Expression::LookRight,
        Expression::Thinking,
        Expression::Focused,
        Expression::Happy,
        Expression::Surprised,
        Expression::Confused,
        Expression::Blink,
    ];

    fn check_outline<const N: usize>(shape: &[&str; N]) {
        assert!(shape.iter().all(|row| row.len() == N));
        for expression in EXPRESSIONS {
            for palette in super::super::PALETTES {
                let pixels = sprite_grid(shape, expression, 0, palette);
                for (y, row) in pixels.iter().enumerate() {
                    for (x, pixel) in row.iter().enumerate() {
                        assert_eq!(pixel.is_some(), shape[y].as_bytes()[x] == b'#');
                        if let Some(color) = pixel {
                            assert!(
                                [
                                    palette.body,
                                    palette.highlight,
                                    palette.shadow,
                                    palette.eyes
                                ]
                                .contains(color)
                            );
                        }
                    }
                }
                let expected_eye_pixels = match expression {
                    Expression::Focused | Expression::Blink => 4,
                    Expression::Surprised => 12,
                    _ => 8,
                };
                assert_eq!(
                    pixels
                        .iter()
                        .flatten()
                        .filter(|pixel| **pixel == Some(palette.eyes))
                        .count(),
                    expected_eye_pixels,
                    "clipped eyes in {N}×{N} {expression:?}"
                );
            }
        }
    }

    #[test]
    fn both_resolutions_and_poses_preserve_outlines_and_palette_colors() {
        check_outline(&SHAPE);
        check_outline(&SQUISH_SHAPE);
        check_outline(&SHAPE_12);
        check_outline(&SQUISH_SHAPE_12);
    }

    #[test]
    fn both_resolutions_keep_the_aerial_boxy_body_and_separate_feet() {
        assert_eq!(&SHAPE[2..4], &[".......##......."; 2]);
        assert_eq!(SHAPE[4], "...##########...");
        assert_eq!(&SHAPE[5..12], &["..############.."; 7]);
        assert_eq!(&SHAPE[13..15], &["....###..###...."; 2]);
        assert_eq!(&SQUISH_SHAPE[3..5], &[".......##......."; 2]);
        assert_eq!(&SQUISH_SHAPE[6..12], &[".##############."; 6]);
        assert_eq!(&SQUISH_SHAPE[13..15], &["...####..####..."; 2]);

        assert_eq!(SHAPE_12[2], ".....##.....");
        assert_eq!(SHAPE_12[3], "..########..");
        assert_eq!(&SHAPE_12[4..9], &[".##########."; 5]);
        assert_eq!(&SHAPE_12[9..11], &["...##..##..."; 2]);
        assert_eq!(SQUISH_SHAPE_12[3], ".....##.....");
        assert_eq!(&SQUISH_SHAPE_12[5..9], &["############"; 4]);
        assert_eq!(&SQUISH_SHAPE_12[9..11], &["..###..###.."; 2]);
    }

    #[test]
    fn face_is_raised_above_the_feet_in_both_poses() {
        for pose in [BodyPose::Neutral, BodyPose::Squish] {
            let pixels = sprite_with_pose(Expression::Idle, 0, pose, Palette::MINT);
            for y in [9, 10] {
                for x in [5, 6, 10, 11] {
                    assert_eq!(pixels[y][x], Some(Palette::MINT.eyes));
                }
            }
            let pixels = sprite_12_with_pose(Expression::Idle, 0, pose, Palette::MINT);
            for y in [6, 7] {
                for x in [3, 4, 7, 8] {
                    assert_eq!(pixels[y][x], Some(Palette::MINT.eyes));
                }
            }
        }
    }

    fn check_hops<const N: usize>(shape: &[&str; N]) {
        assert!(shape[..2].iter().all(|row| row.bytes().all(|p| p == b'.')));
        assert!(shape[N - 1].bytes().all(|p| p == b'.'));
        for expression in EXPRESSIONS {
            let grounded = sprite_grid(shape, expression, 0, Palette::MINT);
            let occupied = grounded.iter().flatten().filter(|p| p.is_some()).count();
            for hop in [1, 2, usize::MAX] {
                let lifted = sprite_grid(shape, expression, hop, Palette::MINT);
                let hop = hop.min(2);
                assert_eq!(lifted[..N - hop], grounded[hop..]);
                assert!(lifted[N - hop..].iter().flatten().all(Option::is_none));
                assert_eq!(
                    lifted.iter().flatten().filter(|p| p.is_some()).count(),
                    occupied,
                    "a hop must not crop the aerial"
                );
            }
        }
    }

    #[test]
    fn hopping_moves_body_and_face_together_and_clears_the_bottom() {
        check_hops(&SHAPE);
        check_hops(&SQUISH_SHAPE);
        check_hops(&SHAPE_12);
        check_hops(&SQUISH_SHAPE_12);
    }

    fn bounds(shape: &[&str]) -> (usize, usize, usize, usize) {
        let occupied: Vec<_> = shape
            .iter()
            .enumerate()
            .flat_map(|(y, row)| {
                row.bytes()
                    .enumerate()
                    .filter_map(move |(x, pixel)| (pixel == b'#').then_some((x, y)))
            })
            .collect();
        (
            occupied.iter().map(|&(x, _)| x).min().unwrap(),
            occupied.iter().map(|&(x, _)| x).max().unwrap(),
            occupied.iter().map(|&(_, y)| y).min().unwrap(),
            occupied.iter().map(|&(_, y)| y).max().unwrap(),
        )
    }

    #[test]
    fn squish_is_one_pixel_shorter_and_two_wider_with_a_fixed_baseline() {
        for (neutral, squished) in [
            (&SHAPE[..], &SQUISH_SHAPE[..]),
            (&SHAPE_12[..], &SQUISH_SHAPE_12[..]),
        ] {
            let (left, right, top, bottom) = bounds(neutral);
            let (sleft, sright, stop, sbottom) = bounds(squished);
            assert_eq!(sleft + 1, left);
            assert_eq!(sright, right + 1);
            assert_eq!(stop, top + 1);
            assert_eq!(sbottom, bottom);
        }
    }
}
