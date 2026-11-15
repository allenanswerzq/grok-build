//! Shapes and 3D scene objects, and the rules each one obeys.

use serde::{Deserialize, Serialize};

use super::{Chart, Key, ObjectKey, Style};

pub(crate) const MAX_SHAPES: usize = 64;
const MAX_TEXT_CHARS: usize = 200;
const MAX_COORDINATE: f64 = 100_000.0;
const MAX_SCENE_OBJECTS: usize = 64;
/// Limit for the vertices, and separately the faces, of all meshes in one scene.
const MAX_MESH_SIZE: usize = 2_000;
/// Default fill opacities: a faint tint for boxes and nearly solid backdrops for scenes and
/// text.
const BOX_FILL: f64 = 0.08;
pub(crate) const SCENE_FILL: f64 = 0.92;
const TEXT_FILL: f64 = 0.92;
/// Accepted line widths and text sizes, in logical units.
pub(crate) const LINE_WIDTHS: std::ops::RangeInclusive<f64> = 0.5..=20.0;
const TEXT_SIZES: std::ops::RangeInclusive<f64> = 8.0..=96.0;

/// A shape in logical units from the monitor's top-left corner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "a drawing holds at most 64 shapes"
)]
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

pub(crate) fn outlined() -> bool {
    true
}

/// A 3D point or size: [x, y, z]. y points up, and +z faces the default front view.
pub type Point3 = [f64; 3];

/// Where a scene is seen from. The camera circles the objects and looks at their center.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn ends(self) -> (bool, bool) {
        (
            matches!(self, Self::Start | Self::Both),
            matches!(self, Self::End | Self::Both),
        )
    }
}

/// How a line is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

/// A color of the current Omarchy theme. `accent` matches the focused window's border.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Box { .. } => "box",
            Self::Line { .. } => "line",
            Self::Text { .. } => "text",
            Self::Scene { .. } => "scene",
            Self::Chart(_) => "chart",
        }
    }

    /// The whole shape's opacity, from 0 to 1.
    pub fn opacity(&self) -> f64 {
        match *self {
            Self::Box { opacity, .. }
            | Self::Line { opacity, .. }
            | Self::Text { opacity, .. }
            | Self::Scene { opacity, .. } => opacity.unwrap_or(1.0),
            Self::Chart(ref chart) => chart.opacity.unwrap_or(1.0),
        }
    }

    /// Fill opacity, from 0 to 1: a box's tint, a text panel, or a scene or chart backdrop.
    /// Lines have none.
    pub fn fill(&self) -> f64 {
        match *self {
            Self::Box { fill, .. } => fill.unwrap_or(BOX_FILL),
            Self::Line { .. } => 0.0,
            Self::Text { fill, .. } => fill.unwrap_or(TEXT_FILL),
            Self::Scene { fill, .. } => fill.unwrap_or(SCENE_FILL),
            Self::Chart(ref chart) => chart.fill(),
        }
    }

    /// Line width in logical units: an outline, or a line itself, which is a bit wider by
    /// default.
    pub fn line_width(&self, style: &Style) -> f64 {
        match *self {
            Self::Box { line_width, .. } | Self::Scene { line_width, .. } => {
                line_width.unwrap_or(style.line_width)
            }
            Self::Line { line_width, .. } => line_width.unwrap_or(style.line_width * 1.5),
            Self::Text { .. } => style.line_width,
            Self::Chart(ref chart) => chart.line_width(style),
        }
    }

    /// Text size in logical units.
    pub fn text_size(&self, style: &Style) -> f64 {
        match *self {
            Self::Text { size, .. } => size.unwrap_or(style.font_size),
            _ => style.font_size,
        }
    }

    /// The frame, as (x, y, width, height), and camera of a shape the user can orbit and
    /// zoom: a scene or a 3D chart.
    pub fn orbit(&self) -> Option<((f64, f64, f64, f64), Camera)> {
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

    /// Whether the shape stays inside a `width` × `height` area, edges included, at every
    /// key.
    pub fn fits(&self, width: f64, height: f64) -> bool {
        let style = Style::default();
        let inside =
            |&(x, y): &(f64, f64)| (0.0..=width).contains(&x) && (0.0..=height).contains(&y);
        std::iter::once(0.0)
            .chain(self.times())
            .all(|t| self.at(t, &style).points().iter().all(inside))
    }

    /// The points that must lie on the monitor: frame corners, line ends, or the text anchor.
    pub(crate) fn points(&self) -> Vec<(f64, f64)> {
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

    pub(crate) fn check(&self) -> Result<(), &'static str> {
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
    pub fn rotation(&self) -> Option<Point3> {
        match *self {
            Self::Cuboid { rotation, .. }
            | Self::Cylinder { rotation, .. }
            | Self::Line { rotation, .. }
            | Self::Mesh { rotation, .. } => rotation,
            Self::Sphere { .. } => None,
        }
    }

    pub(crate) fn check(&self) -> Result<(), &'static str> {
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

pub(crate) fn finite(number: f64) -> bool {
    number.is_finite() && number.abs() <= MAX_COORDINATE
}

pub(crate) fn check_text(text: &str, multiline: bool) -> Result<(), &'static str> {
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
