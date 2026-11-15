//! Charts: data that the overlay lays out with axes, ticks, gridlines, a legend, and theme
//! colors, so a model sends numbers instead of placing every bar and label.
//!
//! A 2D plot draws in its frame. A 3D plot draws in a box of fixed size inside its frame,
//! which the user can orbit and zoom like a scene. Grid plots share one layout: columns run
//! along x, rows along z, and values along y; a heatmap is that grid seen from above, with
//! values as colors.
//!
//! A candlestick plot reads one [open, high, low, close] point per category. A sankey plot
//! takes links between named nodes instead of series, and places each node in the column
//! after its farthest source ([`Chart::nodes`]).
//!
//! Axis ends left unset fit the data, rounded out to whole ticks ([`Scale`]).

use serde::{Deserialize, Serialize};

use super::shape::{LINE_WIDTHS, SCENE_FILL, check_text, finite, outlined};
use super::{Camera, Color, Key, Style};

const MAX_SERIES: usize = 8;
const MAX_NAMES: usize = 100;
/// Values, points, and grid cells in one chart.
const MAX_VALUES: usize = 5_000;
/// About this many ticks per axis.
const TICKS: f64 = 5.0;

/// A chart of data in a frame, in logical units from the monitor's top-left corner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chart {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// A title on the frame's top-left corner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub plot: Plot,
    /// Names along the x axis of bar, line, and area plots, a pie's slices, and a grid's
    /// columns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// Names of a grid's rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<String>,
    /// Every plot except sankey takes 1 to 8 series.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub series: Vec<Series>,
    /// Flows between named nodes: sankey plots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default)]
    pub x_axis: Axis,
    #[serde(default)]
    pub y_axis: Axis,
    #[serde(default)]
    pub z_axis: Axis,
    /// Stack the series of a bar, line, or area plot instead of placing them side by side or
    /// over each other.
    #[serde(default)]
    pub stacked: bool,
    /// Where a 3D plot is seen from.
    #[serde(default)]
    pub camera: Camera,
    /// Backdrop opacity in the theme background, from 0 to 1. The default is 0.92.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<f64>,
    /// Whether to outline the frame. The default is true.
    #[serde(default = "outlined")]
    pub border: bool,
    /// Width of the frame's border and of data lines, in logical units. The default is the
    /// window border width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_width: Option<f64>,
    /// Opacity of the whole chart, from 0 (hidden) to 1 (the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// Values the chart moves to over time; `series` keys change its data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Key>,
}

/// How a chart shows its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plot {
    Bar,
    Line,
    Area,
    Scatter,
    Pie,
    Donut,
    Candlestick,
    Sankey,
    Heatmap,
    Surface,
    #[serde(rename = "scatter_3d")]
    Scatter3d,
    #[serde(rename = "bar_3d")]
    Bar3d,
}

impl Plot {
    pub fn is_3d(self) -> bool {
        matches!(self, Self::Surface | Self::Scatter3d | Self::Bar3d)
    }

    pub fn has_grid(self) -> bool {
        matches!(self, Self::Heatmap | Self::Surface | Self::Bar3d)
    }
}

/// One set of data. A plot reads the field that fits it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Series {
    /// Shown in the legend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The default takes the next theme color; a grid without a color shades by value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    /// One value per category: bar, line, area, pie, and donut plots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<f64>,
    /// [x, y] points for scatter plots, and for line and area plots over numbers; [x, y, z]
    /// points for scatter_3d.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<Vec<f64>>,
    /// Rows of equal length: heatmap, surface, and bar_3d plots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grid: Vec<Vec<f64>>,
}

/// A flow of `value` from one sankey node to another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub value: f64,
}

/// A sankey node: its name, its column, and the larger of its flows in and out.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    pub column: usize,
    pub value: f64,
}

/// An axis's name and range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Axis {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The ends of the range shown. Unset ends fit the data; for a grid's columns and rows
    /// they default to 1 and the count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

impl Chart {
    /// Backdrop opacity, from 0 to 1.
    pub fn fill(&self) -> f64 {
        self.fill.unwrap_or(SCENE_FILL)
    }

    /// Frame and data line width in logical units.
    pub fn line_width(&self, style: &Style) -> f64 {
        self.line_width.unwrap_or(style.line_width)
    }

    /// The first series's grid, which grid plots draw.
    pub fn grid(&self) -> &[Vec<f64>] {
        self.series.first().map_or(&[], |series| &series.grid)
    }

