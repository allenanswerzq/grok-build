//! 3D scenes: lit triangles and a camera that circles the objects and fits them in the
//! scene's frame. Curved surfaces get smooth normals and flat faces keep sharp ones; lines
//! become thin rods with a constant width on screen. The GPU tests depth per pixel, so objects
//! may intersect and still look right.
//!
//! The user can orbit the camera of a scene or 3D chart and zoom it ([`View`]). The fit comes
//! from the scene's own camera, so the size holds steady while the user orbits; zoom scales
//! the fitted image about its middle, and the frame clips whatever ends up outside it.
//!
//! An animated scene fits instead around a ball that holds every object at every key, which
//! looks the same from any side, so the scene holds still in its frame as it moves and turns.

use glam::{Mat4, Quat, Vec2, Vec3, Vec4, vec2};

use super::Canvas;
use crate::draw::gpu::{Pipeline, Rect, Vertex, premultiply};
use crate::draw::model::{Camera, Color, Point3, SceneObject};

/// Scene lines are a little wider than box lines, so they read among lit surfaces.
const SCENE_LINE: f32 = 1.25;
/// Segments around spheres, cylinders, and line rods.
const SOLID_SEGMENTS: usize = 32;
const ROD_SEGMENTS: usize = 8;
/// Eye distance in scene radii: enough perspective to read as 3D without distortion.
const EYE_DISTANCE: f32 = 3.5;
/// Space between a scene's objects and its frame, in logical units.
const SCENE_MARGIN: f32 = 20.0;

/// How a scene is seen: the camera the user orbited to, if any, a zoom over the fitted size,
/// and for an animated scene the ball it fits around.
#[derive(Debug, Clone, Copy)]
pub(in crate::draw) struct View {
    pub(in crate::draw) camera: Option<Camera>,
    pub(in crate::draw) zoom: f32,
    reach: Option<(Vec3, f32)>,
}

impl View {
    /// The view of a scene of `objects`, keyed at `times`, before the user orbits or zooms.
    pub(in crate::draw) fn new(objects: &[SceneObject], times: &[f64]) -> Self {
        let reach = (!times.is_empty()).then(|| {
            let balls: Vec<(Vec3, f32)> = std::iter::once(0.0)
                .chain(times.iter().copied())
                .flat_map(|t| objects.iter().map(move |object| ball(&object.at(t))))
                .collect();
            let (low, high) = balls.iter().fold(
                (Vec3::INFINITY, Vec3::NEG_INFINITY),
                |(low, high), &(center, radius)| {
                    (low.min(center - radius), high.max(center + radius))
                },
            );
            let center = (low + high) / 2.0;
            let radius = balls
                .iter()
                .map(|&(at, radius)| at.distance(center) + radius)
                .fold(1e-6, f32::max);
            (center, radius)
        });
        Self {
            camera: None,
            zoom: 1.0,
            reach,
        }
    }
}

impl Canvas<'_> {
    /// A 3D scene in `frame`, fitted from `camera` (or around the view's ball) and seen
    /// through `view`, drawn in its `depth` slice so it never mixes with other scenes. Line
    /// objects are rods about `line` wide on screen.
    pub(super) fn scene(
        &mut self,
        objects: &[SceneObject],
        (camera, view): (Camera, View),
        frame: Rect,
        depth: (f32, f32),
        line: f32,
    ) {
        let style = self.style;
        let (mut triangles, lines) =
            geometry(objects, |name| premultiply(style.palette.rgb(name), 255));
        let fit = match view.reach {
            Some((center, radius)) => Some(Fit::around(center, radius, frame)),
            None => {
                let points: Vec<Vec3> = triangles
                    .iter()
                    .map(Vertex::position)
                    .chain(lines.iter().flat_map(|&(from, to, _)| [from, to]))
                    .collect();
                Fit::new(&points, camera, frame)
            }
        };
        let Some(mut fit) = fit else {
            return;
        };
        fit.zoom *= view.zoom;
        let radius = line * SCENE_LINE / 2.0 / fit.zoom;
        for (from, to, color) in lines {
            cylinder(&mut triangles, (from, to), radius, ROD_SEGMENTS, color);
        }
        let view = look(view.camera.unwrap_or(camera), fit.center);
        let clip_from_world = self.frame.screen * fit.projection(frame, depth) * view;
        let camera = self.frame.camera(clip_from_world, view);
        self.push(Pipeline::Lit, camera, Some(frame), triangles);
    }
}

