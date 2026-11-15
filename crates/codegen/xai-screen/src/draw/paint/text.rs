//! Text: labels and text panels, shaped in the desktop's monospace font and drawn from a
//! glyph atlas, one quad per glyph.

use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent,
    SwashImage, Weight, Wrap,
};
use glam::{Vec2, vec2};

use super::scene::Pin;
use super::{Canvas, HALO, HALO_ALPHA, MAX_SIDE, circle, rounded};
use crate::draw::gpu::{Image, Pipeline, Rect, Vertex, premultiply};
use crate::draw::model::Rgb;

const TAG_PADDING: Vec2 = vec2(8.0, 3.0);
/// A scene label's dot radius, and the length of each leg of its leader line.
const DOT: f32 = 3.5;
const LEADER: f32 = 22.0;
const PANEL_PADDING: Vec2 = vec2(12.0, 8.0);
/// Labels and text wrap at this many times their font size.
pub(super) const WRAP_EMS: f32 = 30.0;
pub(super) const LINE_HEIGHT: f32 = 1.3;
const ATLAS_WIDTH: u32 = 1024;

/// Shaped text and its size in logical units.
pub(super) struct Text {
    pub(super) buffer: Buffer,
    pub(super) size: Vec2,
}

impl Canvas<'_> {
    /// A label: a solid tag in `rgb` on the top-left corner of a frame outlined `line` wide.
    pub(super) fn tag(&mut self, label: &str, frame: Rect, rgb: Rgb, line: f32) {
        let size = self.style.font_size as f32;
        let max = (WRAP_EMS * size).min(self.screen.x) - 2.0 * TAG_PADDING.x;
        let text = self.layout(label, Weight::BOLD, size, max);
        let size = text.size + 2.0 * TAG_PADDING;
        let tag = tag_rect(frame, line, size, self.screen);
        self.fill(
            &rounded(tag, self.style.rounding as f32),
            premultiply(rgb, 255),
        );
        let ink = self.style.palette.background;
        self.write(&text, vec2(tag.x, tag.y) + TAG_PADDING, ink);
    }

    /// A scene label: a dot on its point and a leader line out to its side, ending at its
    /// text on a panel outlined in its color.
    pub(super) fn pin(&mut self, pin: &Pin) {
        self.alpha = pin.alpha;
        let style = self.style;
        let (size, line) = (style.font_size as f32, style.line_width as f32);
        let text = self.layout(&pin.text, Weight::BOLD, size, WRAP_EMS * size);
        let (ink, halo) = (
            premultiply(pin.rgb, 255),
            premultiply(style.palette.background, HALO_ALPHA),
        );
        let elbow = pin.at + vec2(pin.side, -1.0) * LEADER;
        let end = elbow + vec2(pin.side * LEADER, 0.0);
        let leader = [pin.at, elbow, end];
        self.stroke(&leader, false, line + 2.0 * HALO, halo);
        self.stroke(&leader, false, line, ink);
        self.fill(&circle(pin.at, DOT + HALO), halo);
        self.fill(&circle(pin.at, DOT), ink);
        let size = text.size + 2.0 * TAG_PADDING;
        let left = if pin.side < 0.0 {
            end.x - size.x
        } else {
            end.x
        };
        let panel = panel_rect(vec2(left, end.y - size.y / 2.0), size, self.screen);
        let outline = rounded(panel, style.rounding as f32);
        self.fill(&outline, premultiply(style.palette.background, 235));
        self.stroke(&outline, true, line, ink);
        let at = vec2(panel.x, panel.y) + TAG_PADDING;
        self.write(&text, at, style.palette.foreground);
    }

    /// A text note at `size` on a panel of `alpha` opacity: theme text inside an outline
    /// in `rgb`, or text in `rgb` when there is no outline.
    pub(super) fn panel(
        &mut self,
        content: &str,
        at: Vec2,
        rgb: Rgb,
        size: f32,
        (alpha, border): (u8, bool),
    ) {
        let style = self.style;
        let max = (WRAP_EMS * size).min(self.screen.x) - 2.0 * PANEL_PADDING.x;
        let text = self.layout(content, Weight::NORMAL, size, max);
        let panel = panel_rect(at, text.size + 2.0 * PANEL_PADDING, self.screen);
        let outline = rounded(panel, style.rounding as f32);
        self.fill(&outline, premultiply(style.palette.background, alpha));
        if border {
            let line = style.line_width as f32;
            self.stroke(&outline, true, line, premultiply(rgb, 255));
        }
        let ink = if border {
            style.palette.foreground
        } else {
            rgb
        };
        self.write(&text, vec2(panel.x, panel.y) + PANEL_PADDING, ink);
    }

    /// Shape `content` in the style's font at `size`, wrapped to `max_width` logical units.
    pub(super) fn layout(
        &mut self,
        content: &str,
        weight: Weight,
        size: f32,
        max_width: f32,
    ) -> Text {
        let scale = self.frame.scale;
        let size = size * scale;
        let mut buffer = Buffer::new(self.fonts, Metrics::new(size, size * LINE_HEIGHT));
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_size(Some(max_width.max(1.0) * scale), None);
        let attrs = Attrs::new()
            .family(Family::Name(&self.style.font))
            .weight(weight);
        buffer.set_text(content, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(self.fonts, false);
        let (width, height) = buffer
            .layout_runs()
            .fold((0.0_f32, 0.0_f32), |(w, h), run| {
                (w.max(run.line_w), h.max(run.line_top + run.line_height))
            });
        Text {
            buffer,
            size: vec2(width, height) / scale,
        }
    }

    /// Draw shaped text with its top-left corner at `at`, one quad per glyph.
    pub(super) fn write(&mut self, text: &Text, at: Vec2, rgb: Rgb) {
        let scale = self.frame.scale;
        // Glyphs land on whole pixels, as the rasterizer drew them.
        let origin = (at * scale).round();
        let ink = premultiply(rgb, 255);
        let mut triangles = Vec::new();
        for run in text.buffer.layout_runs() {
            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, run.line_y), 1.0);
                let Some(slot) = self
                    .atlas
                    .glyph(self.fonts, self.glyphs, physical.cache_key)
                else {
                    continue;
                };
                let corner = origin + vec2(physical.x as f32, physical.y as f32) + slot.offset;
                let color = if slot.colored { [1.0; 4] } else { ink };
                let [a, b, c, d] = [
                    vec2(0.0, 0.0),
                    vec2(1.0, 0.0),
                    vec2(0.0, 1.0),
                    vec2(1.0, 1.0),
                ]
                .map(|unit| unit * slot.size);
                triangles.extend([a, b, c, b, d, c].map(|offset| {
                    Vertex::glyph((corner + offset) / scale, slot.texel + offset, color)
                }));
            }
        }
        self.push(Pipeline::Glyph, 0, self.clip, triangles);
    }
}

