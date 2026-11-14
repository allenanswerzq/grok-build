//! `screen_draw`: draw boxes, lines and arrows, text, small 3D scenes, and charts on the
//! screen.
//!
//! Shapes use `screen_read`'s logical units: (x, y) from the active monitor's top-left
//! corner. Each call replaces the whole drawing, and an empty list clears it.
//!
//! Shapes may change over time through keys ([`animation`]); the overlay plays them.
//! Charts ([`chart`]) turn data into laid-out plots.
//!
//! The tool validates shapes, reads the desktop's look ([`Style`]), and sends one
//! [`OverlayRequest`] line to `grok-screen-overlay`, a small Wayland process in `overlay/`
//! that shows the drawing above all windows. Clicks pass through everywhere except scenes
//! and 3D charts, which the user can drag to orbit and scroll to zoom. It lives only while a
//! drawing is visible: clearing, cancellation, or the end of the session kills it, which
//! removes the drawing.
//!
//! `just build` builds the overlay beside the CLI. Linux builds need pkg-config and the
//! Wayland and libxkbcommon development libraries; at run time the overlay needs a layer-shell
//! compositor such as Hyprland.

mod animation;
mod chart;
pub mod overlay;

pub use animation::{Ease, Key, ObjectKey, Repeat};
pub use chart::{Axis, Chart, Link, Plot, Series};

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use xai_tool_protocol::{ToolCapabilities, ToolId, ToolScope};
use xai_tool_runtime::{ListToolsContext, Tool, ToolCallContext, ToolError, ToolErrorKind};
use xai_tool_types::ToolDescription;

use super::hyprland::{Hyprland, Monitor, command};
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::types::tool_metadata::{ToolMetadata, shared_resources};
use crate::util::{ProcessGroup, ProcessScope, global_process_scope};

const MAX_SHAPES: usize = 64;
const MAX_TEXT_CHARS: usize = 200;
const MAX_COORDINATE: f64 = 100_000.0;
const MAX_SCENE_OBJECTS: usize = 64;
/// Limit for the vertices, and separately the faces, of all meshes in one scene.
const MAX_MESH_SIZE: usize = 2_000;
const OVERLAY: &str = "grok-screen-overlay";
/// Covers starting the overlay, loading fonts, and the compositor showing the first frame.
const SHOW_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REPLY: u64 = 64 * 1024;
/// Default fill opacities: a faint tint for boxes and nearly solid backdrops for scenes and
/// text.
pub(crate) const BOX_FILL: f64 = 0.08;
pub(crate) const SCENE_FILL: f64 = 0.92;
pub(crate) const TEXT_FILL: f64 = 0.92;
/// Accepted line widths and text sizes, in logical units.
const LINE_WIDTHS: std::ops::RangeInclusive<f64> = 0.5..=20.0;
const TEXT_SIZES: std::ops::RangeInclusive<f64> = 8.0..=96.0;

/// The complete drawing for the active monitor.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScreenDrawInput {
    /// Shapes that replace the current drawing. Use [] to clear the screen.
    pub shapes: Vec<Shape>,
    /// How shapes with keys play: once, keeping the last frame (the default); in a loop; or
    /// forward and back.
    #[serde(default)]
    pub repeat: Repeat,
}

/// A shape in logical units from the monitor's top-left corner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    /// A rectangle, with an optional label just above it.
    Box {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default)]
        color: Color,
        /// Fill opacity in the box color, from 0 (see-through) to 1 (solid). The default,
        /// 0.08, is a faint tint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<f64>,
        /// Whether to outline the box. The default is true.
        #[serde(default = "outlined")]
        border: bool,
        /// Border width in logical units. The default is the window border width.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_width: Option<f64>,
        /// Opacity of the whole shape, from 0 (hidden) to 1 (the default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<f64>,
        /// Values the shape moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<Key>,
    },
    /// A straight line from (x, y) to (to_x, to_y), with optional arrowheads.
    Line {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
        #[serde(default)]
        color: Color,
        /// Width in logical units. The default is 1.5 times the window border width.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_width: Option<f64>,
        /// Which ends get an arrowhead. The default is none.
        #[serde(default)]
        arrows: Arrows,
        /// Solid, dashed, or dotted. The default is solid.
        #[serde(default)]
        pattern: Pattern,
        /// Opacity of the whole shape, from 0 (hidden) to 1 (the default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<f64>,
        /// Values the shape moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<Key>,
    },
    /// Text whose top-left corner is at (x, y), on a panel in the theme background. It may
    /// span lines and wraps at 30 times its size.
    Text {
        x: f64,
        y: f64,
        text: String,
        /// The panel's border color, or the words' color when there is no border.
        #[serde(default)]
        color: Color,
        /// Font size in logical units. The default is 14.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f64>,
        /// Panel opacity, from 0 (no panel) to 1 (solid). The default is 0.92.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<f64>,
        /// Whether to outline the panel. The default is true.
        #[serde(default = "outlined")]
        border: bool,
        /// Opacity of the whole shape, from 0 (hidden) to 1 (the default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<f64>,
        /// Values the shape moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<Key>,
    },
    /// A 3D view in a frame. The camera fits every object inside the frame, and the user
    /// can drag the frame to orbit the objects (sideways for yaw, up and down for pitch) and
    /// scroll to zoom.
    Scene {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Backdrop opacity in the theme background, from 0 (objects float over the
        /// windows) to 1 (solid). The default is 0.92.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<f64>,
        /// Whether to outline the frame. The default is true.
        #[serde(default = "outlined")]
        border: bool,
        /// Width of the frame's border and of line objects, in logical units. The default
        /// is the window border width.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_width: Option<f64>,
        #[serde(default)]
        camera: Camera,
        objects: Vec<SceneObject>,
        /// Opacity of the whole shape, from 0 (hidden) to 1 (the default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<f64>,
        /// Values the shape moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<Key>,
    },
    /// A chart of data in a frame, laid out with axes, ticks, gridlines, and a legend.
    Chart(Chart),
}

fn outlined() -> bool {
    true
}

/// A 3D point or size: [x, y, z]. y points up, and +z faces the default front view.
pub type Point3 = [f64; 3];

