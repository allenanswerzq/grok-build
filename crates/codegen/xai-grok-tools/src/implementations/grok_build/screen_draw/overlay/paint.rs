//! Turns a drawing into draw data for the GPU: boxes, lines, labels, text panels, and 3D
//! scenes, in the desktop's style.
//!
//! Geometry is in logical units; the screen camera maps it onto output pixels, and 4×
//! multisampling smooths its edges. Every line gets a thin halo in the theme background, so
//! it reads on light and dark windows alike. Lines and scenes come first and text last, so
//! nothing covers words.
//!
//! A 3D scene becomes lit triangles and a camera that circles the objects and fits them in
//! the scene's frame. Curved surfaces get smooth normals and flat faces keep sharp ones;
//! lines become thin rods with a constant width on screen. The GPU tests depth per pixel, so
//! objects may intersect and still look right.
//!
//! The user can orbit a scene's camera and zoom it ([`View`]). The fit comes from the
//! scene's own camera, so the size holds steady while the user orbits; zoom scales the fitted
//! image about its middle, and the frame clips whatever ends up outside it.

use std::collections::HashMap;

use anyhow::{Result, ensure};
use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent,
    SwashImage, Weight, Wrap,
};
use glam::{Mat4, Vec2, Vec3, Vec4, vec2};

use super::gpu::{Frame, Gpu, Image, Pipeline, Rect, Vertex, premultiply};
use crate::implementations::grok_build::screen_draw::{
    Arrows, BOX_FILL, Camera, Pattern, Point3, Rgb, SCENE_FILL, SceneObject, Shape, Style,
    TEXT_FILL,
};

const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 32 * 1024 * 1024;
/// Extra width on each side of a line, in the theme background.
const HALO: f32 = 1.5;
const HALO_ALPHA: u8 = 160;
const TAG_PADDING: Vec2 = vec2(8.0, 3.0);
const PANEL_PADDING: Vec2 = vec2(12.0, 8.0);
/// Labels and text wrap at this many times their font size.
const WRAP_EMS: f32 = 30.0;
const LINE_HEIGHT: f32 = 1.3;
/// Scene lines are a little wider than box lines, so they read among lit surfaces.
const SCENE_LINE: f32 = 1.25;
const CORNER_SEGMENTS: usize = 8;
const CIRCLE_SEGMENTS: usize = 32;
/// Sharp corners extend at most this many half widths.
const MITER_LIMIT: f32 = 4.0;
const ATLAS_WIDTH: u32 = 1024;
/// Segments around spheres, cylinders, and line rods, and sphere bands from pole to pole.
const SOLID_SEGMENTS: usize = 32;
const SPHERE_BANDS: usize = 16;
const ROD_SEGMENTS: usize = 8;
/// Eye distance in scene radii: enough perspective to read as 3D without distortion.
const EYE_DISTANCE: f32 = 3.5;
/// Space between a scene's objects and its frame, in logical units.
const SCENE_MARGIN: f32 = 20.0;

/// Paints drawings. Keeps the fonts, glyph cache, and GPU between drawings.
pub(crate) struct Painter {
    fonts: FontSystem,
    glyphs: SwashCache,
    gpu: Option<Gpu>,
}

impl Painter {
    pub(crate) fn new() -> Self {
        Self {
            fonts: FontSystem::new(),
            glyphs: SwashCache::new(),
            gpu: None,
        }
    }

    /// Paint `shapes` on a transparent image of `size` logical units at `scale` pixels each.
    /// Scene i is seen through `views[i]`; scenes without one keep their own camera.
    ///
    /// # Errors
    /// Fails when the image exceeds the pixel budget or the GPU cannot draw it.
    pub(crate) fn paint(
        &mut self,
        style: &Style,
        shapes: &[Shape],
        views: &[View],
        size: (u32, u32),
        scale: f64,
    ) -> Result<Image> {
        let scale = scale as f32;
        let pixels = |side: u32| (side as f32 * scale).ceil() as u32;
        let (width, height) = (pixels(size.0), pixels(size.1));
        ensure!(
            width > 0
                && height > 0
                && width <= MAX_SIDE
                && height <= MAX_SIDE
                && u64::from(width) * u64::from(height) <= MAX_PIXELS,
            "The drawing surface is too large: {width} × {height} pixels"
        );
        let mut canvas = Canvas {
            frame: Frame::new((width, height), scale),
            atlas: Atlas::default(),
            fonts: &mut self.fonts,
            glyphs: &mut self.glyphs,
            style,
            views,
            screen: vec2(size.0 as f32, size.1 as f32),
        };
        canvas.draw(shapes);
        let mut frame = canvas.frame;
        if frame.draws.is_empty() {
            return Ok(Image::transparent(width, height));
        }
        if canvas.atlas.image.height > 0 {
            frame.atlas = canvas.atlas.image;
        }
        let gpu = match &mut self.gpu {
            Some(gpu) => gpu,
            None => self.gpu.insert(Gpu::new()?),
        };
        gpu.render(&frame)
    }
}