fn point(p: &Point3) -> Vec3 {
    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

/// A line object's ends and color, before it becomes a rod.
type Segment = (Vec3, Vec3, [f32; 4]);

/// Each object's lit triangles and line segments, turned by its rotation about its center.
fn geometry(
    objects: &[SceneObject],
    color: impl Fn(Color) -> [f32; 4],
) -> (Vec<Vertex>, Vec<Segment>) {
    let mut triangles = Vec::new();
    let mut lines = Vec::new();
    for object in objects {
        let start = triangles.len();
        let (center, _) = ball(object);
        let rotation = object.rotation().map_or(Quat::IDENTITY, |degrees| {
            let [x, y, z] = degrees.map(|angle| (angle as f32).to_radians());
            Quat::from_rotation_z(z) * Quat::from_rotation_y(y) * Quat::from_rotation_x(x)
        });
        let turn = |p: Vec3| center + rotation * (p - center);
        match object {
            SceneObject::Cuboid {
                center,
                size,
                color: name,
                ..
            } => cuboid(&mut triangles, point(center), point(size), color(*name)),
            SceneObject::Sphere {
                center,
                radius,
                color: name,
                ..
            } => {
                let center = point(center);
                sphere(
                    &mut triangles,
                    center,
                    *radius as f32,
                    SOLID_SEGMENTS,
                    color(*name),
                );
            }
            SceneObject::Cylinder {
                from,
                to,
                radius,
                color: name,
                ..
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
                ..
            } => lines.push((turn(point(from)), turn(point(to)), color(*name))),
            SceneObject::Mesh {
                vertices,
                faces,
                color: name,
                ..
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
        if let Some(turned) = triangles.get_mut(start..) {
            turned
                .iter_mut()
                .for_each(|vertex| vertex.turn(center, rotation));
        }
    }
    (triangles, lines)
}

/// An object's center, which it turns about, and the radius around it that holds it however
/// it turns.
fn ball(object: &SceneObject) -> (Vec3, f32) {
    let ends = |from, to| {
        let (from, to) = (point(from), point(to));
        ((from + to) / 2.0, from.distance(to) / 2.0)
    };
    match object {
        SceneObject::Cuboid { center, size, .. } => (point(center), point(size).length() / 2.0),
        SceneObject::Sphere { center, radius, .. } => (point(center), *radius as f32),
        SceneObject::Cylinder {
            from, to, radius, ..
        } => {
            let (center, half) = ends(from, to);
            (center, half.hypot(*radius as f32))
        }
        SceneObject::Line { from, to, .. } => ends(from, to),
        SceneObject::Mesh { vertices, .. } => {
            let points: Vec<Vec3> = vertices.iter().map(point).collect();
            let (low, high) = points
                .iter()
                .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(low, high), &p| {
                    (low.min(p), high.max(p))
                });
            let center = (low + high) / 2.0;
            let radius = points
                .iter()
                .map(|p| p.distance(center))
                .fold(0.0, f32::max);
            (center, radius)
        }
    }
}

/// A camera circling the scene's center, and the zoom that fills the frame with the scene.
pub(super) struct Fit {
    /// The point the camera circles: the middle of the objects' bounds.
    pub(super) center: Vec3,
    /// Eye distance from the center, and the radius that holds every point.
    distance: f32,
    radius: f32,
    /// Center of the perspective image, and logical units per image unit.
    middle: Vec2,
    pub(super) zoom: f32,
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
        let zoom = (room(frame) / (max - min).max(Vec2::splat(1e-6))).min_element();
        Some(Self {
            center,
            distance,
            radius,
            middle: (min + max) / 2.0,
            zoom,
        })
    }

    /// A fit that holds the ball at `center` of `radius` seen from any side.
    pub(super) fn around(center: Vec3, radius: f32, frame: Rect) -> Self {
        let distance = EYE_DISTANCE * radius;
        // Half the width of the ball's outline in the perspective image.
        let half = radius * distance / (distance * distance - radius * radius).sqrt();
        Self {
            center,
            distance,
            radius,
            middle: Vec2::ZERO,
            zoom: (room(frame) / (2.0 * half)).min_element(),
        }
    }

    /// Camera space to homogeneous logical screen coordinates: the perspective image,
    /// centered in `frame`, with depth from 0 at the nearest point to 1 at the farthest
    /// mapped into `depth`.
    pub(super) fn projection(&self, frame: Rect, (near_depth, far_depth): (f32, f32)) -> Mat4 {
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

/// The part of a scene's frame its objects may fill, inside the margin.
fn room(frame: Rect) -> Vec2 {
    let margin = SCENE_MARGIN.min(frame.width / 4.0).min(frame.height / 4.0);
    vec2(frame.width, frame.height) - 2.0 * margin
}

/// World to camera space for `camera` circling `center`: x right, y up, z toward the eye.
pub(super) fn look(camera: Camera, center: Vec3) -> Mat4 {
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

pub(super) fn cuboid(out: &mut Vec<Vertex>, center: Vec3, size: Vec3, color: [f32; 4]) {
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

/// A sphere of `segments` around and half as many bands from pole to pole.
pub(super) fn sphere(
    out: &mut Vec<Vertex>,
    center: Vec3,
    radius: f32,
    segments: usize,
    color: [f32; 4],
) {
    let bands = segments / 2;
    let normal = |band: usize, segment: usize| {
        let latitude = std::f32::consts::PI * (band as f32 / bands as f32 - 0.5);
        let longitude = std::f32::consts::TAU * segment as f32 / segments as f32;
        let (lat_sin, lat_cos) = latitude.sin_cos();
        let (lon_sin, lon_cos) = longitude.sin_cos();
        Vec3::new(lat_cos * lon_cos, lat_sin, lat_cos * lon_sin)
    };
    for band in 0..bands {
        for segment in 0..segments {
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
pub(super) fn cylinder(
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