/// Where a scene is seen from. The camera circles the objects and looks at their center.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Camera {
    /// Degrees to circle right from the front. The default is 35.
    pub yaw: f64,
    /// Degrees above the objects, from -90 to 90. The default is 25.
    pub pitch: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: 35.0,
            pitch: 25.0,
        }
    }
}

/// An object in a scene. Units are arbitrary; the scene is scaled to fit its frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneObject {
    /// A box aligned with the axes.
    Cuboid {
        center: Point3,
        size: Point3,
        #[serde(default)]
        color: Color,
        /// Degrees to turn about the x, y, then z axis, around the object's center.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation: Option<Point3>,
        /// Values the object moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<ObjectKey>,
    },
    Sphere {
        center: Point3,
        radius: f64,
        #[serde(default)]
        color: Color,
        /// Values the object moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<ObjectKey>,
    },
    /// A round rod between the centers of its two ends.
    Cylinder {
        from: Point3,
        to: Point3,
        radius: f64,
        #[serde(default)]
        color: Color,
        /// Degrees to turn about the x, y, then z axis, around the object's center.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation: Option<Point3>,
        /// Values the object moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<ObjectKey>,
    },
    Line {
        from: Point3,
        to: Point3,
        #[serde(default)]
        color: Color,
        /// Degrees to turn about the x, y, then z axis, around the object's center.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation: Option<Point3>,
        /// Values the object moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<ObjectKey>,
    },
    /// Triangles; each face lists three indices into `vertices`.
    Mesh {
        vertices: Vec<Point3>,
        faces: Vec<[usize; 3]>,
        #[serde(default)]
        color: Color,
        /// Degrees to turn about the x, y, then z axis, around the object's center.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation: Option<Point3>,
        /// Values the object moves to over time.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<ObjectKey>,
    },
}

/// The ends of a line that get an arrowhead: "end" points at (to_x, to_y).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Arrows {
    #[default]
    None,
    Start,
    End,
    Both,
}

impl Arrows {
    /// Whether the start and the end get an arrowhead.
    pub(crate) fn ends(self) -> (bool, bool) {
        (
            matches!(self, Self::Start | Self::Both),
            matches!(self, Self::End | Self::Both),
        )
    }
}

/// How a line is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

/// A color of the current Omarchy theme. `accent` matches the focused window's border.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    #[default]
    Accent,
    Red,
    Yellow,
    Green,
    Blue,
}

impl Shape {
    fn kind(&self) -> &'static str {
        match self {
            Self::Box { .. } => "box",
            Self::Line { .. } => "line",
            Self::Text { .. } => "text",
            Self::Scene { .. } => "scene",
            Self::Chart(_) => "chart",
        }
    }

    /// The whole shape's opacity, from 0 to 1.
    pub(crate) fn opacity(&self) -> f64 {
        match *self {
            Self::Box { opacity, .. }
            | Self::Line { opacity, .. }
            | Self::Text { opacity, .. }
            | Self::Scene { opacity, .. } => opacity.unwrap_or(1.0),
            Self::Chart(ref chart) => chart.opacity.unwrap_or(1.0),
        }
    }

    /// The frame, as (x, y, width, height), and camera of a shape the user can orbit and
    /// zoom: a scene or a 3D chart.
    pub(crate) fn orbit(&self) -> Option<((f64, f64, f64, f64), Camera)> {
        match *self {
            Self::Scene {
                x,
                y,
                width,
                height,
                camera,
                ..
            } => Some(((x, y, width, height), camera)),
            Self::Chart(ref c) if c.plot.is_3d() => Some(((c.x, c.y, c.width, c.height), c.camera)),
            _ => None,
        }
    }

    /// The points that must lie on the monitor: frame corners, line ends, or the text anchor.
    fn points(&self) -> Vec<(f64, f64)> {
        match *self {
            Self::Box {
                x,
                y,
                width,
                height,
                ..
            }
            | Self::Scene {
                x,
                y,
                width,
                height,
                ..
            } => vec![(x, y), (x + width, y + height)],
            Self::Line {
                x, y, to_x, to_y, ..
            } => vec![(x, y), (to_x, to_y)],
            Self::Text { x, y, .. } => vec![(x, y)],
            Self::Chart(ref c) => vec![(c.x, c.y), (c.x + c.width, c.y + c.height)],
        }
    }

    fn check(&self) -> Result<(), &'static str> {
        if !self.points().iter().all(|&(x, y)| finite(x) && finite(y)) {
            return Err("coordinates must be finite numbers");
        }
        match self {
            _ if !(0.0..=1.0).contains(&self.opacity()) => Err("opacity must be between 0 and 1"),
            Self::Box { width, height, .. } | Self::Scene { width, height, .. }
                if *width <= 0.0 || *height <= 0.0 =>
            {
                Err("width and height must be positive")
            }
            Self::Box { fill, .. } | Self::Scene { fill, .. } | Self::Text { fill, .. }
                if fill.is_some_and(|fill| !(0.0..=1.0).contains(&fill)) =>
            {
                Err("fill must be between 0 and 1")
            }
            Self::Box { line_width, .. }
            | Self::Line { line_width, .. }
            | Self::Scene { line_width, .. }
                if line_width.is_some_and(|width| !LINE_WIDTHS.contains(&width)) =>
            {
                Err("line_width must be from 0.5 to 20")
            }
            Self::Text {
                size: Some(size), ..
            } if !TEXT_SIZES.contains(size) => Err("text size must be from 8 to 96"),
            Self::Box {
                fill: Some(fill),
                border: false,
                ..
            } if *fill == 0.0 => Err("a box without a border needs a fill above 0"),
            Self::Box {
                label: Some(label), ..
            } => check_text(label, false),
            Self::Line {
                x, y, to_x, to_y, ..
            } if x == to_x && y == to_y => Err("the line must have length"),
            Self::Text { text, .. } => check_text(text, true),
            Self::Scene {
                label,
                camera,
                objects,
                ..
            } => {
                if !camera.yaw.is_finite() || !(-90.0..=90.0).contains(&camera.pitch) {
                    return Err("camera yaw must be finite and pitch between -90 and 90");
                }
                if objects.is_empty() || objects.len() > MAX_SCENE_OBJECTS {
                    return Err("a scene needs 1 to 64 objects");
                }
                let (vertices, faces) =
                    objects.iter().fold((0, 0), |(v, f), object| match object {
                        SceneObject::Mesh {
                            vertices, faces, ..
                        } => (v + vertices.len(), f + faces.len()),
                        _ => (v, f),
                    });
                if vertices > MAX_MESH_SIZE || faces > MAX_MESH_SIZE {
                    return Err("a scene's meshes may use at most 2000 vertices and 2000 faces");
                }
                objects.iter().try_for_each(SceneObject::check)?;
                label
                    .as_deref()
                    .map_or(Ok(()), |label| check_text(label, false))
            }
            Self::Chart(chart) => chart.check(),
            _ => Ok(()),
        }
    }
}

