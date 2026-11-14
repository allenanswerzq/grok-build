//! Animation: keys change shapes and scene objects over time.
//!
//! A shape's own fields are its values at the start. Each key gives some fields the values
//! they reach `at` seconds in. A field moves there from the previous key that sets it, or
//! from its own value, eased as the key says, and holds after its last key. Numbers and 3D
//! points move smoothly, and so does chart data of the same size; colors, text, and data of
//! another size switch when their key arrives. The timeline lasts until the latest key, and
//! [`Repeat`] says how it plays.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    BOX_FILL, Chart, Color, Point3, SCENE_FILL, SceneObject, Series, Shape, Style, TEXT_FILL,
};

/// The longest timeline, and the most keys on one shape or object.
const MAX_SECONDS: f64 = 60.0;
const MAX_KEYS: usize = 64;

/// How the timeline plays.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    /// Once, then the last frame stays.
    #[default]
    Once,
    /// From the start again, without end.
    Loop,
    /// Forward, then backward, without end.
    Bounce,
}

/// How a field moves toward a key's value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    /// At a constant speed.
    #[default]
    Linear,
    /// Starting and stopping gently.
    InOut,
    /// Not at all until the key's time, then at once.
    Step,
}

/// Values some of a shape's fields reach `at` seconds in. A key sets only fields its shape
/// has; `yaw` and `pitch` turn a scene's or chart's camera, and `series` replaces a chart's
/// data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Key {
    /// Seconds from the start. Keys go in time order.
    pub at: f64,
    pub ease: Ease,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<Vec<Series>>,
}

/// Values some of a scene object's fields reach `at` seconds in. A key sets only fields its
/// object has.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ObjectKey {
    /// Seconds from the start. Keys go in time order.
    pub at: f64,
    pub ease: Ease,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<Point3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<Point3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Point3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Point3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Point3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
}

impl Repeat {
    /// Where a timeline `seconds` long is after `elapsed` seconds, and whether it stays there.
    pub(super) fn time(self, elapsed: f64, seconds: f64) -> (f64, bool) {
        if seconds <= 0.0 {
            return (0.0, true);
        }
        match self {
            Self::Once => (elapsed.min(seconds), elapsed >= seconds),
            Self::Loop => (elapsed % seconds, false),
            Self::Bounce => (seconds - (elapsed % (2.0 * seconds) - seconds).abs(), false),
        }
    }
}

impl Ease {
    /// How much of the way a field has moved after `share` (0 to 1) of its time.
    fn apply(self, share: f64) -> f64 {
        match self {
            Self::Linear => share,
            Self::InOut => share * share * (3.0 - 2.0 * share),
            Self::Step => 0.0,
        }
    }
}

trait Timed {
    fn timing(&self) -> (f64, Ease);
}

impl Timed for Key {
    fn timing(&self) -> (f64, Ease) {
        (self.at, self.ease)
    }
}

impl Timed for ObjectKey {
    fn timing(&self) -> (f64, Ease) {
        (self.at, self.ease)
    }
}

/// A value that can move part of the way to another.
trait Tween: Clone {
    /// `self` moved `share` (0 to 1) of the way to `to`. Values that cannot blend keep
    /// theirs until the key arrives.
    fn tween(&self, _to: &Self, _share: f64) -> Self {
        self.clone()
    }
}

impl Tween for f64 {
    fn tween(&self, to: &Self, share: f64) -> Self {
        self + (to - self) * share
    }
}

impl Tween for Point3 {
    fn tween(&self, to: &Self, share: f64) -> Self {
        std::array::from_fn(|axis| self[axis].tween(&to[axis], share))
    }
}

impl Tween for Color {}

impl Tween for String {}

/// Lists of the same length move item by item; others switch when the key arrives.
impl<T: Tween> Tween for Vec<T> {
    fn tween(&self, to: &Self, share: f64) -> Self {
        if self.len() != to.len() {
            return self.clone();
        }
        self.iter()
            .zip(to)
            .map(|(from, to)| from.tween(to, share))
            .collect()
    }
}

impl Tween for Series {
    fn tween(&self, to: &Self, share: f64) -> Self {
        Self {
            values: self.values.tween(&to.values, share),
            points: self.points.tween(&to.points, share),
            grid: self.grid.tween(&to.grid, share),
            ..self.clone()
        }
    }
}