/// How the user is looking at a scene: its camera after orbiting, and a zoom over the
/// fitted size.
#[derive(Debug, Clone, Copy)]
pub(super) struct View {
    pub(super) camera: Camera,
    pub(super) zoom: f32,
}

impl View {
    /// The scene's own camera at the fitted size.
    pub(super) fn new(camera: Camera) -> Self {
        Self { camera, zoom: 1.0 }
    }
}

/// Shaped text and its size in logical units.
struct Text {
    buffer: Buffer,
    size: Vec2,
}

/// Builds one frame.
struct Canvas<'a> {
    frame: Frame,
    atlas: Atlas,
    fonts: &'a mut FontSystem,
    glyphs: &'a mut SwashCache,
    style: &'a Style,
    /// Each scene's view after the user's orbiting and zooming, in scene order.
    views: &'a [View],
    /// Screen size in logical units.
    screen: Vec2,
}

impl Canvas<'_> {
    fn draw(&mut self, shapes: &[Shape]) {
        let style = self.style;
        let palette = &style.palette;
        let line = |width: &Option<f64>| width.unwrap_or(style.line_width) as f32;
        let halo = premultiply(palette.background, HALO_ALPHA);
        let scenes = shapes
            .iter()
            .filter(|shape| matches!(shape, Shape::Scene { .. }))
            .count();
        let mut scene_index = 0;
        for shape in shapes {
            match shape {
                Shape::Box {
                    x,
                    y,
                    width,
                    height,
                    color,
                    fill,
                    border,
                    line_width,
                    ..
                } => {
                    let outline = rounded(rect(*x, *y, *width, *height), style.rounding as f32);
                    let rgb = palette.rgb(*color);
                    self.fill(&outline, premultiply(rgb, opacity(*fill, BOX_FILL)));
                    if *border {
                        let line = line(line_width);
                        self.stroke(&outline, true, line + 2.0 * HALO, halo);
                        self.stroke(&outline, true, line, premultiply(rgb, 255));
                    }
                }
                Shape::Line {
                    x,
                    y,
                    to_x,
                    to_y,
                    color,
                    line_width,
                    arrows,
                    pattern,
                } => {
                    let ends = (vec2(*x as f32, *y as f32), vec2(*to_x as f32, *to_y as f32));
                    let width = line_width.unwrap_or(style.line_width * 1.5) as f32;
                    self.line(ends, palette.rgb(*color), width, *arrows, *pattern);
                }
                Shape::Text { .. } => {}
                Shape::Scene {
                    x,
                    y,
                    width,
                    height,
                    camera,
                    objects,
                    fill,
                    border,
                    line_width,
                    ..
                } => {
                    let frame = rect(*x, *y, *width, *height);
                    let outline = rounded(frame, style.rounding as f32);
                    let backdrop = opacity(*fill, SCENE_FILL);
                    self.fill(&outline, premultiply(palette.background, backdrop));
                    let line = line(line_width);
                    if *border {
                        self.stroke(&outline, true, line, premultiply(palette.accent, 255));
                    }
                    // Later scenes take nearer depth slices.
                    let slice = |index: usize| 1.0 - index as f32 / scenes as f32;
                    let depth = (slice(scene_index + 1), slice(scene_index));
                    let view = self
                        .views
                        .get(scene_index)
                        .copied()
                        .unwrap_or_else(|| View::new(*camera));
                    scene_index += 1;
                    self.scene(objects, (*camera, view), frame, depth, line);
                }
            }
        }
        for shape in shapes {
            match shape {
                Shape::Box {
                    x,
                    y,
                    width,
                    height,
                    label: Some(label),
                    color,
                    line_width,
                    ..
                } => {
                    let frame = rect(*x, *y, *width, *height);
                    self.tag(label, frame, palette.rgb(*color), line(line_width));
                }
                Shape::Scene {
                    x,
                    y,
                    width,
                    height,
                    label: Some(label),
                    line_width,
                    ..
                } => {
                    let frame = rect(*x, *y, *width, *height);
                    self.tag(label, frame, palette.accent, line(line_width));
                }
                Shape::Text {
                    x,
                    y,
                    text,
                    color,
                    size,
                    fill,
                    border,
                } => {
                    let size = size.unwrap_or(style.font_size) as f32;
                    let panel = (opacity(*fill, TEXT_FILL), *border);
                    let at = vec2(*x as f32, *y as f32);
                    self.panel(text, at, palette.rgb(*color), size, panel);
                }
                _ => {}
            }
        }
    }

    /// A label: a solid tag in `rgb` on the top-left corner of a frame outlined `line` wide.
    fn tag(&mut self, label: &str, frame: Rect, rgb: Rgb, line: f32) {
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

    /// A text note at `size` on a panel of `alpha` opacity: theme text inside an outline
    /// in `rgb`, or text in `rgb` when there is no outline.
    fn panel(&mut self, content: &str, at: Vec2, rgb: Rgb, size: f32, (alpha, border): (u8, bool)) {
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

    /// A line `width` wide between `ends`: solid with round ends, dashed, or dotted, with a
    /// solid arrowhead on each end that `arrows` names.
    fn line(
        &mut self,
        (from, to): (Vec2, Vec2),
        rgb: Rgb,
        width: f32,
        arrows: Arrows,
        pattern: Pattern,
    ) {
        let length = from.distance(to);
        if length == 0.0 {
            return;
        }
        let direction = (to - from) / length;
        let (at_start, at_end) = arrows.ends();
        let heads = usize::from(at_start) + usize::from(at_end);
        let head = (width * 4.0).max(12.0).min(length / heads.max(1) as f32);
        let tips: Vec<[Vec2; 3]> = [(at_start, from, -direction), (at_end, to, direction)]
            .into_iter()
            .filter(|&(on, ..)| on)
            .map(|(_, tip, direction)| {
                let base = tip - direction * head;
                let side = direction.perp() * head * 0.5;
                [tip, base + side, base - side]
            })
            .collect();
        // The line ends inside each head, which hides its end.
        let start = if at_start {
            from + direction * head * 0.5
        } else {
            from
        };
        let end = if at_end {
            to - direction * head * 0.5
        } else {
            to
        };
        let span = start.distance(end);
        let along = |distance: f32| start + direction * distance.min(span);
        let halo = premultiply(self.style.palette.background, HALO_ALPHA);
        // The halo first, then the line on top of it.
        for (color, extra) in [(halo, 2.0 * HALO), (premultiply(rgb, 255), 0.0)] {
            let wide = width + extra;
            match pattern {
                Pattern::Solid => {
                    self.stroke(&[start, end], false, wide, color);
                    for (cap, at) in [(!at_start, start), (!at_end, end)] {
                        if cap {
                            self.fill(&circle(at, wide / 2.0), color);
                        }
                    }
                }
                Pattern::Dashed => {
                    let (dash, gap) = (width * 4.0 + 4.0, width * 2.0 + 4.0);
                    let dashes = (span / (dash + gap)).ceil() as usize;
                    for index in 0..dashes {
                        let offset = index as f32 * (dash + gap);
                        self.stroke(&[along(offset), along(offset + dash)], false, wide, color);
                    }
                }
                Pattern::Dotted => {
                    // Evenly spaced dots, one on each end.
                    let gaps = (span / (width * 3.0 + 2.0)).round().max(1.0);
                    for index in 0..=gaps as usize {
                        let at = along(span * index as f32 / gaps);
                        self.fill(&circle(at, width * 0.7 + extra / 2.0), color);
                    }
                }
            }
            for tip in &tips {
                if extra > 0.0 {
                    self.stroke(tip, true, extra, color);
                } else {
                    self.fill(tip, color);
                }
            }
        }
    }

    /// A 3D scene in `frame`, fitted from `camera` and seen through `view`, drawn in its
    /// `depth` slice so it never mixes with other scenes. Line objects are rods about `line`
    /// wide on screen.
    fn scene(
        &mut self,
        objects: &[SceneObject],
        (camera, view): (Camera, View),
        frame: Rect,
        depth: (f32, f32),
        line: f32,
    ) {
        let style = self.style;
        let mut triangles = Vec::new();
        let mut lines = Vec::new();
        for object in objects {
            let color = |name| premultiply(style.palette.rgb(name), 255);
            match object {
                SceneObject::Cuboid {
                    center,
                    size,
                    color: name,
                } => cuboid(&mut triangles, point(center), point(size), color(*name)),
                SceneObject::Sphere {
                    center,
                    radius,
                    color: name,
                } => sphere(&mut triangles, point(center), *radius as f32, color(*name)),
                SceneObject::Cylinder {
                    from,
                    to,
                    radius,
                    color: name,
                } => {
                    let ends = (point(from), point(to));
                    cylinder(
                        &mut triangles,
                        ends,
                        *radius as f32,
                        SOLID_SEGMENTS,
                        color(*name),
                    );
                }
                SceneObject::Line {
                    from,
                    to,
                    color: name,
                } => lines.push((point(from), point(to), color(*name))),
                SceneObject::Mesh {
                    vertices,
                    faces,
                    color: name,
                } => {
                    let vertices: Vec<Vec3> = vertices.iter().map(point).collect();
                    for face in faces {
                        if let [Some(&a), Some(&b), Some(&c)] = face.map(|i| vertices.get(i)) {
                            let normal = (b - a).cross(c - a).normalize_or(Vec3::Z);
                            triangle(&mut triangles, [a, b, c], [normal; 3], color(*name));
                        }
                    }
                }
            }
        }
        let points: Vec<Vec3> = triangles
            .iter()
            .map(Vertex::position)
            .chain(lines.iter().flat_map(|&(from, to, _)| [from, to]))
            .collect();
        let Some(mut fit) = Fit::new(&points, camera, frame) else {
            return;
        };
        fit.zoom *= view.zoom;
        let radius = line * SCENE_LINE / 2.0 / fit.zoom;
        for (from, to, color) in lines {
            cylinder(&mut triangles, (from, to), radius, ROD_SEGMENTS, color);
        }
        let view = look(view.camera, fit.center);
        let clip_from_world = self.frame.screen * fit.projection(frame, depth) * view;
        let camera = self.frame.camera(clip_from_world, view);
        self.frame
            .push(Pipeline::Lit, camera, Some(frame), triangles);
    }

    /// Fill a convex polygon.
    fn fill(&mut self, points: &[Vec2], color: [f32; 4]) {
        let Some((&first, rest)) = points.split_first() else {
            return;
        };
        let triangles = rest
            .iter()
            .zip(rest.iter().skip(1))
            .flat_map(|(&a, &b)| [first, a, b])
            .map(|point| Vertex::solid(point, color));
        self.frame.push(Pipeline::Solid, 0, None, triangles);
    }

    /// Stroke a path centered on it, with sharp corners that match Hyprland's borders.
    fn stroke(&mut self, points: &[Vec2], closed: bool, width: f32, color: [f32; 4]) {
        let mut path = points.to_vec();
        path.dedup_by(|a, b| a.distance_squared(*b) < 1e-6);
        let ends = path.first().zip(path.last());
        if closed && path.len() > 2 && ends.is_some_and(|(a, b)| a.distance_squared(*b) < 1e-6) {
            path.pop();
        }
        let count = path.len();
        if count < 2 {
            return;
        }
        let half = width / 2.0;
        // Points wrap around, which closes the path.
        let at = |i: usize| path.get(i % count).copied().unwrap_or_default();
        let normal = |from: usize| (at(from + 1) - at(from)).normalize_or_zero().perp();
        // From the path to the stroke's outer edge at point i: along the miter at corners.
        let offset = |i: usize| {
            let before = (i > 0 || closed).then(|| normal(i + count - 1));
            let after = (i + 1 < count || closed).then(|| normal(i));
            match (before, after) {
                (Some(before), Some(after)) => {
                    let miter = (before + after).normalize_or_zero();
                    miter * half / miter.dot(after).max(1.0 / MITER_LIMIT)
                }
                (Some(side), None) | (None, Some(side)) => side * half,
                (None, None) => Vec2::ZERO,
            }
        };
        let segments = if closed { count } else { count - 1 };
        let triangles = (0..segments)
            .flat_map(|i| {
                let (start, end) = (offset(i), offset(i + 1));
                let (a, b) = (at(i) + start, at(i) - start);
                let (c, d) = (at(i + 1) + end, at(i + 1) - end);
                [a, b, c, b, d, c]
            })
            .map(|point| Vertex::solid(point, color));
        self.frame.push(Pipeline::Solid, 0, None, triangles);
    }

    /// Shape `content` in the style's font at `size`, wrapped to `max_width` logical units.
    fn layout(&mut self, content: &str, weight: Weight, size: f32, max_width: f32) -> Text {
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
    fn write(&mut self, text: &Text, at: Vec2, rgb: Rgb) {
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
        self.frame.push(Pipeline::Glyph, 0, None, triangles);
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
struct Atlas {
    image: Image,
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

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x: x as f32,
        y: y as f32,
        width: width as f32,
        height: height as f32,
    }
}

/// A fill opacity from 0 to 1 as an alpha byte.
fn opacity(fill: Option<f64>, default: f64) -> u8 {
    (fill.unwrap_or(default).clamp(0.0, 1.0) * 255.0).round() as u8
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

/// The outline of a rectangle with circular corners, clockwise. A zero radius gives sharp
/// corners.
fn rounded(rect: Rect, radius: f32) -> Vec<Vec2> {
    let Rect {
        x,
        y,
        width,
        height,
    } = rect;
    let radius = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    let corners = [
        (vec2(x + width - radius, y + radius), -90.0_f32),
        (vec2(x + width - radius, y + height - radius), 0.0),
        (vec2(x + radius, y + height - radius), 90.0),
        (vec2(x + radius, y + radius), 180.0),
    ];
    if radius == 0.0 {
        return corners.map(|(corner, _)| corner).to_vec();
    }
    corners
        .into_iter()
        .flat_map(|(center, start)| {
            (0..=CORNER_SEGMENTS).map(move |step| {
                let angle = start + 90.0 * step as f32 / CORNER_SEGMENTS as f32;
                center + Vec2::from_angle(angle.to_radians()) * radius
            })
        })
        .collect()
}

fn circle(center: Vec2, radius: f32) -> Vec<Vec2> {
    (0..CIRCLE_SEGMENTS)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / CIRCLE_SEGMENTS as f32;
            center + Vec2::from_angle(angle) * radius
        })
        .collect()
}

fn point(p: &Point3) -> Vec3 {
    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

/// A camera circling the scene's center, and the zoom that fills the frame with the scene.
struct Fit {
    /// The point the camera circles: the middle of the objects' bounds.
    center: Vec3,
    /// Eye distance from the center, and the radius that holds every point.
    distance: f32,
    radius: f32,
    /// Center of the perspective image, and logical units per image unit.
    middle: Vec2,
    zoom: f32,
}

impl Fit {
    fn new(points: &[Vec3], camera: Camera, frame: Rect) -> Option<Self> {
        let (low, high) = points
            .iter()
            .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(low, high), &p| {
                (low.min(p), high.max(p))
            });
        if !low.is_finite() || !high.is_finite() {
            return None;
        }
        let center = (low + high) / 2.0;
        let radius = points
            .iter()
            .map(|p| p.distance(center))
            .fold(1e-6, f32::max);
        let distance = EYE_DISTANCE * radius;
        let view = look(camera, center);
        let image = |p: &Vec3| {
            let p = view.transform_point3(*p);
            p.truncate() * distance / (distance - p.z)
        };
        let (min, max) = points
            .iter()
            .map(image)
            .fold((Vec2::INFINITY, Vec2::NEG_INFINITY), |(min, max), p| {
                (min.min(p), max.max(p))
            });
        let margin = SCENE_MARGIN.min(frame.width / 4.0).min(frame.height / 4.0);
        let room = vec2(frame.width, frame.height) - 2.0 * margin;
        let zoom = (room / (max - min).max(Vec2::splat(1e-6))).min_element();
        Some(Self {
            center,
            distance,
            radius,
            middle: (min + max) / 2.0,
            zoom,
        })
    }

    /// Camera space to homogeneous logical screen coordinates: the perspective image,
    /// centered in `frame`, with depth from 0 at the nearest point to 1 at the farthest
    /// mapped into `depth`.
    fn projection(&self, frame: Rect, (near_depth, far_depth): (f32, f32)) -> Mat4 {
        let Self {
            distance,
            radius,
            middle,
            zoom,
            ..
        } = *self;
        // The perspective divide: w is the distance from the eye.
        let w = Vec4::new(0.0, 0.0, -1.0, distance);
        let center = vec2(frame.x + frame.width / 2.0, frame.y + frame.height / 2.0);
        let x = Vec4::new(zoom * distance, 0.0, 0.0, 0.0) + w * (center.x - zoom * middle.x);
        // Screen y points down.
        let y = Vec4::new(0.0, -zoom * distance, 0.0, 0.0) + w * (center.y + zoom * middle.y);
        let (near, far) = (distance - 1.01 * radius, distance + 1.01 * radius);
        let slope = (far_depth - near_depth) * far / (far - near);
        let z = w * (near_depth + slope) - Vec4::new(0.0, 0.0, 0.0, slope * near);
        Mat4::from_cols(x, y, z, w).transpose()
    }
}

/// World to camera space for `camera` circling `center`: x right, y up, z toward the eye.
fn look(camera: Camera, center: Vec3) -> Mat4 {
    Mat4::from_rotation_x((camera.pitch as f32).to_radians())
        * Mat4::from_rotation_y(-(camera.yaw as f32).to_radians())
        * Mat4::from_translation(-center)
}

fn triangle(out: &mut Vec<Vertex>, corners: [Vec3; 3], normals: [Vec3; 3], color: [f32; 4]) {
    out.extend(
        corners
            .into_iter()
            .zip(normals)
            .map(|(corner, normal)| Vertex::lit(corner, normal, color)),
    );
}

/// Two triangles from four corners in order around the quad.
fn quad(out: &mut Vec<Vertex>, [a, b, c, d]: [Vec3; 4], normals: [Vec3; 4], color: [f32; 4]) {
    let [na, nb, nc, nd] = normals;
    triangle(out, [a, b, c], [na, nb, nc], color);
    triangle(out, [a, c, d], [na, nc, nd], color);
}

fn cuboid(out: &mut Vec<Vertex>, center: Vec3, size: Vec3, color: [f32; 4]) {
    let half = size / 2.0;
    for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
        let (u, v) = (
            Vec3::new(axis.z, axis.x, axis.y),
            Vec3::new(axis.y, axis.z, axis.x),
        );
        for normal in [axis, -axis] {
            let middle = center + normal * half;
            let corner = |a: f32, b: f32| middle + u * half * a + v * half * b;
            let corners = [
                corner(-1.0, -1.0),
                corner(1.0, -1.0),
                corner(1.0, 1.0),
                corner(-1.0, 1.0),
            ];
            quad(out, corners, [normal; 4], color);
        }
    }
}

fn sphere(out: &mut Vec<Vertex>, center: Vec3, radius: f32, color: [f32; 4]) {
    let normal = |band: usize, segment: usize| {
        let latitude = std::f32::consts::PI * (band as f32 / SPHERE_BANDS as f32 - 0.5);
        let longitude = std::f32::consts::TAU * segment as f32 / SOLID_SEGMENTS as f32;
        let (lat_sin, lat_cos) = latitude.sin_cos();
        let (lon_sin, lon_cos) = longitude.sin_cos();
        Vec3::new(lat_cos * lon_cos, lat_sin, lat_cos * lon_sin)
    };
    for band in 0..SPHERE_BANDS {
        for segment in 0..SOLID_SEGMENTS {
            let normals = [
                normal(band, segment),
                normal(band, segment + 1),
                normal(band + 1, segment + 1),
                normal(band + 1, segment),
            ];
            quad(out, normals.map(|n| center + n * radius), normals, color);
        }
    }
}

/// A capped round rod between the centers of its ends.
fn cylinder(
    out: &mut Vec<Vertex>,
    (from, to): (Vec3, Vec3),
    radius: f32,
    segments: usize,
    color: [f32; 4],
) {
    let Some(axis) = (to - from).try_normalize() else {
        return;
    };
    let (u, v) = axis.any_orthonormal_pair();
    let around = |i: usize| {
        let (sin, cos) = (std::f32::consts::TAU * i as f32 / segments as f32).sin_cos();
        u * cos + v * sin
    };
    for i in 0..segments {
        let (a, b) = (around(i), around(i + 1));
        let side = [
            from + a * radius,
            from + b * radius,
            to + b * radius,
            to + a * radius,
        ];
        quad(out, side, [a, b, b, a], color);
        triangle(out, [from, side[1], side[0]], [-axis; 3], color);
        triangle(out, [to, side[3], side[2]], [axis; 3], color);
    }
}