impl SceneObject {
    /// Degrees to turn about the x, y, then z axis, around the object's center.
    pub(crate) fn rotation(&self) -> Option<Point3> {
        match *self {
            Self::Cuboid { rotation, .. }
            | Self::Cylinder { rotation, .. }
            | Self::Line { rotation, .. }
            | Self::Mesh { rotation, .. } => rotation,
            Self::Sphere { .. } => None,
        }
    }

    fn check(&self) -> Result<(), &'static str> {
        let mut numbers: Vec<f64> = match self {
            Self::Cuboid { center, size, .. } => center.iter().chain(size).copied().collect(),
            Self::Sphere { center, radius, .. } => center.iter().chain([radius]).copied().collect(),
            Self::Cylinder {
                from, to, radius, ..
            } => from.iter().chain(to).chain([radius]).copied().collect(),
            Self::Line { from, to, .. } => from.iter().chain(to).copied().collect(),
            Self::Mesh { vertices, .. } => vertices.iter().flatten().copied().collect(),
        };
        numbers.extend(self.rotation().into_iter().flatten());
        if !numbers.into_iter().all(finite) {
            return Err("3D coordinates must be finite numbers");
        }
        match self {
            Self::Cuboid { size, .. } if size.iter().any(|side| *side <= 0.0) => {
                Err("cuboid sizes must be positive")
            }
            Self::Sphere { radius, .. } | Self::Cylinder { radius, .. } if *radius <= 0.0 => {
                Err("radius must be positive")
            }
            Self::Cylinder { from, to, .. } | Self::Line { from, to, .. } if from == to => {
                Err("from and to must differ")
            }
            Self::Mesh { faces, .. } if faces.is_empty() => Err("a mesh needs at least one face"),
            Self::Mesh {
                vertices, faces, ..
            } if faces.iter().flatten().any(|index| *index >= vertices.len()) => {
                Err("mesh faces must use existing vertex indices")
            }
            _ => Ok(()),
        }
    }
}

fn finite(number: f64) -> bool {
    number.is_finite() && number.abs() <= MAX_COORDINATE
}

fn check_text(text: &str, multiline: bool) -> Result<(), &'static str> {
    if text.trim().is_empty() {
        Err("text must not be empty")
    } else if text.chars().count() > MAX_TEXT_CHARS {
        Err("text must be at most 200 characters")
    } else if text
        .chars()
        .any(|c| c.is_control() && !(multiline && c == '\n'))
    {
        Err("text must not contain control characters")
    } else {
        Ok(())
    }
}

fn invalid_shape(index: usize, shape: &Shape, problem: &str) -> ToolError {
    ToolError::invalid_arguments(format!("Shape {} ({}): {problem}", index + 1, shape.kind()))
}

/// Reject malformed shapes before anything is drawn.
///
/// # Errors
/// Names the first invalid shape and the rule it breaks.
pub fn validate(shapes: &[Shape]) -> Result<(), ToolError> {
    if shapes.len() > MAX_SHAPES {
        return Err(ToolError::invalid_arguments(format!(
            "Use at most {MAX_SHAPES} shapes"
        )));
    }
    for (index, shape) in shapes.iter().enumerate() {
        shape
            .check()
            .map_err(|problem| invalid_shape(index, shape, problem))?;
        shape
            .check_keys()
            .map_err(|problem| invalid_shape(index, shape, &problem))?;
    }
    Ok(())
}

/// Reject shapes that leave the monitor at any key. Points on its edge fit.
fn fit(shapes: &[Shape], monitor: &Monitor) -> Result<(), ToolError> {
    let style = Style::default();
    for (index, shape) in shapes.iter().enumerate() {
        let mut moments = std::iter::once(0.0)
            .chain(shape.times())
            .map(|t| shape.at(t, &style));
        if !moments.all(|shape| shape.points().iter().all(|&(x, y)| monitor.contains(x, y))) {
            let (width, height) = monitor.logical_size();
            return Err(invalid_shape(
                index,
                shape,
                &format!(
                    "it extends past monitor {}, which is {} × {} logical units",
                    monitor.name, width, height
                ),
            ));
        }
    }
    Ok(())
}

/// An RGB color.
pub type Rgb = [u8; 3];

/// Theme colors. [`Color`] names pick one; `background` and `foreground` style text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    pub accent: Rgb,
    pub red: Rgb,
    pub yellow: Rgb,
    pub green: Rgb,
    pub blue: Rgb,
    pub background: Rgb,
    pub foreground: Rgb,
}

impl Default for Palette {
    /// Tokyo Night, Omarchy's default theme.
    fn default() -> Self {
        Self {
            accent: [0x7a, 0xa2, 0xf7],
            red: [0xf7, 0x76, 0x8e],
            yellow: [0xe0, 0xaf, 0x68],
            green: [0x9e, 0xce, 0x6a],
            blue: [0x7a, 0xa2, 0xf7],
            background: [0x1a, 0x1b, 0x26],
            foreground: [0xa9, 0xb1, 0xd6],
        }
    }
}

impl Palette {
    /// The RGB value of a theme color name.
    pub fn rgb(&self, color: Color) -> Rgb {
        match color {
            Color::Accent => self.accent,
            Color::Red => self.red,
            Color::Yellow => self.yellow,
            Color::Green => self.green,
            Color::Blue => self.blue,
        }
    }

