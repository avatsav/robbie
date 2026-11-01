//! Startup-only image detection and encoding through ratatui-image's Picker.
//! No image files, filesystem caches, or encoding in the draw loop.
use std::io::IsTerminal;

use super::sprite::{Palette, Rgb, SIZE as SPRITE_SIZE, Sprite, sprite_with_pose};
use image::{DynamicImage, Rgba, RgbaImage};
use ratatui_image::{
    FontSize, Resize,
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};

use super::{ERROR, IDLE, IMAGE_SIZE as SIZE, Moment, PALETTES, SUCCESS, THINKING, WORKING};

pub(super) struct Graphics {
    frames: Vec<(Sprite, Protocol)>,
}

impl Graphics {
    // Must run after entering the alternate screen, before EventStream owns stdin.
    pub(super) fn detect() -> Option<Self> {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return None;
        }
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        Self::from_picker(&picker)
    }

    pub(super) fn from_picker(picker: &Picker) -> Option<Self> {
        if !matches!(
            picker.protocol_type(),
            ProtocolType::Kitty | ProtocolType::Sixel | ProtocolType::Iterm2
        ) {
            return None;
        }
        let font = picker.font_size();
        let mut frames: Vec<(Sprite, Protocol)> = Vec::new();
        for palette in PALETTES {
            for moment in IDLE
                .iter()
                .chain(THINKING)
                .chain(WORKING)
                .chain(SUCCESS)
                .chain(ERROR)
            {
                let pixels = sprite_with_pose(moment.expression, moment.hop, moment.pose, palette);
                if frames.iter().any(|(cached, _)| *cached == pixels) {
                    continue;
                }
                let image = canvas(&pixels, font)?;
                let protocol = picker
                    .new_protocol(DynamicImage::ImageRgba8(image), SIZE, Resize::Fit(None))
                    .ok()?;
                frames.push((pixels, protocol));
            }
        }
        Some(Self { frames })
    }

    pub(super) fn frame(&self, moment: Moment, palette: Palette) -> Option<&Protocol> {
        let pixels = sprite_with_pose(moment.expression, moment.hop, moment.pose, palette);
        self.frames
            .iter()
            .find(|(cached, _)| *cached == pixels)
            .map(|(_, protocol)| protocol)
    }
}

fn canvas(pixels: &Sprite, font: FontSize) -> Option<RgbaImage> {
    let width = u32::from(font.width) * u32::from(SIZE.width);
    let height = u32::from(font.height) * u32::from(SIZE.height);
    // Reject implausible capability responses before allocating image buffers.
    if width > 1024 || height > 1024 {
        return None;
    }
    let scale = width.min(height) / SPRITE_SIZE as u32;
    if scale == 0 {
        return None;
    }
    let side = SPRITE_SIZE as u32 * scale;
    let left = (width - side) / 2;
    // Both source poses leave the last sprite row transparent. Anchor their
    // shared grounded baseline at the canvas bottom instead of centering that
    // empty row. Keep this offset fixed across frames so hops still lift up.
    let top = height - (SPRITE_SIZE as u32 - 1) * scale;
    let mut image = RgbaImage::new(width, height);
    for (y, row) in pixels.iter().enumerate() {
        for (x, pixel) in row.iter().enumerate() {
            if let Some(Rgb(r, g, b)) = pixel {
                for dy in 0..scale {
                    for dx in 0..scale {
                        image.put_pixel(
                            left + x as u32 * scale + dx,
                            top + y as u32 * scale + dy,
                            Rgba([*r, *g, *b, 255]),
                        );
                    }
                }
            }
        }
    }
    Some(image)
}

#[cfg(test)]
mod tests {
    use super::super::{E, P, m};
    use super::*;

    #[test]
    fn picker_selection_and_cached_frames_cover_every_scene_and_palette() {
        let mut picker = Picker::halfblocks();
        assert!(Graphics::from_picker(&picker).is_none());
        for kind in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
        ] {
            picker.set_protocol_type(kind);
            let graphics = Graphics::from_picker(&picker).unwrap();
            let mut unique = Vec::new();
            for palette in PALETTES {
                for moment in IDLE
                    .iter()
                    .chain(THINKING)
                    .chain(WORKING)
                    .chain(SUCCESS)
                    .chain(ERROR)
                {
                    let pixels =
                        sprite_with_pose(moment.expression, moment.hop, moment.pose, palette);
                    if !unique.contains(&pixels) {
                        unique.push(pixels);
                    }
                    let protocol = graphics.frame(*moment, palette).unwrap();
                    assert_eq!(protocol.size(), SIZE);
                    assert!(matches!(
                        (kind, protocol),
                        (ProtocolType::Kitty, Protocol::Kitty(_))
                            | (ProtocolType::Sixel, Protocol::Sixel(_))
                            | (ProtocolType::Iterm2, Protocol::ITerm2(_))
                    ));
                    assert!(std::ptr::eq(
                        protocol,
                        graphics.frame(*moment, palette).unwrap()
                    ));
                }
            }
            assert_eq!(graphics.frames.len(), unique.len());
        }
    }

    #[test]
    fn canvas_keeps_square_pixels_exact_colors_transparency_and_hop_offsets() {
        let pixels = sprite_with_pose(E::Idle, 0, P::Neutral, Palette::MINT);
        let grounded = canvas(&pixels, FontSize::new(8, 16)).unwrap();
        assert_eq!(grounded.dimensions(), (48, 48));
        assert_eq!(grounded.get_pixel(0, 0).0, [0, 0, 0, 0]);
        // Both feet touch the shared baseline; the gap between them stays clear.
        assert_ne!(grounded.get_pixel(12, 47).0[3], 0);
        assert_ne!(grounded.get_pixel(33, 47).0[3], 0);
        assert_eq!(grounded.get_pixel(24, 47).0[3], 0);
        for (y, row) in pixels.iter().enumerate() {
            for (x, pixel) in row.iter().enumerate() {
                if let Some(Rgb(r, g, b)) = pixel {
                    for dy in 0..3 {
                        for dx in 0..3 {
                            assert_eq!(
                                grounded
                                    .get_pixel(x as u32 * 3 + dx, 3 + y as u32 * 3 + dy)
                                    .0,
                                [*r, *g, *b, 255]
                            );
                        }
                    }
                }
            }
        }
        let moment = m(E::Idle, 2, P::Neutral, 100);
        let hopped = canvas(
            &sprite_with_pose(moment.expression, moment.hop, moment.pose, Palette::MINT),
            FontSize::new(8, 16),
        )
        .unwrap();
        for y in 0..42 {
            for x in 0..48 {
                assert_eq!(hopped.get_pixel(x, y), grounded.get_pixel(x, y + 6));
            }
        }
        assert!((42..48).all(|y| (0..48).all(|x| hopped.get_pixel(x, y).0[3] == 0)));
    }

    #[test]
    fn invalid_or_implausible_cell_dimensions_fail_without_allocating_large_images() {
        let pixels = sprite_with_pose(E::Idle, 0, P::Neutral, Palette::MINT);
        for font in [
            FontSize::new(0, 16),
            FontSize::new(8, 0),
            FontSize::new(1, 1),
            FontSize::new(512, 512),
        ] {
            assert!(canvas(&pixels, font).is_none());
        }
    }
}