/// Move `value` to time `t` through the keys that set it.
fn track<K: Timed, T: Tween>(value: &mut T, keys: &[K], t: f64, field: impl Fn(&K) -> Option<&T>) {
    let mut start = 0.0;
    for key in keys {
        let Some(to) = field(key) else {
            continue;
        };
        let (at, ease) = key.timing();
        if t < at {
            *value = value.tween(to, ease.apply((t - start) / (at - start)));
            return;
        }
        *value = to.clone();
        start = at;
    }
}

/// [`track`] for an optional field, which starts from `default` when unset.
fn track_or<K: Timed, T: Tween>(
    value: &mut Option<T>,
    default: T,
    keys: &[K],
    t: f64,
    field: impl Fn(&K) -> Option<&T>,
) {
    if keys.iter().any(|key| field(key).is_some()) {
        track(value.get_or_insert(default), keys, t, field);
    }
}

/// Move each named field to time `t` through `keys`. `field = default` names an optional
/// field and the value it starts from when unset.
macro_rules! keyed {
    (@ $keys:ident, $t:ident, $field:ident) => {
        track($field, &$keys[..], $t, |key| key.$field.as_ref())
    };
    (@ $keys:ident, $t:ident, $field:ident, $default:expr) => {
        track_or($field, $default, &$keys[..], $t, |key| key.$field.as_ref())
    };
    ($keys:ident, $t:ident: $($field:ident $(= $default:expr)?),* $(,)?) => {{
        $( keyed!(@ $keys, $t, $field $(, $default)?); )*
    }};
}

/// How long the shapes' timeline lasts: until its latest key.
pub(super) fn seconds(shapes: &[Shape]) -> f64 {
    shapes.iter().flat_map(Shape::times).fold(0.0, f64::max)
}

impl Shape {
    pub(super) fn keys(&self) -> &[Key] {
        match self {
            Self::Box { keys, .. }
            | Self::Line { keys, .. }
            | Self::Text { keys, .. }
            | Self::Scene { keys, .. } => keys,
            Self::Chart(chart) => &chart.keys,
        }
    }

    /// The times of the shape's keys and its objects' keys.
    pub(super) fn times(&self) -> Vec<f64> {
        let objects = match self {
            Self::Scene { objects, .. } => objects.as_slice(),
            _ => &[],
        };
        let object_keys = objects.iter().flat_map(SceneObject::keys).map(|key| key.at);
        self.keys()
            .iter()
            .map(|key| key.at)
            .chain(object_keys)
            .collect()
    }

    /// The shape `t` seconds in. Unset fields that keys move start from their defaults in
    /// `style`.
    pub(super) fn at(&self, t: f64, style: &Style) -> Self {
        let mut shape = self.clone();
        match &mut shape {
            Self::Box {
                x,
                y,
                width,
                height,
                color,
                fill,
                line_width,
                opacity,
                keys,
                ..
            } => keyed!(keys, t: x, y, width, height, color, fill = BOX_FILL,
                line_width = style.line_width, opacity = 1.0),
            Self::Line {
                x,
                y,
                to_x,
                to_y,
                color,
                line_width,
                opacity,
                keys,
                ..
            } => keyed!(keys, t: x, y, to_x, to_y, color,
                line_width = style.line_width * 1.5, opacity = 1.0),
            Self::Text {
                x,
                y,
                text,
                color,
                size,
                fill,
                opacity,
                keys,
                ..
            } => keyed!(keys, t: x, y, text, color, size = style.font_size, fill = TEXT_FILL,
                opacity = 1.0),
            Self::Scene {
                x,
                y,
                width,
                height,
                fill,
                line_width,
                opacity,
                camera,
                objects,
                keys,
                ..
            } => {
                keyed!(keys, t: x, y, width, height, fill = SCENE_FILL,
                    line_width = style.line_width, opacity = 1.0);
                track(&mut camera.yaw, &keys[..], t, |key| key.yaw.as_ref());
                track(&mut camera.pitch, &keys[..], t, |key| key.pitch.as_ref());
                for object in objects.iter_mut() {
                    *object = object.at(t);
                }
            }
            Self::Chart(chart) => *chart = chart.at(t, style),
        }
        shape
    }

    /// Keys run forward within the longest timeline, set only fields their shape or object
    /// has, and reach values the shape accepts.
    pub(super) fn check_keys(&self) -> Result<(), String> {
        check_timeline(self.keys(), self.kind(), self.keyable())?;
        if let Self::Scene { objects, .. } = self {
            for object in objects {
                check_timeline(object.keys(), object.kind(), object.keyable())?;
            }
        }
        let style = Style::default();
        self.times().into_iter().try_for_each(|t| {
            (self.at(t, &style).check()).map_err(|problem| format!("at {t} s, {problem}"))
        })
    }

