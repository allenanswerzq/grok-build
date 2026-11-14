//! Charts: a backdrop, a legend, axes with ticks and gridlines, and the data.
//!
//! A 2D plot reserves room for its legend above, tick labels to the left and below, and axis
//! names beside them, then draws its data in what remains, clipped to it. A 3D plot draws its
//! data in a box of fixed size that fits the frame from any side, over a floor grid and two
//! back walls of value lines; its labels go where their 3D anchors land on the screen.
//! A sankey plot fills its area with columns of nodes joined by bands.

use std::f32::consts::{FRAC_PI_2, TAU};

use cosmic_text::Weight;
use glam::{Vec2, Vec3, vec2, vec3};

use super::{
    Canvas, Fit, LINE_HEIGHT, Pipeline, Rect, Vertex, View, WRAP_EMS, circle, cuboid, cylinder,
    look, opacity, premultiply, rect, rounded, sphere,
};
use crate::implementations::grok_build::screen_draw::chart::Scale;
use crate::implementations::grok_build::screen_draw::{
    Axis, Chart, Palette, Plot, Rgb, SCENE_FILL,
};

/// Space inside the frame, and between labels and what they name, in logical units.
const PADDING: f32 = 14.0;
const GAP: f32 = 6.0;
/// Tick labels and legend names, relative to the style's font size.
const SMALL: f32 = 0.85;
const SWATCH: f32 = 10.0;
const RAMP_WIDTH: f32 = 120.0;
const GRID_ALPHA: u8 = 45;
const AXIS_ALPHA: u8 = 140;
const BAR_ALPHA: u8 = 235;
const AREA_ALPHA: u8 = 80;
const POINT_ALPHA: u8 = 220;
/// Point markers' radius in line widths, and the most points a line marks.
const MARKER: f32 = 1.6;
const MARKED_POINTS: usize = 40;
/// A candle's body as a share of its category's band.
const CANDLE: f64 = 0.6;
/// Sankey nodes' width and the space between nodes in a column, in logical units, the
/// opacity of the bands between them, and the steps along each band's curve.
const NODE_WIDTH: f32 = 12.0;
const NODE_GAP: f32 = 12.0;
const BAND_ALPHA: u8 = 90;
const BAND_STEPS: usize = 32;
/// Half the 3D box's width, height, and depth. The ball it fits in leaves room for labels.
const BOX: Vec3 = vec3(1.0, 0.65, 1.0);
const LABEL_ROOM: f32 = 1.2;
/// How far 3D labels sit outside the box, in box units.
const OUTSIDE: f32 = 0.12;
const MARKER_RADIUS: f32 = 0.035;
const MARKER_SEGMENTS: usize = 10;
const RULE_SEGMENTS: usize = 6;

/// Where each mark falls along an axis, from 0 to 1, and its label.
type Marks = Vec<(f64, String)>;