    /// Sankey nodes in the order links first name them, each one column after its farthest
    /// source. None when the links loop back to a node.
    pub fn nodes(&self) -> Option<Vec<Node>> {
        let mut names: Vec<&str> = Vec::new();
        let mut index = |name| {
            names
                .iter()
                .position(|known| *known == name)
                .unwrap_or_else(|| {
                    names.push(name);
                    names.len() - 1
                })
        };
        let ends: Vec<(usize, usize, f64)> = (self.links.iter())
            .map(|link| {
                (
                    index(link.from.as_str()),
                    index(link.to.as_str()),
                    link.value,
                )
            })
            .collect();
        let mut column = vec![0; names.len()];
        // Each pass moves targets right of their sources; without a loop, one pass per node
        // settles them all.
        let settled = (0..=names.len()).any(|_| {
            let mut moved = false;
            for &(from, to, _) in &ends {
                if column[to] <= column[from] {
                    column[to] = column[from] + 1;
                    moved = true;
                }
            }
            !moved
        });
        if !settled {
            return None;
        }
        let (mut inflow, mut outflow) = (vec![0.0; names.len()], vec![0.0; names.len()]);
        for &(from, to, value) in &ends {
            outflow[from] += value;
            inflow[to] += value;
        }
        let nodes = names.into_iter().enumerate().map(|(i, name)| Node {
            name: name.to_owned(),
            column: column[i],
            value: f64::max(inflow[i], outflow[i]),
        });
        Some(nodes.collect())
    }

    pub(crate) fn check(&self) -> Result<(), &'static str> {
        let series = &self.series;
        let axes = [&self.x_axis, &self.y_axis, &self.z_axis];
        let numbers = series.iter().flat_map(|s| {
            let points = s.points.iter().flatten();
            s.values.iter().chain(points).chain(s.grid.iter().flatten())
        });
        let flows = self.links.iter().map(|link| &link.value);
        let count = series
            .iter()
            .map(|s| s.values.len() + s.points.len() + s.grid.iter().map(Vec::len).sum::<usize>())
            .sum::<usize>();
        let limits = axes
            .iter()
            .flat_map(|axis| axis.min.iter().chain(&axis.max));
        let rows = self.grid();
        let columns = rows.first().map_or(0, Vec::len);
        let only = |s: &Series, values: bool, points: bool, grid: bool| {
            s.values.is_empty() != values
                && s.points.is_empty() != points
                && s.grid.is_empty() != grid
        };
        let by_category = series
            .iter()
            .all(|s| only(s, true, false, false) && s.values.len() == self.categories.len());
        let by_point = |size: usize| {
            (series.iter())
                .all(|s| only(s, false, true, false) && s.points.iter().all(|p| p.len() == size))
        };
        let problem = match self.plot {
            _ if self.width <= 0.0 || self.height <= 0.0 => "width and height must be positive",
            _ if self.fill.is_some_and(|fill| !(0.0..=1.0).contains(&fill)) => {
                "fill must be between 0 and 1"
            }
            _ if self
                .line_width
                .is_some_and(|width| !LINE_WIDTHS.contains(&width)) =>
            {
                "line_width must be from 0.5 to 20"
            }
            _ if !self.camera.yaw.is_finite() || !(-90.0..=90.0).contains(&self.camera.pitch) => {
                "camera yaw must be finite and pitch between -90 and 90"
            }
            Plot::Sankey if !series.is_empty() || self.links.is_empty() => {
                "a sankey takes links and no series"
            }
            _ if self.plot != Plot::Sankey && (series.is_empty() || series.len() > MAX_SERIES) => {
                "a chart needs 1 to 8 series"
            }
            _ if self.plot != Plot::Sankey && !self.links.is_empty() => "only a sankey takes links",
            _ if self.categories.len() > MAX_NAMES || self.rows.len() > MAX_NAMES => {
                "a chart takes at most 100 categories and 100 rows"
            }
            _ if self.links.len() > MAX_NAMES => "a sankey takes at most 100 links",
            _ if count > MAX_VALUES => "a chart takes at most 5000 values, points, and grid cells",
            _ if !numbers.chain(limits).chain(flows).copied().all(finite) => {
                "chart numbers must be finite"
            }
            _ if axes
                .iter()
                .any(|a| a.min.zip(a.max).is_some_and(|(min, max)| min >= max)) =>
            {
                "an axis min must be below its max"
            }
            Plot::Bar | Plot::Pie | Plot::Donut if self.categories.is_empty() => {
                "bar and pie plots need categories"
            }
            Plot::Bar if !by_category => "each series needs one value per category",
            Plot::Line | Plot::Area
                if !(by_category && !self.categories.is_empty() || by_point(2)) =>
            {
                "each series needs one value per category, or [x, y] points"
            }
            Plot::Scatter if !by_point(2) => "each series needs [x, y] points",
            Plot::Scatter3d if !by_point(3) => "each series needs [x, y, z] points",
            Plot::Candlestick
                if series.len() > 1
                    || !by_point(4)
                    || series[0].points.len() != self.categories.len()
                    || series[0].points.iter().any(|p| {
                        let [open, high, low, close] = [p[0], p[1], p[2], p[3]];
                        high < open.max(close) || low > open.min(close)
                    }) =>
            {
                "a candlestick takes one series with an [open, high, low, close] point per category, high the highest and low the lowest"
            }
            Plot::Sankey if self.links.iter().any(|link| link.value <= 0.0) => {
                "every link value must be positive"
            }
            Plot::Sankey if self.nodes().is_none() => "sankey links must not loop back to a node",
            Plot::Pie | Plot::Donut
                if series.len() > 1
                    || !by_category
                    || series[0].values.iter().any(|v| *v < 0.0)
                    || series[0].values.iter().sum::<f64>() <= 0.0 =>
            {
                "a pie takes one series of values that are not negative and not all 0"
            }
            Plot::Heatmap | Plot::Surface | Plot::Bar3d
                if series.len() > 1
                    || !only(&series[0], false, false, true)
                    || columns == 0
                    || rows.iter().any(|row| row.len() != columns) =>
            {
                "a grid plot takes one series with a grid of equal rows"
            }
            Plot::Surface if rows.len() < 2 || columns < 2 => {
                "a surface needs at least 2 rows of 2"
            }
            _ if self.plot.has_grid()
                && (!self.categories.is_empty() && self.categories.len() != columns
                    || !self.rows.is_empty() && self.rows.len() != rows.len()) =>
            {
                "categories must name every grid column, and rows every grid row"
            }
            _ => "",
        };
        if !problem.is_empty() {
            return Err(problem);
        }
        let labels = axes.iter().filter_map(|axis| axis.label.as_ref());
        let names = series.iter().filter_map(|s| s.name.as_ref());
        let nodes = self.links.iter().flat_map(|link| [&link.from, &link.to]);
        (self
            .categories
            .iter()
            .chain(&self.rows)
            .chain(labels)
            .chain(names)
            .chain(nodes))
        .chain(&self.label)
        .try_for_each(|name| check_text(name, false))
    }
}