    /// The fields a key of this shape may set.
    fn keyable(&self) -> &'static [&'static str] {
        match self {
            Self::Box { .. } => &[
                "x",
                "y",
                "width",
                "height",
                "color",
                "fill",
                "opacity",
                "line_width",
            ],
            Self::Line { .. } => &["x", "y", "to_x", "to_y", "color", "opacity", "line_width"],
            Self::Text { .. } => &["x", "y", "text", "color", "size", "fill", "opacity"],
            Self::Scene { .. } => &[
                "x",
                "y",
                "width",
                "height",
                "fill",
                "opacity",
                "line_width",
                "yaw",
                "pitch",
            ],
            Self::Chart(_) => &[
                "x",
                "y",
                "width",
                "height",
                "fill",
                "opacity",
                "line_width",
                "yaw",
                "pitch",
                "series",
            ],
        }
    }
}

impl Chart {
    /// The chart `t` seconds in.
    pub(super) fn at(&self, t: f64, style: &Style) -> Self {
        let mut chart = self.clone();
        let Self {
            x,
            y,
            width,
            height,
            series,
            fill,
            line_width,
            opacity,
            camera,
            keys,
            ..
        } = &mut chart;
        keyed!(keys, t: x, y, width, height, series, fill = SCENE_FILL,
            line_width = style.line_width, opacity = 1.0);
        track(&mut camera.yaw, &keys[..], t, |key| key.yaw.as_ref());
        track(&mut camera.pitch, &keys[..], t, |key| key.pitch.as_ref());
        chart
    }
}

impl SceneObject {
    fn keys(&self) -> &[ObjectKey] {
        match self {
            Self::Cuboid { keys, .. }
            | Self::Sphere { keys, .. }
            | Self::Cylinder { keys, .. }
            | Self::Line { keys, .. }
            | Self::Mesh { keys, .. } => keys,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Cuboid { .. } => "cuboid",
            Self::Sphere { .. } => "sphere",
            Self::Cylinder { .. } => "cylinder",
            Self::Line { .. } => "line",
            Self::Mesh { .. } => "mesh",
        }
    }

    /// The object `t` seconds in.
    pub(super) fn at(&self, t: f64) -> Self {
        let mut object = self.clone();
        match &mut object {
            Self::Cuboid {
                center,
                size,
                color,
                rotation,
                keys,
            } => keyed!(keys, t: center, size, color, rotation = [0.0; 3]),
            Self::Sphere {
                center,
                radius,
                color,
                keys,
            } => keyed!(keys, t: center, radius, color),
            Self::Cylinder {
                from,
                to,
                radius,
                color,
                rotation,
                keys,
            } => keyed!(keys, t: from, to, radius, color, rotation = [0.0; 3]),
            Self::Line {
                from,
                to,
                color,
                rotation,
                keys,
            } => keyed!(keys, t: from, to, color, rotation = [0.0; 3]),
            Self::Mesh {
                color,
                rotation,
                keys,
                ..
            } => keyed!(keys, t: color, rotation = [0.0; 3]),
        }
        object
    }

    /// The fields a key of this object may set.
    fn keyable(&self) -> &'static [&'static str] {
        match self {
            Self::Cuboid { .. } => &["center", "size", "color", "rotation"],
            Self::Sphere { .. } => &["center", "radius", "color"],
            Self::Cylinder { .. } => &["from", "to", "radius", "color", "rotation"],
            Self::Line { .. } => &["from", "to", "color", "rotation"],
            Self::Mesh { .. } => &["color", "rotation"],
        }
    }
}

/// Keys of one `kind` of shape or object: few enough, in time order within the longest
/// timeline, and setting only `keyable` fields.
fn check_timeline<K: Timed + Serialize>(
    keys: &[K],
    kind: &str,
    keyable: &[&str],
) -> Result<(), String> {
    if keys.len() > MAX_KEYS {
        return Err(format!("a {kind} takes at most {MAX_KEYS} keys"));
    }
    let mut last = 0.0;
    for key in keys {
        let (at, _) = key.timing();
        if !(last..=MAX_SECONDS).contains(&at) {
            return Err(format!(
                "key times must rise from 0 to at most {MAX_SECONDS} seconds"
            ));
        }
        last = at;
        if let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(key)
            && let Some(field) = fields.keys().find(|field| {
                !matches!(field.as_str(), "at" | "ease") && !keyable.contains(&field.as_str())
            })
        {
            return Err(format!("a {kind} key cannot set {field}"));
        }
    }
    Ok(())
}