    /// Read `name = "#rrggbb"` lines of an Omarchy `colors.toml`. Colors it does not
    /// define keep their defaults.
    fn parse(colors: &str) -> Self {
        let mut palette = Self::default();
        for line in colors.lines() {
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let Some(rgb) = hex(value.trim().trim_matches('"')) else {
                continue;
            };
            let slot = match name.trim() {
                "accent" => &mut palette.accent,
                "red" => &mut palette.red,
                "yellow" => &mut palette.yellow,
                "green" => &mut palette.green,
                "blue" => &mut palette.blue,
                "background" => &mut palette.background,
                "foreground" => &mut palette.foreground,
                _ => continue,
            };
            *slot = rgb;
        }
        palette
    }
}

fn hex(value: &str) -> Option<Rgb> {
    let digits = value
        .strip_prefix('#')
        .filter(|digits| digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let [_, r, g, b] = u32::from_str_radix(digits, 16).ok()?.to_be_bytes();
    Some([r, g, b])
}

/// How drawings look, read from the desktop on every draw so theme changes apply at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    /// Line width in logical units: Hyprland's window border size.
    pub line_width: f64,
    /// Corner radius in logical units: Hyprland's window rounding.
    pub rounding: f64,
    /// Font family for labels and text: the system monospace font.
    pub font: String,
    pub font_size: f64,
    pub palette: Palette,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            line_width: 2.0,
            rounding: 0.0,
            font: "monospace".to_owned(),
            font_size: 14.0,
            palette: Palette::default(),
        }
    }
}

impl Style {
    /// Omarchy theme colors, Hyprland border size and rounding, and the monospace font.
    /// Settings that cannot be read keep their defaults.
    async fn current(hyprland: Hyprland) -> Self {
        let (palette, line_width, rounding, font) = tokio::join!(
            theme_palette(),
            hyprland.option("general:border_size"),
            hyprland.option("decoration:rounding"),
            monospace_font(),
        );
        let default = Self::default();
        Self {
            line_width: line_width.map_or(default.line_width, |width| width.clamp(1.0, 6.0)),
            rounding: rounding.map_or(default.rounding, |radius| radius.clamp(0.0, 20.0)),
            font: font.unwrap_or(default.font),
            font_size: default.font_size,
            palette: palette.unwrap_or(default.palette),
        }
    }
}

async fn theme_palette() -> Option<Palette> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(xai_dirs::home_dir()?.join(".local/state")))?;
    let colors = tokio::fs::read_to_string(state.join("omarchy/current/theme/colors.toml"))
        .await
        .ok()?;
    Some(Palette::parse(&colors))
}

async fn monospace_font() -> Option<String> {
    let name = command("fc-match", &["-f", "%{family[0]}", "monospace"], 1024)
        .await
        .ok()?;
    let name = String::from_utf8(name).ok()?.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// One line sent to `grok-screen-overlay`: the whole drawing for one monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayRequest {
    /// Monitor name, as Wayland and Hyprland report it.
    pub monitor: String,
    pub style: Style,
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub repeat: Repeat,
}

/// What the screen shows after a call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScreenDrawOutput {
    Drawn {
        monitor: String,
        width: f64,
        height: f64,
        shapes: usize,
    },
    Cleared,
}

impl std::fmt::Display for ScreenDrawOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Drawn {
                monitor,
                width,
                height,
                shapes,
            } => write!(
                f,
                "Drew {shapes} {} on monitor {monitor} ({width} × {height} logical units). \
                 The drawing stays until the next screen_draw call or the end of the session.",
                if *shapes == 1 { "shape" } else { "shapes" }
            ),
            Self::Cleared => f.write_str("Cleared the drawing."),
        }
    }
}

impl xai_tool_runtime::ToolOutput for ScreenDrawOutput {}

impl From<ScreenDrawOutput> for ToolOutput {
    fn from(output: ScreenDrawOutput) -> Self {
        Self::Text(output.to_string().into())
    }
}

/// Drawing boundary. Install a [`PainterBackend`] to replace desktop drawing.
#[async_trait]
pub trait ScreenPainter: Send + Sync {
    /// Replace the drawing on the active monitor. No shapes clears it.
    ///
    /// # Errors
    /// Returns an error when the desktop cannot draw or a shape leaves the monitor.
    async fn screen_draw(&self, input: &ScreenDrawInput) -> Result<ScreenDrawOutput, ToolError>;
}

/// A session's screen painter. Insert into tool Resources to supply another backend.
#[derive(Clone)]
pub struct PainterBackend(Arc<dyn ScreenPainter>);

impl PainterBackend {
    /// Wrap a painter for shared use within a tool session.
    pub fn new(painter: impl ScreenPainter + 'static) -> Self {
        Self(Arc::new(painter))
    }
}

impl std::fmt::Debug for PainterBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PainterBackend").finish_non_exhaustive()
    }
}

const DESCRIPTION: &str = r#"Draw on the user's screen to point at or explain something.

Screen:
- Shapes appear on the active monitor, above all windows.
- Clicks and typing pass through to the windows below.
- The user can drag a scene or 3D chart to orbit it, and scroll over it to zoom.
- Positions use screen_read's logical units: (x, y) from the monitor's top-left corner.
- To mark a point in a screen_read window screenshot, add the window's x and y.
- Each call replaces the previous drawing. Call with an empty list to clear it.
- The drawing also clears when the session ends.

Shapes:
- A line runs from (x, y) to (to_x, to_y).
- A line's arrows puts a head on its start, end, or both ends.
- A line's pattern makes it solid, dashed, or dotted.
- A scene is a 3D view of cuboids, spheres, cylinders, lines, and triangle meshes; y points up.
- A scene's camera circles its objects by yaw and pitch in degrees and fits them in the frame.
- Boxes, scenes, and text panels take a fill opacity from 0 to 1 and an optional border.
- An outline with a faint fill highlights what is under a box; fill 1 covers it.
- A scene with fill 0 and no border floats its objects over the windows.
- Text without a border shows its words in its color.
- Boxes, lines, and scenes take a line_width, and text takes a size, both in logical units.
- The defaults follow the window border and a 14-unit font.
- Colors come from the user's theme. accent, the default, matches the focused window's border.

Animation:
- To animate a shape, give it keys. Each key has a time `at` in seconds and the values it sets.
- Shape keys can set x, y, width, height, to_x, to_y, color, fill, opacity, line_width, size,
  text, and a scene's camera yaw and pitch.