impl Canvas<'_> {
    /// A chart in its frame. A 3D plot is seen through `orbit`: its depth slice and view.
    pub(super) fn chart(&mut self, chart: &Chart, orbit: Option<((f32, f32), View)>) {
        let style = self.style;
        let frame = rect(chart.x, chart.y, chart.width, chart.height);
        let outline = rounded(frame, style.rounding as f32);
        let backdrop = opacity(chart.fill, SCENE_FILL);
        self.fill(&outline, premultiply(style.palette.background, backdrop));
        let line = chart.line_width.unwrap_or(style.line_width) as f32;
        if chart.border {
            self.stroke(&outline, true, line, premultiply(style.palette.accent, 255));
        }
        let padding = PADDING.min(frame.width / 4.0).min(frame.height / 4.0);
        let inner = Rect {
            x: frame.x + padding,
            y: frame.y + padding,
            width: frame.width - 2.0 * padding,
            height: frame.height - 2.0 * padding,
        };
        self.clip = Some(frame);
        let area = self.legend(chart, inner);
        match (chart.plot, orbit) {
            (Plot::Pie | Plot::Donut, _) => self.pie(chart, area),
            (Plot::Heatmap, _) => self.heatmap(chart, area),
            (Plot::Sankey, _) => self.sankey(chart, area),
            (_, Some(orbit)) => self.solid(chart, area, frame, orbit),
            _ => self.plane(chart, area, line),
        }
        self.clip = None;
    }

    /// The legend across the top of `area`: a swatch and name for each named series or pie
    /// slice, or the color scale of a grid that shades by value. Returns the area below it.
    fn legend(&mut self, chart: &Chart, area: Rect) -> Rect {
        let palette = &self.style.palette;
        let row = self.small() * LINE_HEIGHT;
        let shaded = chart.plot.has_grid() && chart.series.iter().all(|s| s.color.is_none());
        let entries: Vec<(Vec<Rgb>, String)> = match chart.plot {
            _ if shaded => {
                let [_, values, _] = chart.scales();
                let (low, high) = (
                    number(values.min, values.step),
                    number(values.max, values.step),
                );
                let shades = (0..24).map(|step| ramp(palette, f64::from(step) / 23.0));
                vec![(shades.collect(), format!("{low} to {high}"))]
            }
            Plot::Pie | Plot::Donut => (chart.categories.iter().enumerate())
                .map(|(index, name)| (vec![hue(palette, index)], name.clone()))
                .collect(),
            // Candles show rises and falls by color, and sankey nodes carry their names.
            Plot::Candlestick | Plot::Sankey => Vec::new(),
            _ => (0..chart.series.len())
                .filter_map(|index| {
                    let name = chart.series[index].name.clone()?;
                    Some((vec![series_rgb(chart, palette, index)], name))
                })
                .collect(),
        };
        if entries.is_empty() {
            return area;
        }
        let (mut x, mut y) = (area.x, area.y);
        for (colors, name) in entries {
            let text = self.text(&name);
            let swatch = if colors.len() > 1 { RAMP_WIDTH } else { SWATCH };
            let width = swatch + GAP + text.size.x;
            if x > area.x && x + width > area.x + area.width {
                (x, y) = (area.x, y + row);
            }
            let step = swatch / colors.len() as f32;
            for (index, rgb) in colors.into_iter().enumerate() {
                let shade = Rect {
                    x: x + step * index as f32,
                    y: y + (row - SWATCH) / 2.0,
                    width: step + 0.5,
                    height: SWATCH,
                };
                self.fill(
                    &rounded(shade, 2.0_f32.min(step / 2.0)),
                    premultiply(rgb, 255),
                );
            }
            self.write(
                &text,
                vec2(x + swatch + GAP, y),
                self.style.palette.foreground,
            );
            x += width + 2.0 * GAP;
        }
        let top = y + row + GAP;
        Rect {
            y: top,
            height: area.height - (top - area.y),
            ..area
        }
    }

    /// Bar, line, area, and scatter plots on a value axis up and a category or number axis
    /// across.
    fn plane(&mut self, chart: &Chart, area: Rect, line: f32) {
        let palette = &self.style.palette;
        let row = self.small() * LINE_HEIGHT;
        let [xs, ys, _] = chart.scales();
        let numeric = chart.plot != Plot::Candlestick
            && chart.series.iter().any(|series| !series.points.is_empty());
        let count = chart.categories.len();
        let x_marks = if numeric {
            ticks(xs)
        } else {
            slots(&chart.categories, &Axis::default(), count, true)
        };
        let y_marks = ticks(ys);
        let left = (y_marks.iter())
            .map(|(_, label)| self.text(label).size.x)
            .fold(0.0, f32::max);
        let named = |axis: &Axis| if axis.label.is_some() { row + GAP } else { 0.0 };
        // Half a row above and beside the plot holds the end ticks' labels.
        let top = named(&chart.y_axis) + row / 2.0;
        let bottom = row + GAP + named(&chart.x_axis);
        let right = if numeric { row } else { 0.0 };
        let plot = Rect {
            x: area.x + left + GAP,
            y: area.y + top,
            width: area.width - left - GAP - right,
            height: area.height - top - bottom,
        };
        if plot.width < 20.0 || plot.height < 20.0 {
            return;
        }
        let at = |u: f64, v: f64| {
            vec2(
                plot.x + plot.width * u as f32,
                plot.y + plot.height * (1.0 - v as f32),
            )
        };
        let grid = premultiply(palette.foreground, GRID_ALPHA);
        for &(v, _) in &y_marks {
            self.stroke(&[at(0.0, v), at(1.0, v)], false, 1.0, grid);
        }
        for &(u, _) in x_marks.iter().filter(|_| numeric) {
            self.stroke(&[at(u, 0.0), at(u, 1.0)], false, 1.0, grid);
        }
        let base = ys.place(0.0).clamp(0.0, 1.0);
        let axis = premultiply(palette.foreground, AXIS_ALPHA);
        self.stroke(&[at(0.0, base), at(1.0, base)], false, 1.0, axis);
        let ink = palette.foreground;
        let y_labels = y_marks
            .into_iter()
            .map(|(v, label)| (at(0.0, v) - vec2(GAP, 0.0), label));
        self.labels(y_labels, vec2(1.0, 0.5), ink);
        let x_labels = x_marks
            .into_iter()
            .map(|(u, label)| (at(u, 0.0) + vec2(0.0, GAP), label));
        self.labels(x_labels, vec2(0.5, 0.0), ink);
        if let Some(label) = &chart.y_axis.label {
            self.labels([(vec2(area.x, area.y), label.clone())], Vec2::ZERO, ink);
        }
        if let Some(label) = &chart.x_axis.label {
            let middle = vec2(plot.x + plot.width / 2.0, area.y + area.height);
            self.labels([(middle, label.clone())], vec2(0.5, 1.0), ink);
        }
        self.clip = Some(plot);
        let stacks = chart.stacks();
        let groups = if chart.stacked { 1 } else { chart.series.len() };
        let band = 1.0 / count.max(1) as f64;
        let rounding = (self.style.rounding as f32).min(3.0);
        for (index, series) in chart.series.iter().enumerate() {
            let rgb = series_rgb(chart, palette, index);
            let solid = premultiply(rgb, 255);
            let spans = stacks.get(index).map_or(&[][..], Vec::as_slice);
            match chart.plot {
                Plot::Bar => {
                    let width = band * 0.8 / groups as f64;
                    let slot = if chart.stacked { 0 } else { index };
                    for (category, &(from, to)) in spans.iter().enumerate() {
                        let u = band * (category as f64 + 0.1) + width * slot as f64;
                        let (a, b) = (at(u, ys.place(from)), at(u + width, ys.place(to)));
                        let gap = ((b.x - a.x) * 0.08).min(2.0);
                        let bar = Rect {
                            x: a.x + gap / 2.0,
                            y: a.y.min(b.y),
                            width: b.x - a.x - gap,
                            height: (a.y - b.y).abs(),
                        };
                        self.fill(&rounded(bar, rounding), premultiply(rgb, BAR_ALPHA));
                    }
                }
                Plot::Line | Plot::Area => {
                    let (tops, bases): (Vec<Vec2>, Vec<Vec2>) = if series.points.is_empty() {
                        let u = |category: usize| band * (category as f64 + 0.5);
                        (spans.iter().enumerate())
                            .map(|(c, &(from, to))| {
                                (at(u(c), ys.place(to)), at(u(c), ys.place(from)))
                            })
                            .unzip()
                    } else {
                        (series.points.iter())
                            .map(|p| (at(xs.place(p[0]), ys.place(p[1])), at(xs.place(p[0]), base)))
                            .unzip()
                    };
                    if chart.plot == Plot::Area {
                        for (top, bottom) in tops.windows(2).zip(bases.windows(2)) {
                            let quad = [bottom[0], top[0], top[1], bottom[1]];
                            self.fill(&quad, premultiply(rgb, AREA_ALPHA));
                        }
                    }
                    self.stroke(&tops, false, line * 1.5, solid);
                    for &point in tops.iter().filter(|_| tops.len() <= MARKED_POINTS) {
                        let halo = premultiply(palette.background, 255);
                        self.fill(&circle(point, line * MARKER + 1.5), halo);
                        self.fill(&circle(point, line * MARKER), solid);
                    }
                }
                Plot::Candlestick => {
                    let (rise, fall) = (
                        premultiply(palette.green, 255),
                        premultiply(palette.red, 255),
                    );
                    for (category, candle) in series.points.iter().enumerate() {
                        let [open, high, low, close] = [0, 1, 2, 3].map(|i| ys.place(candle[i]));
                        let color = if close >= open { rise } else { fall };
                        let middle = band * (category as f64 + 0.5);
                        let wick = [at(middle, high), at(middle, low)];
                        self.stroke(&wick, false, (line / 2.0).max(1.0), color);
                        let half = band * CANDLE / 2.0;
                        let (a, b) = (at(middle - half, open), at(middle + half, close));
                        let body = Rect {
                            x: a.x,
                            y: a.y.min(b.y),
                            width: b.x - a.x,
                            height: (a.y - b.y).abs().max(1.0),
                        };
                        self.fill(&rounded(body, rounding.min(body.width / 4.0)), color);
                    }
                }
                _ => {
                    for point in &series.points {
                        let center = at(xs.place(point[0]), ys.place(point[1]));
                        self.fill(
                            &circle(center, line * MARKER + 1.0),
                            premultiply(rgb, POINT_ALPHA),
                        );
                    }
                }
            }
        }
        self.clip = None;
    }

    /// Slices clockwise from the top, each labeled with its share when there is room.
    fn pie(&mut self, chart: &Chart, area: Rect) {
        let palette = &self.style.palette;
        let values = chart
            .series
            .first()
            .map_or(&[][..], |series| &series.values);
        let total: f64 = values.iter().sum();
        let center = vec2(area.x + area.width / 2.0, area.y + area.height / 2.0);
        let radius = (area.width.min(area.height) / 2.0 - GAP).max(1.0);
        let hole = if chart.plot == Plot::Donut {
            radius * 0.58
        } else {
            0.0
        };
        let point = |angle: f32, distance: f32| center + Vec2::from_angle(angle) * distance;
        let backdrop = premultiply(palette.background, 255);
        let (mut start, mut shares) = (-FRAC_PI_2, Vec::new());
        for (index, &value) in values.iter().enumerate() {
            let share = value / total;
            let sweep = TAU * share as f32;
            let steps = ((share * 128.0).ceil() as usize).max(1);
            let color = premultiply(hue(palette, index), 255);
            let angle = |step: usize| start + sweep * step as f32 / steps as f32;
            for step in 0..steps {
                let (a, b) = (angle(step), angle(step + 1));
                let slice = [
                    point(a, hole),
                    point(a, radius),
                    point(b, radius),
                    point(b, hole),
                ];
                self.fill(&slice, color);
            }
            if share >= 0.04 {
                let middle = point(start + sweep / 2.0, (hole.max(radius * 0.3) + radius) / 2.0);
                shares.push((middle, format!("{:.0}%", share * 100.0)));
            }
            start += sweep;
        }
        let mut angle = -FRAC_PI_2;
        for &value in values.iter().filter(|_| values.len() > 1) {
            self.stroke(
                &[point(angle, hole), point(angle, radius)],
                false,
                2.0,
                backdrop,
            );
            angle += TAU * (value / total) as f32;
        }
        self.labels(shares, vec2(0.5, 0.5), palette.background);
    }

    /// Nodes as bars in columns, as tall as their flow, joined by bands as thick as each
    /// link's value in the color of the node it leaves. One scale fits the fullest column;
    /// each column's nodes stack in the order links first name them, centered.
    fn sankey(&mut self, chart: &Chart, area: Rect) {
        let palette = &self.style.palette;
        let Some(nodes) = chart.nodes() else {
            return;
        };
        let columns = nodes.iter().map(|node| node.column).max().unwrap_or(0) + 1;
        let members: Vec<Vec<usize>> = (0..columns)
            .map(|column| {
                (0..nodes.len())
                    .filter(|&i| nodes[i].column == column)
                    .collect()
            })
            .collect();
        let gaps = |count: usize| NODE_GAP * count.saturating_sub(1) as f32;
        let scale = (members.iter())
            .map(|column| {
                let total: f64 = column.iter().map(|&i| nodes[i].value).sum();
                (area.height - gaps(column.len())).max(1.0) / total as f32
            })
            .fold(f32::INFINITY, f32::min);
        let height = |i: usize| nodes[i].value as f32 * scale;
        let x = |column: usize| {
            area.x + (area.width - NODE_WIDTH) * column as f32 / (columns - 1).max(1) as f32
        };
        let mut tops = vec![0.0; nodes.len()];
        for column in &members {
            let used = column.iter().map(|&i| height(i)).sum::<f32>() + gaps(column.len());
            let mut y = area.y + (area.height - used) / 2.0;
            for &i in column {
                tops[i] = y;
                y += height(i) + NODE_GAP;
            }
        }
        // Each node's bands leave from its top down in the order of their targets, and arrive
        // in the order of their sources, so bands cross as little as their nodes allow.
        let index = |name: &str| nodes.iter().position(|node| node.name == name).unwrap_or(0);
        let ends: Vec<(usize, usize, f32)> = (chart.links.iter())
            .map(|link| {
                (
                    index(&link.from),
                    index(&link.to),
                    link.value as f32 * scale,
                )
            })
            .collect();
        let mut order: Vec<usize> = (0..ends.len()).collect();
        let (mut leaving, mut arriving) = (tops.clone(), tops.clone());
        let mut starts = vec![(0.0, 0.0); ends.len()];
        order.sort_by(|&a, &b| tops[ends[a].1].total_cmp(&tops[ends[b].1]));
        for &link in &order {
            let (from, _, thickness) = ends[link];
            starts[link].0 = leaving[from];
            leaving[from] += thickness;
        }
        order.sort_by(|&a, &b| tops[ends[a].0].total_cmp(&tops[ends[b].0]));
        for &link in &order {
            let (_, to, thickness) = ends[link];
            starts[link].1 = arriving[to];
            arriving[to] += thickness;
        }
        for (&(from, to, thickness), &(top, bottom)) in ends.iter().zip(&starts) {
            let (left, right) = (x(nodes[from].column) + NODE_WIDTH, x(nodes[to].column));
            let curve: Vec<Vec2> = (0..=BAND_STEPS)
                .map(|step| {
                    let t = step as f32 / BAND_STEPS as f32;
                    let eased = t * t * (3.0 - 2.0 * t);
                    vec2(left + (right - left) * t, top + (bottom - top) * eased)
                })
                .collect();
            let color = premultiply(hue(palette, from), BAND_ALPHA);
            let below = vec2(0.0, thickness);
            for pair in curve.windows(2) {
                self.fill(&[pair[0], pair[1], pair[1] + below, pair[0] + below], color);
            }
        }
        let ink = palette.foreground;
        let mut taken = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            let bar = Rect {
                x: x(node.column),
                y: tops[i],
                width: NODE_WIDTH,
                height: height(i).max(1.0),
            };
            self.fill(&rounded(bar, 2.0), premultiply(hue(palette, i), 255));
            let step = if node.value.fract() == 0.0 { 1.0 } else { 0.1 };
            let label = format!("{}  {}", node.name, number(node.value, step));
            let middle = vec2(0.0, bar.y + bar.height / 2.0);
            let (at, anchor) = if node.column + 1 == columns {
                (middle + vec2(bar.x - GAP, 0.0), vec2(1.0, 0.5))
            } else {
                (middle + vec2(bar.x + NODE_WIDTH + GAP, 0.0), vec2(0.0, 0.5))
            };
            self.label(&mut taken, at, anchor, &label, ink);
        }
    }

    /// A grid of cells shaded by value: columns across and rows down, labeled on the left
    /// and below, with each value written in its cell when they fit.
    fn heatmap(&mut self, chart: &Chart, area: Rect) {
        let palette = &self.style.palette;
        let size = self.small();
        let row = size * LINE_HEIGHT;
        let grid = chart.grid();
        let (rows, columns) = (grid.len(), grid.first().map_or(0, Vec::len));
        let [_, values, _] = chart.scales();
        let row_marks = slots(&chart.rows, &chart.z_axis, rows, true);
        let column_marks = slots(&chart.categories, &chart.x_axis, columns, true);
        let left = (row_marks.iter())
            .map(|(_, label)| self.text(label).size.x)
            .fold(0.0, f32::max);
        let named = |axis: &Axis| if axis.label.is_some() { row + GAP } else { 0.0 };
        let top = named(&chart.z_axis);
        let plot = Rect {
            x: area.x + left + GAP,
            y: area.y + top,
            width: area.width - left - GAP,
            height: area.height - top - row - GAP - named(&chart.x_axis),
        };
        if plot.width < 20.0 || plot.height < 20.0 || columns == 0 {
            return;
        }
        let (width, height) = (plot.width / columns as f32, plot.height / rows as f32);
        let gap = (width.min(height) * 0.08).min(1.5);
        let written = rows * columns <= 400 && width >= size * 3.5 && height >= row;
        let mut numbers = Vec::new();
        for (r, cells) in grid.iter().enumerate() {
            for (c, &value) in cells.iter().enumerate() {
                let corner = vec2(plot.x + width * c as f32, plot.y + height * r as f32);
                let cell = Rect {
                    x: corner.x + gap / 2.0,
                    y: corner.y + gap / 2.0,
                    width: width - gap,
                    height: height - gap,
                };
                let shade = ramp(palette, values.place(value));
                self.fill(&rounded(cell, gap * 2.0), premultiply(shade, 255));
                if written {
                    let middle = corner + vec2(width, height) / 2.0;
                    numbers.push((middle, number(value, values.step / 10.0)));
                }
            }
        }
        self.labels(numbers, vec2(0.5, 0.5), palette.background);
        let ink = palette.foreground;
        let at_row = |v: f64| vec2(plot.x - GAP, plot.y + plot.height * v as f32);
        self.labels(
            row_marks.into_iter().map(|(v, label)| (at_row(v), label)),
            vec2(1.0, 0.5),
            ink,
        );
        let at_column = |u: f64| vec2(plot.x + plot.width * u as f32, plot.y + plot.height + GAP);
        let labels = column_marks
            .into_iter()
            .map(|(u, label)| (at_column(u), label));
        self.labels(labels, vec2(0.5, 0.0), ink);
        if let Some(label) = &chart.z_axis.label {
            self.labels([(vec2(area.x, area.y), label.clone())], Vec2::ZERO, ink);
        }
        if let Some(label) = &chart.x_axis.label {
            let middle = vec2(plot.x + plot.width / 2.0, area.y + area.height);
            self.labels([(middle, label.clone())], vec2(0.5, 1.0), ink);
        }
    }

    /// A 3D plot in a box that the camera circles, fitted in `area` and clipped to `frame`.
    fn solid(&mut self, chart: &Chart, area: Rect, frame: Rect, (depth, view): ((f32, f32), View)) {
        let palette = &self.style.palette;
        let mut fit = Fit::around(Vec3::ZERO, BOX.length() * LABEL_ROOM, area);
        fit.zoom *= view.zoom;
        let eye = look(view.camera.unwrap_or(chart.camera), fit.center);
        let world = fit.projection(area, depth) * eye;
        let camera = self.frame.camera(self.frame.screen * world, eye);
        // A point from 0 to 1 along each axis to the box, and a box point to the screen.
        let at =
            |u: f64, v: f64, w: f64| vec3(u as f32, v as f32, w as f32).mul_add(2.0 * BOX, -BOX);
        let screen = |point: Vec3| world.project_point3(point).truncate();
        let [xs, ys, zs] = chart.scales();
        let grid = chart.grid();
        let (rows, columns) = (grid.len(), grid.first().map_or(0, Vec::len));
        let centered = chart.plot == Plot::Bar3d;
        let (x_marks, z_marks) = if chart.plot == Plot::Scatter3d {
            (ticks(xs), ticks(zs))
        } else {
            (
                slots(&chart.categories, &chart.x_axis, columns, centered),
                slots(&chart.rows, &chart.z_axis, rows, centered),
            )
        };
        let y_marks = ticks(ys);
        let mut triangles = Vec::new();
        let faint = premultiply(mix(palette.foreground, palette.background, 0.6), 255);
        let rod = 0.5 / fit.zoom;
        let mut rules = vec![
            (at(0.0, 0.0, 0.0), at(1.0, 0.0, 0.0)),
            (at(1.0, 0.0, 0.0), at(1.0, 0.0, 1.0)),
            (at(1.0, 0.0, 1.0), at(0.0, 0.0, 1.0)),
            (at(0.0, 0.0, 1.0), at(0.0, 0.0, 0.0)),
            (at(0.0, 0.0, 0.0), at(0.0, 1.0, 0.0)),
        ];
        rules.extend(
            x_marks
                .iter()
                .map(|&(u, _)| (at(u, 0.0, 0.0), at(u, 0.0, 1.0))),
        );
        rules.extend(
            z_marks
                .iter()
                .map(|&(w, _)| (at(0.0, 0.0, w), at(1.0, 0.0, w))),
        );
        for &(v, _) in &y_marks {
            rules.extend([
                (at(0.0, v, 0.0), at(1.0, v, 0.0)),
                (at(0.0, v, 0.0), at(0.0, v, 1.0)),
            ]);
        }
        for ends in rules {
            cylinder(&mut triangles, ends, rod, RULE_SEGMENTS, faint);
        }
        let shade = |value: f64| {
            let fixed = chart.series.first().and_then(|series| series.color);
            let rgb = fixed.map_or_else(|| ramp(palette, ys.place(value)), |c| palette.rgb(c));
            premultiply(rgb, 255)
        };
        let height = |value: f64| ys.place(value).clamp(0.0, 1.0);
        match chart.plot {
            Plot::Scatter3d => {
                for (index, series) in chart.series.iter().enumerate() {
                    let color = premultiply(series_rgb(chart, palette, index), 255);
                    for point in &series.points {
                        let place = [xs.place(point[0]), ys.place(point[1]), zs.place(point[2])];
                        if place.iter().all(|share| (-1e-3..=1.001).contains(share)) {
                            let center = at(place[0], place[1], place[2]);
                            sphere(
                                &mut triangles,
                                center,
                                MARKER_RADIUS,
                                MARKER_SEGMENTS,
                                color,
                            );
                        }
                    }
                }
            }
            Plot::Bar3d => {
                let base = height(0.0);
                let cell = vec3(2.0 / columns as f32, 0.0, 2.0 / rows as f32) * BOX * 0.7;
                for (r, cells) in grid.iter().enumerate() {
                    for (c, &value) in cells.iter().enumerate() {
                        let (u, w) = (
                            (c as f64 + 0.5) / columns as f64,
                            (r as f64 + 0.5) / rows as f64,
                        );
                        let (low, high) = (
                            at(u, base.min(height(value)), w),
                            at(u, base.max(height(value)), w),
                        );
                        if high.y - low.y > 1e-4 {
                            let size = cell + Vec3::Y * (high.y - low.y);
                            cuboid(&mut triangles, (low + high) / 2.0, size, shade(value));
                        }
                    }
                }
            }
            Plot::Surface => {
                let last = |count: usize| (count - 1) as f64;
                let point = |r: usize, c: usize| {
                    at(
                        c as f64 / last(columns),
                        height(grid[r][c]),
                        r as f64 / last(rows),
                    )
                };
                // Up the slope: across the rows, crossed with along them.
                let normal = |r: usize, c: usize| {
                    let along = point(r, (c + 1).min(columns - 1)) - point(r, c.saturating_sub(1));
                    let across = point((r + 1).min(rows - 1), c) - point(r.saturating_sub(1), c);
                    across.cross(along).normalize_or(Vec3::Y)
                };
                let vertex = |(r, c): (usize, usize)| {
                    Vertex::lit(point(r, c), normal(r, c), shade(grid[r][c]))
                };
                for r in 0..rows - 1 {
                    for c in 0..columns - 1 {
                        let [a, b, d, e] =
                            [(r, c), (r, c + 1), (r + 1, c + 1), (r + 1, c)].map(vertex);
                        triangles.extend([a, b, d, a, d, e]);
                    }
                }
            }
            _ => {}
        }
        self.push(Pipeline::Lit, camera, Some(frame), triangles);
        let ink = palette.foreground;
        let outside = |point: Vec3, toward: Vec3| screen(point + toward * OUTSIDE);
        // The axes meet at the box's corners, so all their labels share one set of taken areas.
        let mut taken = Vec::new();
        let corner = vec3(-0.5, 0.0, 0.5);
        let marks = (x_marks.into_iter())
            .map(|(u, label)| (outside(at(u, 0.0, 1.0), Vec3::Z), vec2(0.5, 0.0), label))
            .chain(
                z_marks
                    .into_iter()
                    .map(|(w, label)| (outside(at(1.0, 0.0, w), Vec3::X), vec2(0.0, 0.5), label)),
            )
            .chain(
                y_marks
                    .into_iter()
                    .map(|(v, label)| (outside(at(0.0, v, 1.0), corner), vec2(1.0, 0.5), label)),
            );
        let names = [
            (
                &chart.x_axis,
                outside(at(0.5, 0.0, 1.0), Vec3::Z * 3.5),
                vec2(0.5, 0.0),
            ),
            (
                &chart.z_axis,
                outside(at(1.0, 0.0, 0.5), Vec3::X * 3.5),
                vec2(0.0, 0.5),
            ),
            (
                &chart.y_axis,
                outside(at(0.0, 1.0, 1.0), Vec3::Y),
                vec2(0.5, 1.0),
            ),
        ];
        let names = names
            .into_iter()
            .filter_map(|(axis, at, anchor)| Some((at, anchor, axis.label.clone()?)));
        for (at, anchor, label) in marks.chain(names) {
            self.label(&mut taken, at, anchor, &label, ink);
        }
    }

    fn small(&self) -> f32 {
        self.style.font_size as f32 * SMALL
    }

    /// Text in the small size.
    fn text(&mut self, content: &str) -> super::Text {
        let size = self.small();
        self.layout(content, Weight::NORMAL, size, WRAP_EMS * size)
    }

    /// Write each label in the small size with `anchor` (0 to 1 across and down the text) on
    /// its point, skipping any that would overlap one already written.
    fn labels(&mut self, labels: impl IntoIterator<Item = (Vec2, String)>, anchor: Vec2, rgb: Rgb) {
        let mut taken = Vec::new();
        for (at, content) in labels {
            self.label(&mut taken, at, anchor, &content, rgb);
        }
    }

    /// Write one label unless it would overlap an area in `taken`, which then gains its own.
    fn label(&mut self, taken: &mut Vec<Rect>, at: Vec2, anchor: Vec2, content: &str, rgb: Rgb) {
        let text = self.text(content);
        let corner = at - anchor * text.size;
        let area = Rect {
            x: corner.x - GAP / 2.0,
            y: corner.y,
            width: text.size.x + GAP,
            height: text.size.y,
        };
        let overlaps = |other: &Rect| {
            area.x < other.x + other.width
                && other.x < area.x + area.width
                && area.y < other.y + other.height
                && other.y < area.y + area.height
        };
        if !taken.iter().any(overlaps) {
            taken.push(area);
            self.write(&text, corner, rgb);
        }
    }
}

