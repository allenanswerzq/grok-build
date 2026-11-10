//! `screen_draw`: draw boxes, lines and arrows, text, and small 3D scenes on the screen.
//!
//! Shapes use `screen_read`'s logical units: (x, y) from the active monitor's top-left
//! corner. Each call replaces the whole drawing, and an empty list clears it.
//!
//! The tool validates shapes, reads the desktop's look ([`Style`]), and sends one
//! [`OverlayRequest`] line to `grok-screen-overlay`, a small Wayland process in `overlay/`
//! that shows the drawing above all windows. Clicks pass through everywhere except scenes,
//! which the user can drag to orbit and scroll to zoom. It lives only while a drawing is
//! visible: clearing, cancellation, or the end of the session kills it, which removes the
//! drawing.
//!
//! `just build` builds the overlay beside the CLI. Linux builds need pkg-config and the
//! Wayland and libxkbcommon development libraries; at run time the overlay needs a layer-shell
//! compositor such as Hyprland.

pub mod overlay;

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScreenDrawInput {
    /// Shapes that replace the current drawing. Use [] to clear the screen.
    pub shapes: Vec<Shape>,
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
    },
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
    },
    Sphere {
        center: Point3,
        radius: f64,
        #[serde(default)]
        color: Color,
    },
    /// A round rod between the centers of its two ends.
    Cylinder {
        from: Point3,
        to: Point3,
        radius: f64,
        #[serde(default)]
        color: Color,
    },
    Line {
        from: Point3,
        to: Point3,
        #[serde(default)]
        color: Color,
    },
    /// Triangles; each face lists three indices into `vertices`.
    Mesh {
        vertices: Vec<Point3>,
        faces: Vec<[usize; 3]>,
        #[serde(default)]
        color: Color,
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
        }
    }

    fn check(&self) -> Result<(), &'static str> {
        if !self.points().iter().all(|&(x, y)| finite(x) && finite(y)) {
            return Err("coordinates must be finite numbers");
        }
        match self {
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
            _ => Ok(()),
        }
    }
}

impl SceneObject {
    fn check(&self) -> Result<(), &'static str> {
        let numbers: Vec<f64> = match self {
            Self::Cuboid { center, size, .. } => center.iter().chain(size).copied().collect(),
            Self::Sphere { center, radius, .. } => center.iter().chain([radius]).copied().collect(),
            Self::Cylinder {
                from, to, radius, ..
            } => from.iter().chain(to).chain([radius]).copied().collect(),
            Self::Line { from, to, .. } => from.iter().chain(to).copied().collect(),
            Self::Mesh { vertices, .. } => vertices.iter().flatten().copied().collect(),
        };
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
    }
    Ok(())
}

/// Reject shapes that leave the monitor. Points on its edge fit.
fn fit(shapes: &[Shape], monitor: &Monitor) -> Result<(), ToolError> {
    for (index, shape) in shapes.iter().enumerate() {
        if !shape.points().iter().all(|&(x, y)| monitor.contains(x, y)) {
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
    /// Replace the drawing on the active monitor. An empty list clears it.
    ///
    /// # Errors
    /// Returns an error when the desktop cannot draw or a shape leaves the monitor.
    async fn screen_draw(&self, shapes: &[Shape]) -> Result<ScreenDrawOutput, ToolError>;
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
        "Draw on the user's screen to point at or explain something: boxes, lines and arrows, short text, and small 3D scenes on the active monitor, above all windows. Clicks and typing pass through to the windows below, except that the user can drag a scene to orbit around it and scroll over it to zoom. Positions use screen_read's logical units: (x, y) from the monitor's top-left corner. To mark a point in a screen_read window screenshot, add the window's x and y. A scene is a 3D view of cuboids, spheres, cylinders, lines, and triangle meshes in one space where y points up; the camera circles them by yaw and pitch in degrees and fits them in the frame. A line runs from (x, y) to (to_x, to_y); arrows puts a head on its start, end, or both ends, and pattern makes it solid, dashed, or dotted. Boxes, scenes, and text panels take a fill opacity from 0 to 1 and an optional border: an outline with a faint fill highlights what is under a box, fill 1 covers it, a scene with fill 0 and no border floats its objects over the windows, and text without a border shows its words in its color. Boxes, lines, and scenes take a line_width and text takes a size, both in logical units; the defaults follow the window border and a 14-unit font. Colors come from the user's theme; accent, the default, matches the focused window's border. Each call replaces the previous drawing; call with an empty list to clear it. The drawing also clears when the session ends."
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
        painter.screen_draw(&input.shapes).await
    }
}

/// Draws through one `grok-screen-overlay` on the monitor `screen_read` reads.
struct HyprlandPainter {
    scope: ProcessScope,
    overlay: Mutex<Option<Overlay>>,
}

#[async_trait]
impl ScreenPainter for HyprlandPainter {
    async fn screen_draw(&self, shapes: &[Shape]) -> Result<ScreenDrawOutput, ToolError> {
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
            shapes: shapes.to_vec(),
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
        async fn screen_draw(&self, shapes: &[Shape]) -> Result<ScreenDrawOutput, ToolError> {
            Ok(ScreenDrawOutput::Drawn {
                monitor: "fake".to_owned(),
                width: 100.0,
                height: 100.0,
                shapes: shapes.len(),
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
        };
        let output = ScreenDrawTool.run(ctx.clone(), input).await.unwrap();
        assert!(matches!(output, ScreenDrawOutput::Drawn { shapes: 1, .. }));
        let invalid = ScreenDrawInput {
            shapes: shapes(json!([{"kind": "text", "x": 1, "y": 1, "text": ""}])),
        };
        assert!(ScreenDrawTool.run(ctx, invalid).await.is_err());
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
        let input: ToolInput = ScreenDrawInput { shapes: Vec::new() }.into();
        assert!(
            matches!(AccessKind::from(&input), AccessKind::Tool(name) if name == "screen_draw")
        );
        assert!(!ScreenDrawTool.capabilities().is_read_only);
    }
}