- Scene object keys can set center, size, radius, from, to, color, and rotation.
- rotation turns an object in degrees about the x, y, then z axis around its center.
- A key's ease sets how fields move from their previous values: linear at constant speed (the
  default), in_out easing in and out, or step all at once when the key arrives.
- Opacity 0 hides a shape, so step keys on opacity make flipbook frames.
- The timeline ends at the latest key, at most 60 seconds.
- repeat plays the timeline once and keeps the last frame (once, the default), again from the
  start (loop), or forward and back (bounce).

Charts:
- A chart lays out its own axes, ticks, gridlines, legend, and colors in its frame.
- Its plot is bar, line, area, scatter, pie, donut, candlestick, sankey, heatmap, surface,
  scatter_3d, or bar_3d.
- A chart takes 1 to 8 series, except a sankey, which takes links instead.
- Bar, line, area, pie, and donut series give one value per category.
- Line, area, and scatter series may give [x, y] points instead; scatter_3d series give [x, y, z].
- Pie and donut take one series.
- Heatmap, surface, and bar_3d take one series with a grid of equal rows.
- A grid's columns run along x, its rows along z, and its values along y (up).
- A heatmap shows its grid from above, with values as colors.
- A candlestick takes one series with an [open, high, low, close] point per category.
- Rising candles are green and falling ones red.
- A sankey's links are each {from, to, value} between named nodes.
- A sankey node sits one column right of its farthest source.
- Each sankey column stacks its nodes in the order links first name them.
- categories names the x categories, pie slices, or grid columns; rows names grid rows.
- x_axis, y_axis, and z_axis each take a label and an optional min and max.
- Unset axis ends fit the data.
- stacked stacks bar, line, and area series that give values.
- A series takes a name for the legend and a color.
- Without colors, series take theme colors in turn and grids shade by value.
- Keys that set series change a chart's data over time.
- The axes fit the data of every key, so they hold still."#;

/// Registered tool that draws on the active monitor.
#[derive(Debug, Default)]
pub struct ScreenDrawTool;

impl ToolMetadata for ScreenDrawTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }
    fn description_template(&self) -> &str {
        DESCRIPTION
    }
}

impl Tool for ScreenDrawTool {
    type Args = ScreenDrawInput;
    type Output = ScreenDrawOutput;
    fn id(&self) -> ToolId {
        ToolId::new("screen_draw").expect("valid tool id")
    }
    fn description(&self, _: &ListToolsContext) -> ToolDescription {
        ToolDescription::new("screen_draw", self.sanitized_description_template())
    }
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(ToolScope::Write),
            ..Default::default()
        }
    }
    async fn run(
        &self,
        ctx: ToolCallContext,
        input: ScreenDrawInput,
    ) -> Result<ScreenDrawOutput, ToolError> {
        validate(&input.shapes)?;
        let painter = {
            let resources = shared_resources(&ctx)?;
            let mut resources = resources.lock().await;
            if resources.get::<PainterBackend>().is_none() {
                let scope = resources
                    .get::<ProcessScope>()
                    .cloned()
                    .unwrap_or_else(ProcessScope::new);
                resources.insert(PainterBackend::new(HyprlandPainter {
                    scope,
                    overlay: Mutex::default(),
                }));
            }
            resources.require::<PainterBackend>()?.0.clone()
        };
        painter.screen_draw(&input).await
    }
}

/// Draws through one `grok-screen-overlay` on the monitor `screen_read` reads.
struct HyprlandPainter {
    scope: ProcessScope,
    overlay: Mutex<Option<Overlay>>,
}

#[async_trait]
impl ScreenPainter for HyprlandPainter {
    async fn screen_draw(&self, input: &ScreenDrawInput) -> Result<ScreenDrawOutput, ToolError> {
        let shapes = &input.shapes;
        let mut slot = self.overlay.lock().await;
        if shapes.is_empty() {
            *slot = None;
            return Ok(ScreenDrawOutput::Cleared);
        }
        let Some(hyprland) = Hyprland::session() else {
            return Err(failure("Drawing requires a Hyprland session"));
        };
        let monitor = hyprland.active_monitor().await?;
        if hyprland.locked().await {
            return Err(failure("The screen is locked"));
        }
        fit(shapes, &monitor)?;
        let request = OverlayRequest {
            monitor: monitor.name.clone(),
            style: Style::current(hyprland).await,
            shapes: shapes.clone(),
            repeat: input.repeat,
        };
        // The overlay stays out of the slot until it answers. A failed or canceled call
        // drops it, which kills the process and removes its drawing.
        let running = slot
            .take()
            .and_then(|mut overlay| overlay.running().then_some(overlay));
        let mut overlay = match running {
            Some(overlay) => overlay,
            None => Overlay::spawn(&overlay_executable()?, &self.scope)?,
        };
        tokio::time::timeout(SHOW_TIMEOUT, overlay.show(&request))
            .await
            .map_err(|_| {
                ToolError::new(
                    ToolErrorKind::Timeout,
                    "Screen draw: the drawing did not appear in time",
                )
            })??;
        *slot = Some(overlay);
        let (width, height) = monitor.logical_size();
        Ok(ScreenDrawOutput::Drawn {
            monitor: monitor.name,
            width,
            height,
            shapes: shapes.len(),
        })
    }
}

/// `GROK_SCREEN_OVERLAY`, else `grok-screen-overlay` beside the running binary (or one
/// directory up, for tests and examples), else on PATH.
fn overlay_executable() -> Result<PathBuf, ToolError> {
    if let Some(path) = std::env::var_os("GROK_SCREEN_OVERLAY") {
        return Ok(path.into());
    }
    std::env::current_exe()
        .ok()
        .and_then(|binary| {
            binary
                .ancestors()
                .skip(1)
                .take(2)
                .map(|directory| directory.join(OVERLAY))
                .find(|path| path.is_file())
        })
        .or_else(|| which::which(OVERLAY).ok())
        .ok_or_else(|| {
            failure(
                "Cannot find grok-screen-overlay; build it with `just build` or set GROK_SCREEN_OVERLAY",
            )
        })
}