impl Chart {
    /// The x, y, and z scales of the data. A grid's columns and rows use [`Scale::span`].
    pub fn scales(&self) -> [Scale; 3] {
        let extents = extents(self);
        let axes = [&self.x_axis, &self.y_axis, &self.z_axis];
        std::array::from_fn(|axis| Scale::fit(axes[axis], extents[axis]))
    }

    /// Where each series's bar, line, or area runs in each category, as (from, to): from 0,
    /// or stacked on the series before, positive values rising and negative ones falling.
    pub fn stacks(&self) -> Vec<Vec<(f64, f64)>> {
        let count = self
            .series
            .iter()
            .map(|s| s.values.len())
            .max()
            .unwrap_or(0);
        let (mut rising, mut falling) = (vec![0.0; count], vec![0.0; count]);
        let mut stacks = Vec::new();
        for series in &self.series {
            let mut spans = Vec::new();
            for (index, &value) in series.values.iter().enumerate() {
                let top = if value >= 0.0 {
                    &mut rising
                } else {
                    &mut falling
                };
                match top.get_mut(index).filter(|_| self.stacked) {
                    Some(top) => {
                        spans.push((*top, *top + value));
                        *top += value;
                    }
                    None => spans.push((0.0, value)),
                }
            }
            stacks.push(spans);
        }
        stacks
    }

    /// Fix unset axis ends to fit the data at every key, so the axes hold still while keys
    /// change the data.
    pub fn pin_axes(&mut self) {
        if self.keys.iter().all(|key| key.series.is_none()) {
            return;
        }
        let style = Style::default();
        let extents = std::iter::once(0.0)
            .chain(self.keys.iter().map(|key| key.at))
            .map(|t| extents(&self.at(t, &style)))
            .fold([None; 3], |all: [Option<(f64, f64)>; 3], extents| {
                std::array::from_fn(|axis| match (all[axis], extents[axis]) {
                    (Some((a, b)), Some((c, d))) => Some((a.min(c), b.max(d))),
                    (one, other) => one.or(other),
                })
            });
        let axes = [&mut self.x_axis, &mut self.y_axis, &mut self.z_axis];
        for (axis, extent) in axes.into_iter().zip(extents) {
            if extent.is_some() {
                let scale = Scale::fit(axis, extent);
                (axis.min, axis.max) = (Some(scale.min), Some(scale.max));
            }
        }
    }
}

/// A numeric axis: its range and the step between ticks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