/// Labeled ticks of a numeric scale.
fn ticks(scale: Scale) -> Marks {
    let ticks = scale.ticks();
    ticks
        .map(|value| (scale.place(value), number(value, scale.step)))
        .collect()
}

/// Marks for `count` grid columns or rows, or categories: their names, or else numbers along
/// the span `axis` sets. Centered marks sit in the middle of equal bands; others run from
/// edge to edge.
fn slots(names: &[String], axis: &Axis, count: usize, centered: bool) -> Marks {
    let place = |index: f64| {
        if centered {
            (index + 0.5) / count as f64
        } else {
            index / (count.max(2) - 1) as f64
        }
    };
    if !names.is_empty() {
        let names = names.iter().enumerate();
        return names
            .map(|(index, name)| (place(index as f64), name.clone()))
            .collect();
    }
    let span = Chart::span(axis, count);
    if count < 2 {
        return vec![(place(0.0), number(span.min + 0.5, 1.0))];
    }
    let index = |value: f64| span.place(value) * (count - 1) as f64;
    let ticks = span.ticks();
    ticks
        .map(|value| (place(index(value)), number(value, span.step)))
        .collect()
}

/// `value` with the decimals `step` needs, in thousands (k), millions (M), or billions (B)
/// when the step is that large.
fn number(value: f64, step: f64) -> String {
    let units = [(1e9, "B"), (1e6, "M"), (1e3, "k")];
    let (size, unit) = units
        .into_iter()
        .find(|&(size, _)| step >= size)
        .unwrap_or((1.0, ""));
    let (value, step) = (value / size, step / size);
    let decimals = (-step.log10().floor()).clamp(0.0, 6.0) as usize;
    let value = if value.abs() < step * 1e-6 {
        0.0
    } else {
        value
    };
    format!("{value:.decimals$}{unit}")
}

/// Series colors in turn: the theme's four hues, then the same hues toward the text color.
fn hue(palette: &Palette, index: usize) -> Rgb {
    let base = [palette.blue, palette.green, palette.yellow, palette.red][index % 4];
    if index / 4 % 2 == 1 {
        mix(base, palette.foreground, 0.45)
    } else {
        base
    }
}

fn series_rgb(chart: &Chart, palette: &Palette, index: usize) -> Rgb {
    let color = chart.series.get(index).and_then(|series| series.color);
    color.map_or_else(|| hue(palette, index), |color| palette.rgb(color))
}

/// The color for `share` (0 to 1) of a value range: blue, green, yellow, then red.
fn ramp(palette: &Palette, share: f64) -> Rgb {
    let stops = [palette.blue, palette.green, palette.yellow, palette.red];
    let at = share.clamp(0.0, 1.0) as f32 * 3.0;
    let index = (at.floor() as usize).min(2);
    mix(stops[index], stops[index + 1], at - index as f32)
}

fn mix(from: Rgb, to: Rgb, share: f32) -> Rgb {
    std::array::from_fn(|i| {
        let (from, to) = (f32::from(from[i]), f32::from(to[i]));
        (from + (to - from) * share).round() as u8
    })
}