/// A running `grok-screen-overlay`. Requests and replies are single JSON lines.
struct Overlay {
    child: Child,
    group: Arc<ProcessGroup>,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

/// The overlay's answer once the compositor shows a drawing, or why it cannot.
#[derive(Deserialize)]
struct OverlayReply {
    #[serde(default)]
    error: Option<String>,
}

impl Overlay {
    fn spawn(executable: &std::path::Path, scope: &ProcessScope) -> Result<Self, ToolError> {
        let mut command = Command::new(executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let (mut child, group) = scope
            .spawn(command)
            .map_err(|error| failure(format!("Cannot start {}: {error}", executable.display())))?;
        if !global_process_scope().register(&group) {
            return Err(failure("The session is closing"));
        }
        let input = child.stdin.take().ok_or_else(|| failure("Missing stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| failure("Missing stdout"))?;
        Ok(Self {
            child,
            group,
            input,
            output: BufReader::new(output),
        })
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    async fn show(&mut self, request: &OverlayRequest) -> Result<(), ToolError> {
        let mut line = serde_json::to_vec(request).map_err(failure)?;
        line.push(b'\n');
        self.input.write_all(&line).await.map_err(failure)?;
        self.input.flush().await.map_err(failure)?;
        let mut reply = Vec::new();
        (&mut self.output)
            .take(MAX_REPLY)
            .read_until(b'\n', &mut reply)
            .await
            .map_err(failure)?;
        if reply.last() != Some(&b'\n') {
            return Err(failure(
                "The screen overlay stopped; the compositor may not support layer-shell surfaces",
            ));
        }
        let reply: OverlayReply = serde_json::from_slice(&reply).map_err(failure)?;
        reply.error.map_or(Ok(()), |error| Err(failure(error)))
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // Kill the whole group: its surface, and with it the drawing, goes away.
        let _ = self.group.kill();
    }
}

fn failure(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(ToolErrorKind::Execution, format!("Screen draw: {error}"))
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;
    use xai_tool_runtime::{Tool, ToolError};

    use super::{
        Arrows, Color, PainterBackend, Palette, ScreenDrawInput, ScreenDrawOutput, ScreenDrawTool,
        ScreenPainter, Shape, fit, validate,
    };
    use crate::implementations::grok_build::hyprland::fixtures::monitor;
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::test_ctx;

    fn shapes(value: serde_json::Value) -> Vec<Shape> {
        serde_json::from_value::<ScreenDrawInput>(json!({ "shapes": value }))
            .unwrap()
            .shapes
    }

    fn error(result: Result<(), ToolError>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn input_takes_three_shapes_and_theme_colors() {
        let drawing = shapes(json!([
            {"kind": "box", "x": 104, "y": 82, "width": 120, "height": 36, "label": "Click Save"},
            {"kind": "line", "x": 400, "y": 300, "to_x": 230, "to_y": 100, "color": "red", "arrows": "end"},
            {"kind": "text", "x": 420, "y": 300, "text": "Then choose\na folder"}
        ]));
        assert!(matches!(
            drawing[0],
            Shape::Box {
                color: Color::Accent,
                ..
            }
        ));
        assert!(matches!(
            drawing[1],
            Shape::Line {
                color: Color::Red,
                arrows: Arrows::End,
                ..
            }
        ));
        assert!(validate(&drawing).is_ok());
        for bad in [
            json!({}),
            json!({"shapes": [], "monitor": "eDP-1"}),
            json!({"shapes": [{"kind": "circle", "x": 1, "y": 1}]}),
            json!({"shapes": [{"kind": "box", "x": 1, "y": 1, "width": 1, "height": 1, "color": "#ff0000"}]}),
        ] {
            assert!(serde_json::from_value::<ScreenDrawInput>(bad).is_err());
        }
    }

    #[test]
    fn validation_names_the_shape_and_the_rule() {
        let box_shape = |width: f64, label: &str| {
            shapes(
                json!([{"kind": "box", "x": 0, "y": 0, "width": width, "height": 10, "label": label}]),
            )
        };
        assert_eq!(
            error(validate(&box_shape(0.0, "ok"))),
            "Shape 1 (box): width and height must be positive"
        );
        assert!(error(validate(&box_shape(10.0, " "))).contains("must not be empty"));
        assert!(error(validate(&box_shape(10.0, "two\nlines"))).contains("control characters"));
        assert!(error(validate(&box_shape(10.0, &"x".repeat(201)))).contains("at most 200"));
        let line = shapes(json!([{"kind": "line", "x": 5, "y": 5, "to_x": 5, "to_y": 5}]));
        assert!(error(validate(&line)).contains("must have length"));
        let far = shapes(json!([{"kind": "text", "x": 1e9, "y": 0, "text": "far"}]));
        assert!(error(validate(&far)).contains("finite numbers"));
        let many =
            vec![shapes(json!([{"kind": "text", "x": 0, "y": 0, "text": "a"}]))[0].clone(); 65];
        assert!(error(validate(&many)).contains("at most 64"));
    }

    #[test]
    fn every_point_must_be_on_the_monitor() {
        let monitor = monitor();
        let edge = shapes(json!([{"kind": "box", "x": 0, "y": 0, "width": 1280, "height": 800}]));
        assert!(fit(&edge, &monitor).is_ok());
        let past = shapes(json!([
            {"kind": "text", "x": 10, "y": 10, "text": "fits"},
            {"kind": "line", "x": 10, "y": 10, "to_x": 1300, "to_y": 10}
        ]));
        assert_eq!(
            error(fit(&past, &monitor)),
            "Shape 2 (line): it extends past monitor eDP-1, which is 1280 × 800 logical units"
        );
    }

    #[test]
    fn scenes_check_their_camera_objects_and_meshes() {
        let scene = |camera: serde_json::Value, objects: serde_json::Value| {
            shapes(json!([{
                "kind": "scene", "x": 0, "y": 0, "width": 300, "height": 200,
                "camera": camera, "objects": objects
            }]))
        };
        let triangle = json!([{
            "kind": "mesh", "vertices": [[0, 0, 0], [1, 0, 0], [0, 1, 0]], "faces": [[0, 1, 2]]
        }]);
        assert!(validate(&scene(json!({}), triangle.clone())).is_ok());
        assert!(error(validate(&scene(json!({"pitch": 91}), triangle))).contains("pitch"));
        let bad_index = json!([{
            "kind": "mesh", "vertices": [[0, 0, 0]], "faces": [[0, 1, 2]]
        }]);
        assert_eq!(
            error(validate(&scene(json!({}), bad_index))),
            "Shape 1 (scene): mesh faces must use existing vertex indices"
        );
        let flat = json!([{"kind": "cuboid", "center": [0, 0, 0], "size": [1, 0, 1]}]);
        assert!(error(validate(&scene(json!({}), flat))).contains("sizes must be positive"));
        assert!(error(validate(&scene(json!({}), json!([])))).contains("1 to 64 objects"));
    }

    #[test]
    fn fill_is_an_opacity_and_a_box_must_stay_visible() {
        let box_shape = |extra: serde_json::Value| {
            let mut shape = json!({"kind": "box", "x": 0, "y": 0, "width": 10, "height": 10});
            shape
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            shapes(json!([shape]))
        };
        assert!(validate(&box_shape(json!({"fill": 1, "border": false}))).is_ok());
        assert!(validate(&box_shape(json!({"fill": 0}))).is_ok());
        assert!(matches!(
            box_shape(json!({}))[0],
            Shape::Box {
                fill: None,
                border: true,
                ..
            }
        ));
        assert_eq!(
            error(validate(&box_shape(json!({"fill": 1.5})))),
            "Shape 1 (box): fill must be between 0 and 1"
        );
        assert!(
            error(validate(&box_shape(json!({"fill": 0, "border": false}))))
                .contains("needs a fill above 0")
        );
    }

    #[test]
    fn palette_reads_an_omarchy_theme_and_keeps_defaults_for_gaps() {
        let palette = Palette::parse(
            "mode = \"dark\"\n\naccent = \"#faa968\"\nbackground = \"#05182e\"\nred = \"#f85525\"\n\
             foreground = \"#zzzzzz\"\nbright_red = \"#000000\"\n",
        );
        assert_eq!(palette.rgb(Color::Accent), [0xfa, 0xa9, 0x68]);
        assert_eq!(palette.rgb(Color::Red), [0xf8, 0x55, 0x25]);
        assert_eq!(palette.background, [0x05, 0x18, 0x2e]);
        assert_eq!(palette.foreground, Palette::default().foreground);
        assert_eq!(palette.rgb(Color::Green), Palette::default().green);
    }

    #[test]
    fn output_tells_the_model_what_the_screen_shows() {
        let drawn = ScreenDrawOutput::Drawn {
            monitor: "eDP-1".to_owned(),
            width: 1280.0,
            height: 800.0,
            shapes: 1,
        };
        assert_eq!(
            drawn.to_string(),
            "Drew 1 shape on monitor eDP-1 (1280 × 800 logical units). \
             The drawing stays until the next screen_draw call or the end of the session."
        );
        assert_eq!(
            ScreenDrawOutput::Cleared.to_string(),
            "Cleared the drawing."
        );
    }

    struct FakePainter;

    #[async_trait]
    impl ScreenPainter for FakePainter {
        async fn screen_draw(
            &self,
            input: &ScreenDrawInput,
        ) -> Result<ScreenDrawOutput, ToolError> {
            Ok(ScreenDrawOutput::Drawn {
                monitor: "fake".to_owned(),
                width: 100.0,
                height: 100.0,
                shapes: input.shapes.len(),
            })
        }
    }

    #[tokio::test]
    async fn installed_backend_draws_after_validation() {
        let mut resources = Resources::new();
        resources.insert(PainterBackend::new(FakePainter));
        let ctx = test_ctx(resources.into_shared());
        let input = ScreenDrawInput {
            shapes: shapes(json!([{"kind": "text", "x": 1, "y": 1, "text": "hi"}])),
            ..Default::default()
        };
        let output = ScreenDrawTool.run(ctx.clone(), input).await.unwrap();
        assert!(matches!(output, ScreenDrawOutput::Drawn { shapes: 1, .. }));
        let invalid = ScreenDrawInput {
            shapes: shapes(json!([{"kind": "text", "x": 1, "y": 1, "text": ""}])),
            ..Default::default()
        };
        assert!(ScreenDrawTool.run(ctx, invalid).await.is_err());
    }

    #[test]
    fn keys_move_fields_through_time() {
        use super::{Repeat, Style};

        let shape = &shapes(json!([{
            "kind": "box", "x": 0, "y": 0, "width": 10, "height": 10,
            "keys": [{"at": 2, "x": 100}, {"at": 4, "x": 0, "ease": "step"}]
        }]))[0];
        let x = |t: f64| match shape.at(t, &Style::default()) {
            Shape::Box { x, .. } => x,
            other => panic!("expected a box, got {other:?}"),
        };
        assert_eq!([x(1.0), x(3.0), x(4.0), x(9.0)], [50.0, 100.0, 0.0, 0.0]);
        assert_eq!(Repeat::Once.time(5.0, 4.0), (4.0, true));
        assert_eq!(Repeat::Loop.time(5.0, 4.0), (1.0, false));
        assert_eq!(Repeat::Bounce.time(5.0, 4.0), (3.0, false));
        let line = |keys: serde_json::Value| {
            shapes(json!([{"kind": "line", "x": 0, "y": 0, "to_x": 5, "to_y": 5, "keys": keys}]))
        };
        assert_eq!(
            error(validate(&line(json!([{"at": 1, "width": 3}])))),
            "Shape 1 (line): a line key cannot set width"
        );
        let backward = line(json!([{"at": 2, "x": 1}, {"at": 1, "x": 2}]));
        assert!(error(validate(&backward)).contains("key times must rise"));
        let opaque = line(json!([{"at": 1, "opacity": 2}]));
        assert!(error(validate(&opaque)).contains("at 1 s, opacity must be between 0 and 1"));
    }

    #[test]
    fn charts_check_their_data_and_fit_their_axes() {
        use super::Style;

        let chart = |extra: serde_json::Value| {
            let mut chart = json!({
                "kind": "chart", "x": 0, "y": 0, "width": 400, "height": 300, "plot": "bar",
                "categories": ["a", "b"], "series": [{"values": [3, 7]}, {"values": [-2, 4]}]
            });
            let fields = chart.as_object_mut().unwrap();
            fields.extend(extra.as_object().unwrap().clone());
            shapes(json!([chart]))
        };
        let only = |shapes: Vec<Shape>| match shapes.into_iter().next() {
            Some(Shape::Chart(chart)) => chart,
            other => panic!("expected a chart, got {other:?}"),
        };
        validate(&chart(json!({}))).unwrap();
        let [_, values, _] = only(chart(json!({}))).scales();
        assert_eq!((values.min, values.max, values.step), (-2.0, 8.0, 2.0));
        let stacked = only(chart(json!({"stacked": true})));
        assert_eq!(stacked.stacks()[1], [(0.0, -2.0), (7.0, 11.0)]);
        assert_eq!(stacked.scales()[1].max, 12.0);

        let unknown = json!({"shapes": [{"kind": "chart", "x": 0, "y": 0, "width": 9, "height": 9,
            "plot": "bar", "series": [], "colour": "red"}]});
        assert!(serde_json::from_value::<ScreenDrawInput>(unknown).is_err());
        assert_eq!(
            error(validate(&chart(json!({"categories": ["a"]})))),
            "Shape 1 (chart): each series needs one value per category"
        );
        assert!(error(validate(&chart(json!({"plot": "pie"})))).contains("a pie takes one series"));
        let surface = chart(json!({"plot": "surface"}));
        assert!(error(validate(&surface)).contains("a grid plot takes one series"));

        let keys = json!([{"at": 1, "series": [{"values": [1, 30]}]}]);
        let keyed = chart(json!({"series": [{"values": [1, 2]}], "keys": keys}));
        validate(&keyed).unwrap();
        let mut keyed = only(keyed);
        keyed.pin_axes();
        assert_eq!(
            (keyed.y_axis.min, keyed.y_axis.max),
            (Some(0.0), Some(30.0))
        );
        let halfway = only(vec![Shape::Chart(keyed).at(0.5, &Style::default())]);
        assert_eq!(halfway.series[0].values, [1.0, 16.0]);

        let candles = |points: serde_json::Value| {
            chart(json!({"plot": "candlestick", "series": [{"points": points}]}))
        };
        let rising = only(candles(json!([[10, 14, 9, 13], [13, 13, 6, 7]])));
        assert_eq!(
            (rising.scales()[1].min, rising.scales()[1].max),
            (6.0, 14.0)
        );
        let high_too_low = candles(json!([[10, 12, 9, 13], [13, 13, 6, 7]]));
        assert!(error(validate(&high_too_low)).contains("high the highest"));

        let sankey = |links: serde_json::Value| {
            shapes(
                json!([{"kind": "chart", "x": 0, "y": 0, "width": 400, "height": 300,
                "plot": "sankey", "links": links}]),
            )
        };
        let flows = sankey(json!([
            {"from": "pay", "to": "rent", "value": 40}, {"from": "pay", "to": "saved", "value": 20},
            {"from": "bonus", "to": "saved", "value": 10}, {"from": "rent", "to": "landlord", "value": 40},
        ]));
        validate(&flows).unwrap();
        let nodes = only(flows).nodes().unwrap();
        let placed: Vec<_> = nodes
            .iter()
            .map(|n| (n.name.as_str(), n.column, n.value))
            .collect();
        assert_eq!(
            placed,
            [
                ("pay", 0, 60.0),
                ("rent", 1, 40.0),
                ("saved", 1, 30.0),
                ("bonus", 0, 10.0),
                ("landlord", 2, 40.0),
            ]
        );
        let looped = sankey(
            json!([{"from": "a", "to": "b", "value": 1}, {"from": "b", "to": "a", "value": 1}]),
        );
        assert!(error(validate(&looped)).contains("must not loop"));
        assert!(
            error(validate(&chart(
                json!({"links": [{"from": "a", "to": "b", "value": 1}]})
            )))
            .contains("only a sankey")
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn overlay_answers_each_request_and_dies_with_its_handle() {
        use std::os::unix::fs::PermissionsExt;

        use super::{Overlay, OverlayRequest, Style};
        use crate::util::ProcessScope;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("overlay");
        // Answers the first request, rejects the second, and records its PID.
        let script = "#!/bin/sh\nprintf '%s' \"$$\" > \"$0.pid\"\nread -r _; echo '{}'\nread -r _; echo '{\"error\":\"Unknown monitor\"}'\nsleep 60\n";
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = OverlayRequest {
            monitor: "eDP-1".to_owned(),
            style: Style::default(),
            shapes: shapes(json!([{"kind": "text", "x": 1, "y": 1, "text": "hi"}])),
            repeat: Default::default(),
        };
        // Dropping a scope kills its processes, so keep it for the whole test.
        let scope = ProcessScope::new();
        let mut overlay = Overlay::spawn(&executable, &scope).unwrap();
        overlay.show(&request).await.unwrap();
        let rejected = overlay.show(&request).await.unwrap_err().to_string();
        assert_eq!(rejected, "Screen draw: Unknown monitor");
        let pid = std::fs::read_to_string(directory.path().join("overlay.pid")).unwrap();
        drop(overlay);
        let stat = format!("/proc/{pid}/stat");
        for _ in 0..100 {
            let alive = std::fs::read_to_string(&stat).is_ok_and(|stat| {
                stat.rsplit_once(')')
                    .is_some_and(|(_, rest)| rest.split_whitespace().next() != Some("Z"))
            });
            if !alive {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("dropping the overlay left process {pid} running");
    }

    #[test]
    fn tool_is_registered_as_a_permissioned_write() {
        use crate::registry::types::ToolRegistryBuilder;
        use crate::types::{ToolInput, tool::ToolKind};
        use xai_grok_permission_rules::types::AccessKind;
        let kinds = ToolRegistryBuilder::new().known_tool_kinds();
        assert_eq!(kinds.get("GrokBuild:screen_draw"), Some(&ToolKind::Other));
        let input: ToolInput = ScreenDrawInput::default().into();
        assert!(
            matches!(AccessKind::from(&input), AccessKind::Tool(name) if name == "screen_draw")
        );
        assert!(!ScreenDrawTool.capabilities().is_read_only);
    }
}