impl Scale {
    /// The range `axis` sets, with unset ends fitting `extent` rounded out to whole ticks.
    fn fit(axis: &Axis, extent: Option<(f64, f64)>) -> Self {
        let (low, high) = extent.unwrap_or((0.0, 1.0));
        let mut scale = Self::exact(axis.min.unwrap_or(low), axis.max.unwrap_or(high));
        // Rounding the ends out to whole steps can widen the range enough to want a larger
        // step, so round until the step holds; fitting a fitted range then changes nothing.
        for _ in 0..3 {
            let step = scale.step;
            let min = axis.min.unwrap_or((scale.min / step).floor() * step);
            let max = axis.max.unwrap_or((scale.max / step).ceil() * step);
            scale = Self::exact(min, max);
            if scale.step == step {
                break;
            }
        }
        scale
    }

    /// The range from `low` to `high`, widened when it is empty.
    pub fn exact(low: f64, high: f64) -> Self {
        let (min, max) = if high > low {
            (low, high)
        } else {
            (low - 0.5, low + 0.5)
        };
        Self {
            min,
            max,
            step: nice((max - min) / TICKS),
        }
    }

    /// The values a grid's `count` columns or rows span along `axis`.
    pub fn span(axis: &Axis, count: usize) -> Self {
        Self::exact(axis.min.unwrap_or(1.0), axis.max.unwrap_or(count as f64))
    }

    /// Where `value` falls, from 0 at the minimum to 1 at the maximum.
    pub fn place(self, value: f64) -> f64 {
        (value - self.min) / (self.max - self.min)
    }

    /// The ticks within the range: whole steps.
    pub fn ticks(self) -> impl Iterator<Item = f64> {
        let first = (self.min / self.step - 1e-9).ceil() as i64;
        let last = (self.max / self.step + 1e-9).floor() as i64;
        (first..=last)
            .take(100)
            .map(move |tick| tick as f64 * self.step)
    }
}

/// A round step near `raw`: 1, 2, or 5 times a power of ten.
fn nice(raw: f64) -> f64 {
    if !(raw > 0.0 && raw.is_finite()) {
        return 1.0;
    }
    let power = 10_f64.powf(raw.log10().floor());
    let step = match raw / power {
        share if share < 1.5 => 1.0,
        share if share < 3.0 => 2.0,
        share if share < 7.0 => 5.0,
        _ => 10.0,
    };
    step * power
}

/// The lowest and highest data value along x, y, and z, where the data sets them.
fn extents(chart: &Chart) -> [Option<(f64, f64)>; 3] {
    let mut extents = [None; 3];
    let mut add = |axis: usize, value: f64| {
        let (low, high) = extents[axis].get_or_insert((value, value));
        *low = value.min(*low);
        *high = value.max(*high);
    };
    for series in &chart.series {
        for point in &series.points {
            if chart.plot == Plot::Candlestick {
                point.iter().for_each(|&v| add(1, v));
            } else {
                point
                    .iter()
                    .take(3)
                    .enumerate()
                    .for_each(|(axis, &v)| add(axis, v));
            }
        }
        series
            .grid
            .iter()
            .flatten()
            .for_each(|&value| add(1, value));
    }
    for (from, to) in chart.stacks().into_iter().flatten() {
        if chart.stacked {
            add(1, from);
        }
        add(1, to);
    }
    if matches!(chart.plot, Plot::Bar | Plot::Area | Plot::Bar3d) {
        add(1, 0.0);
    }
    extents
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chart(fields: serde_json::Value) -> Chart {
        let mut chart = json!({
            "x": 0, "y": 0, "width": 400, "height": 300, "plot": "bar",
            "categories": ["a", "b"], "series": [{"values": [3, 7]}, {"values": [-2, 4]}]
        });
        let all = chart.as_object_mut().unwrap();
        all.extend(fields.as_object().unwrap().clone());
        serde_json::from_value(chart).unwrap()
    }

    #[test]
    fn scales_round_out_to_whole_ticks() {
        let bars = chart(json!({}));
        assert_eq!(
            bars.scales()[1],
            Scale {
                min: -2.0,
                max: 8.0,
                step: 2.0
            }
        );

        let stacked = chart(json!({"stacked": true}));
        assert_eq!(stacked.stacks()[1], [(0.0, -2.0), (7.0, 11.0)]);
        assert_eq!(stacked.scales()[1].max, 12.0);

        let candles = chart(json!({
            "plot": "candlestick", "series": [{"points": [[10, 14, 9, 13], [13, 13, 6, 7]]}]
        }));
        let [_, prices, _] = candles.scales();
        assert_eq!((prices.min, prices.max), (6.0, 14.0));
    }

    #[test]
    fn pinned_axes_fit_every_key() {
        let mut keyed = chart(json!({
            "series": [{"values": [1, 2]}],
            "keys": [{"at": 1, "series": [{"values": [1, 30]}]}]
        }));
        keyed.pin_axes();
        assert_eq!(
            (keyed.y_axis.min, keyed.y_axis.max),
            (Some(0.0), Some(30.0))
        );
    }
}
