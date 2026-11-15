//! Turns a drawing into draw data for the GPU, in the desktop's style. This file draws
//! boxes and lines and holds the 2D primitives the other parts share.
//!
//! Geometry is in logical units; the screen camera maps it onto output pixels, and 4×
//! multisampling smooths its edges. Every line gets a thin halo in the theme background, so
//! it reads on light and dark windows alike. Lines and scenes come first and text last, so
//! nothing covers words.
//!
//! - [`text`]: labels and text panels.
//! - [`scene`]: 3D scenes, their cameras, and the user's orbit and zoom.
//! - [`chart`]: charts with axes, ticks, and a legend.

use std::ffi::c_void;
use std::ptr::NonNull;

use anyhow::{Context, Result, ensure};
use cosmic_text::{FontSystem, SwashCache};
use glam::{Vec2, vec2};

mod chart;
mod scene;
mod text;

pub(super) use scene::View;
use text::Atlas;

use super::gpu::{Frame, Gpu, Pipeline, Rect, Target, Vertex, premultiply};
use super::model::{Arrows, Chart, Pattern, Rgb, Shape, Style};

const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 32 * 1024 * 1024;
/// Extra width on each side of a line, in the theme background.
const HALO: f32 = 1.5;
const HALO_ALPHA: u8 = 160;
const CORNER_SEGMENTS: usize = 8;
const CIRCLE_SEGMENTS: usize = 32;
/// Sharp corners extend at most this many half widths.
const MITER_LIMIT: f32 = 4.0;

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

    /// A target that draws on the Wayland surface `window` on `display`.
    ///
    /// # Safety
    /// Both must stay valid until the target is dropped.
    pub(crate) unsafe fn target(
        &mut self,
        display: NonNull<c_void>,
        window: NonNull<c_void>,
    ) -> Result<Target> {
        let gpu = match &mut self.gpu {
            Some(gpu) => gpu,
            None => self.gpu.insert(Gpu::new()?),
        };
        // SAFETY: the caller's guarantee.
        unsafe { gpu.target(display, window) }
    }

    /// Paint `shapes` on `target`, transparent elsewhere, at `size` logical units of `scale`
    /// pixels each. Scene i is seen through `views[i]`. Returns false when the surface skipped
    /// the frame.
    ///
    /// # Errors
    /// Fails when the image exceeds the pixel budget or the GPU cannot draw it.
    pub(crate) fn paint(
        &mut self,
        target: &mut Target,
        style: &Style,
        shapes: &[Shape],
        views: &[View],
        size: (u32, u32),
        scale: f64,
    ) -> Result<bool> {
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
            alpha: 1.0,
            clip: None,
        };
        canvas.draw(shapes);
        let mut frame = canvas.frame;
        if canvas.atlas.image.height > 0 {
            frame.atlas = canvas.atlas.image;
        }
        let gpu = self.gpu.as_ref().context("The target outlived its GPU")?;
        gpu.render(&frame, target)
    }
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
    /// The opacity of the shape being drawn.
    alpha: f32,
    /// Where 2D drawing shows, when not the whole screen.
    clip: Option<Rect>,
}

impl Canvas<'_> {
    fn draw(&mut self, shapes: &[Shape]) {
        let style = self.style;
        let palette = &style.palette;
        let halo = premultiply(palette.background, HALO_ALPHA);
        let scenes = shapes
            .iter()
            .filter(|shape| shape.orbit().is_some())
            .count();
        let mut scene_index = 0;
        for shape in shapes {
            self.alpha = shape.opacity() as f32;
            let (line, fill) = (shape.line_width(style) as f32, opacity(shape.fill()));
            // Each scene and 3D chart takes its view and a depth slice, later ones nearer.
            let orbit = shape.orbit().map(|_| {
                let index = scene_index;
                scene_index += 1;
                let slice = |index: usize| 1.0 - index as f32 / scenes as f32;
                let view = self.views.get(index).copied();
                let view = view.unwrap_or_else(|| View::new(&[], &[]));
                ((slice(index + 1), slice(index)), view)
            });
            match shape {
                Shape::Box {
                    x,
                    y,
                    width,
                    height,
                    color,
                    border,
                    ..
                } => {
                    let outline = rounded(rect(*x, *y, *width, *height), style.rounding as f32);
                    let rgb = palette.rgb(*color);
                    self.fill(&outline, premultiply(rgb, fill));
                    if *border {
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
                    arrows,
                    pattern,
                    ..
                } => {
                    let ends = (vec2(*x as f32, *y as f32), vec2(*to_x as f32, *to_y as f32));
                    self.line(ends, palette.rgb(*color), line, *arrows, *pattern);
                }
                Shape::Text { .. } => {}
                Shape::Scene {
                    x,
                    y,
                    width,
                    height,
                    camera,
                    objects,
                    border,
                    ..
                } => {
                    let frame = rect(*x, *y, *width, *height);
                    let outline = rounded(frame, style.rounding as f32);
                    self.fill(&outline, premultiply(palette.background, fill));
                    if *border {
                        self.stroke(&outline, true, line, premultiply(palette.accent, 255));
                    }
                    if let Some((depth, view)) = orbit {
                        self.scene(objects, (*camera, view), frame, depth, line);
                    }
                }
                Shape::Chart(chart) => self.chart(chart, orbit),
            }
        }
        for shape in shapes {
            self.alpha = shape.opacity() as f32;
            let line = shape.line_width(style) as f32;
            match shape {
                Shape::Box {
                    x,
                    y,
                    width,
                    height,
                    label: Some(label),
                    color,
                    ..
                } => {
                    let frame = rect(*x, *y, *width, *height);
                    self.tag(label, frame, palette.rgb(*color), line);
                }
                Shape::Scene {
                    x,
                    y,
                    width,
                    height,
                    label: Some(label),
                    ..
                }
                | Shape::Chart(Chart {
                    x,
                    y,
                    width,
                    height,
                    label: Some(label),
                    ..
                }) => {
                    let frame = rect(*x, *y, *width, *height);
                    self.tag(label, frame, palette.accent, line);
                }
                Shape::Text {
                    x,
                    y,
                    text,
                    color,
                    border,
                    ..
                } => {
                    let size = shape.text_size(style) as f32;
                    let panel = (opacity(shape.fill()), *border);
                    let at = vec2(*x as f32, *y as f32);
                    self.panel(text, at, palette.rgb(*color), size, panel);
                }
                _ => {}
            }
        }
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

    /// Add triangles at the current shape's opacity.
    fn push(
        &mut self,
        pipeline: Pipeline,
        camera: u32,
        clip: Option<Rect>,
        triangles: impl IntoIterator<Item = Vertex>,
    ) {
        let alpha = self.alpha;
        if alpha > 0.0 {
            let faded = triangles.into_iter().map(|vertex| vertex.faded(alpha));
            self.frame.push(pipeline, camera, clip, faded);
        }
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
        self.push(Pipeline::Solid, 0, self.clip, triangles);
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
        self.push(Pipeline::Solid, 0, self.clip, triangles);
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
fn opacity(fill: f64) -> u8 {
    (fill.clamp(0.0, 1.0) * 255.0).round() as u8
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