/// Where a glyph sits in the atlas, in pixels.
#[derive(Debug, Clone, Copy)]
struct Slot {
    texel: Vec2,
    size: Vec2,
    /// From the pen position to the image's top-left corner.
    offset: Vec2,
    /// Color glyphs, such as emoji, keep their own colors.
    colored: bool,
}

/// Glyph images packed in rows into one texture of premultiplied RGBA.
pub(super) struct Atlas {
    pub(super) image: Image,
    slots: HashMap<CacheKey, Option<Slot>>,
    /// Where the next glyph goes, and the height of the current row.
    cursor: (u32, u32),
    row: u32,
}

impl Default for Atlas {
    fn default() -> Self {
        Self {
            image: Image::transparent(ATLAS_WIDTH, 0),
            slots: HashMap::new(),
            cursor: (0, 0),
            row: 0,
        }
    }
}

impl Atlas {
    fn glyph(
        &mut self,
        fonts: &mut FontSystem,
        cache: &mut SwashCache,
        key: CacheKey,
    ) -> Option<Slot> {
        if let Some(slot) = self.slots.get(&key) {
            return *slot;
        }
        let slot = cache
            .get_image(fonts, key)
            .as_ref()
            .and_then(|image| self.pack(image));
        self.slots.insert(key, slot);
        slot
    }

    fn pack(&mut self, glyph: &SwashImage) -> Option<Slot> {
        let (width, height) = (glyph.placement.width, glyph.placement.height);
        let colored = match glyph.content {
            SwashContent::Mask => false,
            SwashContent::Color => true,
            SwashContent::SubpixelMask => return None,
        };
        if width == 0 || height == 0 || width > ATLAS_WIDTH {
            return None;
        }
        if self.cursor.0 + width > ATLAS_WIDTH {
            self.cursor = (0, self.cursor.1 + self.row);
            self.row = 0;
        }
        let (x, y) = self.cursor;
        if y + height > MAX_SIDE {
            return None;
        }
        self.cursor.0 += width;
        self.row = self.row.max(height);
        let image = &mut self.image;
        if image.height < y + height {
            image.height = y + height;
            image
                .data
                .resize((ATLAS_WIDTH * image.height * 4) as usize, 0);
        }
        let channels = if colored { 4 } else { 1 };
        for (row, source) in glyph
            .data
            .chunks_exact((width * channels) as usize)
            .enumerate()
        {
            let start = ((y as usize + row) * ATLAS_WIDTH as usize + x as usize) * 4;
            let Some(target) = image.data.get_mut(start..start + width as usize * 4) else {
                continue;
            };
            for (pixel, texel) in target
                .chunks_exact_mut(4)
                .zip(source.chunks_exact(channels as usize))
            {
                let (rgb, alpha) = match *texel {
                    [r, g, b, alpha] => ([r, g, b], alpha),
                    [alpha] => ([255; 3], alpha),
                    _ => continue,
                };
                let [r, g, b] =
                    rgb.map(|value| ((u32::from(value) * u32::from(alpha) + 127) / 255) as u8);
                pixel.copy_from_slice(&[r, g, b, alpha]);
            }
        }
        Some(Slot {
            texel: vec2(x as f32, y as f32),
            size: vec2(width as f32, height as f32),
            offset: vec2(glyph.placement.left as f32, -glyph.placement.top as f32),
            colored,
        })
    }
}

/// Where a label goes: above the frame's top-left outer corner, or just inside the frame
/// when the screen's top edge is too close. It never leaves the screen horizontally.
fn tag_rect(frame: Rect, line: f32, size: Vec2, screen: Vec2) -> Rect {
    let outer = line / 2.0;
    let above = frame.y - outer - size.y;
    Rect {
        x: (frame.x - outer).min(screen.x - size.x).max(0.0),
        y: if above >= 0.0 { above } else { frame.y + outer },
        width: size.x,
        height: size.y,
    }
}

/// Where a text panel goes: its top-left corner at `at`, moved back onto the screen.
fn panel_rect(at: Vec2, size: Vec2, screen: Vec2) -> Rect {
    let corner = at.min(screen - size).max(Vec2::ZERO);
    Rect {
        x: corner.x,
        y: corner.y,
        width: size.x,
        height: size.y,
    }
}
