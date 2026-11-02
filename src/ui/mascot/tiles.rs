//! Solid half-block tiles for terminals without confirmed image support.
use super::sprite::{COMPACT_SIZE, COMPACT_TERMINAL_ROWS, Palette, Rgb, sprite_12_with_pose};
use ratatui::{
    buffer::Buffer,
    layout::{Rect, Size},
    style::Color,
};

use super::Moment;

pub(super) const SIZE: Size = Size::new(COMPACT_SIZE as u16, COMPACT_TERMINAL_ROWS as u16);

pub(super) fn render(moment: Moment, palette: Palette, area: Rect, buf: &mut Buffer) {
    let pixels = sprite_12_with_pose(moment.expression, moment.hop, moment.pose, palette);
    let area = area.intersection(buf.area);
    // Each cell preserves the colors of two vertically adjacent sprite pixels.
    for cy in 0..area.height.min(SIZE.height) {
        for cx in 0..area.width.min(SIZE.width) {
            let cell = &mut buf[(area.x + cx, area.y + cy)];
            cell.reset();
            let top = pixels[cy as usize * 2][cx as usize];
            let bottom = pixels[cy as usize * 2 + 1][cx as usize];
            match (top, bottom) {
                (Some(top), Some(bottom)) => {
                    cell.set_char('▀').set_fg(rgb(top)).set_bg(rgb(bottom));
                }
                (Some(top), None) => {
                    cell.set_char('▀').set_fg(rgb(top));
                }
                (None, Some(bottom)) => {
                    cell.set_char('▄').set_fg(rgb(bottom));
                }
                (None, None) => {}
            }
        }
    }
}

fn rgb(Rgb(r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::super::{IDLE, PALETTES, SUCCESS, all_moments};
    use super::*;

    #[test]
    fn tiles_preserve_every_pixel_color_and_transparency_at_an_offset() {
        for palette in PALETTES {
            for moment in all_moments() {
                let pixels =
                    sprite_12_with_pose(moment.expression, moment.hop, moment.pose, palette);
                let area = Rect::new(2, 3, SIZE.width, SIZE.height);
                let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 12));
                render(*moment, palette, area, &mut buffer);
                for y in 0..SIZE.height {
                    for x in 0..SIZE.width {
                        let cell = &buffer[(area.x + x, area.y + y)];
                        let (top, bottom) = match cell.symbol() {
                            "▀" => (Some(cell.fg), (cell.bg != Color::Reset).then_some(cell.bg)),
                            "▄" => {
                                assert_eq!(cell.bg, Color::Reset);
                                (None, Some(cell.fg))
                            }
                            " " => {
                                assert_eq!(cell.fg, Color::Reset);
                                assert_eq!(cell.bg, Color::Reset);
                                (None, None)
                            }
                            other => panic!("unexpected tile: {other}"),
                        };
                        assert_eq!(top, pixels[y as usize * 2][x as usize].map(rgb));
                        assert_eq!(bottom, pixels[y as usize * 2 + 1][x as usize].map(rgb));
                    }
                }
                assert_eq!(buffer[(0, 0)].symbol(), " ");
            }
        }
    }

    #[test]
    fn clipped_redraws_clear_old_poses_without_writing_outside_the_area() {
        for width in 0..=SIZE.width {
            for height in 0..=SIZE.height {
                let area = Rect::new(2, 2, width, height);
                let mut actual = Buffer::empty(Rect::new(0, 0, 20, 12));
                render(SUCCESS[2], Palette::MINT, area, &mut actual);
                render(IDLE[0], Palette::BLUE, area, &mut actual);
                let mut expected = Buffer::empty(actual.area);
                render(IDLE[0], Palette::BLUE, area, &mut expected);
                assert_eq!(actual, expected);
            }
        }
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 3));
        render(IDLE[0], Palette::MINT, Rect::new(2, 2, 12, 6), &mut buffer);
    }
}
