//! 3D scenes: lit triangles and a camera that circles the objects and fits them in the
//! scene's frame. Curved surfaces get smooth normals and flat faces keep sharp ones; meshes
//! and tubes blend normals across gentle bends and keep creases sharp. Tubes follow a smooth
//! curve through their points, and pouches bulge them evenly; a coil is a tube along a path
//! that tangles through its box. Lines become thin rods with a constant width on screen.
//! The GPU tests depth per pixel, so objects may intersect and still look right.
//!
//! The user can orbit the camera of a scene or 3D chart and zoom it ([`View`]). The fit comes
//! from the scene's own camera, so the size holds steady while the user orbits; zoom scales
//! the fitted image about its middle, and the frame clips whatever ends up outside it.
//!
//! Materials shade in the GPU: plastic, flesh with fine bumps, and glass, which draws after
//! the solid objects, back to front, so it shows what lies behind it. Blobs are meshed on a
//! grid and kept between frames, since most frames only move the camera. Labels pin text to
//! projected points and draw with the drawing's text.
//!
//! An animated scene fits instead around a ball that holds every object at every key, which
//! looks the same from any side, so the scene holds still in its frame as it moves and turns.

use std::collections::HashMap;

use glam::{Mat4, Quat, Vec2, Vec3, Vec4, vec2};

use super::Canvas;
use crate::draw::gpu::{Pipeline, Rect, Vertex, premultiply};
use crate::draw::model::{COIL_POINTS, Camera, Color, Material, Point3, Rgb, SceneObject};

/// Scene lines are a little wider than box lines, so they read among lit surfaces.
const SCENE_LINE: f32 = 1.25;
/// Segments around spheres, cylinders, and line rods.
const SOLID_SEGMENTS: usize = 32;
const ROD_SEGMENTS: usize = 8;
/// Segments around a tube, and steps along its curve from one point to the next.
const TUBE_SEGMENTS: usize = 24;
const TUBE_STEPS: usize = 8;
/// Rings along each pouch, and a pouch's radius where it pinches, as a share of its widest.
const POUCH_RINGS: usize = 10;
const POUCH_PINCH: f32 = 0.78;
/// How far a coil's waves run: long enough to fill its ellipsoid without repeating.
const COIL_LENGTH: f32 = 6.0;
/// Neighboring mesh faces share a smooth normal unless they meet at more than about 50°.
const CREASE: f32 = 0.64;
/// Grid cells along a blob's longest side, and how far balls melt together, as a share of
/// their mean radius.
const BLOB_CELLS: f32 = 44.0;
const BLOB_MELT: f32 = 0.6;
/// The most blob surfaces kept between frames.
const BLOB_CACHE: usize = 64;
/// Flesh bumps across one scene radius.
const BUMPS: f32 = 26.0;
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

/// A label's text and color, and where its point lands on the screen.
pub(super) struct Pin {
    pub(super) text: String,
    pub(super) rgb: Rgb,
    pub(super) at: Vec2,
    /// Which way the leader line runs: 1 to the right, -1 to the left.
    pub(super) side: f32,
    /// The scene's opacity.
    pub(super) alpha: f32,
}

/// Blob surfaces, as positions and normals, by their balls' bits.
#[derive(Default)]
pub(in crate::draw) struct Blobs(HashMap<Vec<[u64; 4]>, Vec<(Vec3, Vec3)>>);

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
        let color = |name| premultiply(style.palette.rgb(name), 255);
        let (mut triangles, lines) = geometry(objects, color, self.blobs);
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
        let screen_from_world = fit.projection(frame, depth) * view;
        let camera = self
            .frame
            .camera(self.frame.screen * screen_from_world, view);
        let detail = BUMPS / fit.radius;
        triangles
            .iter_mut()
            .for_each(|vertex| vertex.set_material(vertex.material(), detail));
        // Every vertex of a triangle shares its object's material, so triangles stay whole.
        let (glass, solid): (Vec<Vertex>, Vec<Vertex>) = triangles
            .into_iter()
            .partition(|vertex| vertex.material() == Material::Glass as u8 as f32);
        self.push(Pipeline::Lit, camera, Some(frame), solid);
        // Far triangles first, so each glass layer blends over the ones behind it.
        let mut glass: Vec<&[Vertex]> = glass.chunks_exact(3).collect();
        let depth = |triangle: &&[Vertex]| {
            let middle = triangle.iter().map(Vertex::position).sum::<Vec3>() / 3.0;
            view.transform_point3(middle).z
        };
        glass.sort_by(|a, b| depth(a).total_cmp(&depth(b)));
        self.push(Pipeline::Glass, camera, Some(frame), glass.concat());
        let middle = frame.x + frame.width / 2.0;
        for object in objects {
            if let SceneObject::Label {
                point: p,
                text,
                color,
                ..
            } = object
            {
                let at = screen_from_world.project_point3(point(p)).truncate();
                // A label whose point the user zoomed out of the frame goes with it.
                let inside = (frame.x..=frame.x + frame.width).contains(&at.x)
                    && (frame.y..=frame.y + frame.height).contains(&at.y);
                if !inside {
                    continue;
                }
                self.pins.push(Pin {
                    text: text.clone(),
                    rgb: style.palette.rgb(*color),
                    at,
                    side: if at.x < middle { -1.0 } else { 1.0 },
                    alpha: self.alpha,
                });
            }
        }
    }
}

fn point(p: &Point3) -> Vec3 {
    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

/// A line object's ends and color, before it becomes a rod.
type Segment = (Vec3, Vec3, [f32; 4]);

/// Each object's lit triangles, marked with its material, and line segments, turned by its
/// rotation about its center.
fn geometry(
    objects: &[SceneObject],
    color: impl Fn(Color) -> [f32; 4],
    blobs: &mut Blobs,
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
            } => sphere(
                &mut triangles,
                point(center),
                Vec3::splat(*radius as f32),
                SOLID_SEGMENTS,
                color(*name),
            ),
            SceneObject::Ellipsoid {
                center,
                size,
                color: name,
                ..
            } => sphere(
                &mut triangles,
                point(center),
                point(size) / 2.0,
                SOLID_SEGMENTS,
                color(*name),
            ),
            SceneObject::Tube {
                points,
                radius,
                radii,
                pouches,
                color: name,
                ..
            } => {
                let radii = radii
                    .as_ref()
                    .filter(|radii| radii.len() == points.len())
                    .map_or_else(|| vec![*radius; points.len()], Clone::clone);
                let points: Vec<(Vec3, f32)> = (points.iter().map(point))
                    .zip(radii.iter().map(|radius| *radius as f32))
                    .collect();
                tube(&mut triangles, &points, pouches.unwrap_or(0), color(*name));
            }
            SceneObject::Coil {
                center,
                size,
                radius,
                color: name,
                ..
            } => {
                let points: Vec<(Vec3, f32)> = coil(point(center), point(size))
                    .into_iter()
                    .map(|at| (at, *radius as f32))
                    .collect();
                tube(&mut triangles, &points, 0, color(*name));
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
                smooth(&mut triangles, &vertices, faces, color(*name));
            }
            SceneObject::Blob {
                balls, color: name, ..
            } => {
                let key = balls.iter().map(|ball| ball.map(f64::to_bits)).collect();
                if blobs.0.len() >= BLOB_CACHE {
                    blobs.0.clear();
                }
                let surface = blobs.0.entry(key).or_insert_with(|| blob(balls));
                let rgb = color(*name);
                triangles.extend(
                    surface
                        .iter()
                        .map(|&(at, normal)| Vertex::lit(at, normal, rgb)),
                );
            }
            SceneObject::Label { .. } => {}
        }
        let material = object.material() as u8 as f32;
        if let Some(turned) = triangles.get_mut(start..) {
            turned.iter_mut().for_each(|vertex| {
                vertex.turn(center, rotation);
                vertex.set_material(material, 0.0);
            });
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
        SceneObject::Ellipsoid { center, size, .. } => {
            (point(center), point(size).max_element() / 2.0)
        }
        SceneObject::Sphere { center, radius, .. } => (point(center), *radius as f32),
        SceneObject::Coil {
            center,
            size,
            radius,
            ..
        } => (point(center), point(size).length() / 2.0 + *radius as f32),
        SceneObject::Cylinder {
            from, to, radius, ..
        } => {
            let (center, half) = ends(from, to);
            (center, half.hypot(*radius as f32))
        }
        SceneObject::Line { from, to, .. } => ends(from, to),
        SceneObject::Mesh { vertices, .. } => around(vertices, 0.0),
        SceneObject::Blob { balls, .. } => {
            let centers: Vec<Point3> = balls.iter().map(|&[x, y, z, _]| [x, y, z]).collect();
            let largest = balls.iter().map(|ball| ball[3]).fold(0.0, f64::max);
            around(&centers, largest as f32)
        }
        SceneObject::Label { point: p, .. } => (point(p), 0.0),
        SceneObject::Tube {
            points,
            radius,
            radii,
            ..
        } => {
            let thickest = radii.iter().flatten().fold(*radius, |a, &b| a.max(b));
            around(points, thickest as f32)
        }
    }
}

/// The middle of the bounds of `points`, and the radius that holds them grown by `margin`.
fn around(points: &[Point3], margin: f32) -> (Vec3, f32) {
    let points: Vec<Vec3> = points.iter().map(point).collect();
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
    (center, radius + margin)
}

/// A camera circling the scene's center, and the zoom that fills the frame with the scene.
pub(super) struct Fit {
    /// The point the camera circles: the middle of the objects' bounds.
    pub(super) center: Vec3,
    /// Eye distance from the center, and the radius that holds every point.
    distance: f32,
    pub(super) radius: f32,
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

/// A sphere of `segments` around and half as many bands from pole to pole, stretched to
/// `radii` along x, y, and z.
pub(super) fn sphere(
    out: &mut Vec<Vertex>,
    center: Vec3,
    radii: Vec3,
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
            let corners = normals.map(|n| center + n * radii);
            quad(
                out,
                corners,
                normals.map(|n| (n / radii).normalize()),
                color,
            );
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

/// The triangles of `faces`. Faces that meet at a gentle angle share smooth normals, and
/// creases stay sharp.
fn smooth(out: &mut Vec<Vertex>, vertices: &[Vec3], faces: &[[usize; 3]], color: [f32; 4]) {
    let faces: Vec<([usize; 3], [Vec3; 3], Vec3)> = faces
        .iter()
        .filter_map(|&face| {
            let [Some(&a), Some(&b), Some(&c)] = face.map(|i| vertices.get(i)) else {
                return None;
            };
            Some((face, [a, b, c], (b - a).cross(c - a)))
        })
        .collect();
    // Summed unnormalized face normals weigh each face by its area.
    let mut sums = vec![Vec3::ZERO; vertices.len()];
    for (face, _, normal) in &faces {
        for &i in face {
            sums[i] += *normal;
        }
    }
    for (face, corners, normal) in faces {
        let flat = normal.normalize_or(Vec3::Z);
        let normals = face.map(|i| {
            let smooth = sums[i].normalize_or(flat);
            if smooth.dot(flat) > CREASE {
                smooth
            } else {
                flat
            }
        });
        triangle(out, corners, normals, color);
    }
}

/// A round tube along a smooth curve through `points`, each with its radius, with
/// rounded ends.
fn tube(out: &mut Vec<Vertex>, points: &[(Vec3, f32)], pouches: u32, color: [f32; 4]) {
    let mut points = points.to_vec();
    points.dedup_by(|next, previous| next.0.distance(previous.0) < 1e-6);
    let path = match pouches {
        0 => curve(&points),
        pouches => pouched(&curve(&points), pouches as usize),
    };
    for &(center, radius) in [path.first(), path.last()].into_iter().flatten() {
        sphere(out, center, Vec3::splat(radius), SOLID_SEGMENTS, color);
    }
    let Some(first) = path.get(1).zip(path.first()).map(|(b, a)| b.0 - a.0) else {
        return;
    };
    // Each ring turns as little as it can from the one before, so the tube never twists.
    let mut tangent = first.normalize_or(Vec3::X);
    let mut side = tangent.any_orthonormal_vector();
    let mut vertices = Vec::with_capacity(path.len() * TUBE_SEGMENTS);
    let mut faces = Vec::with_capacity(path.len() * TUBE_SEGMENTS * 2);
    for (ring, &(center, radius)) in path.iter().enumerate() {
        let ahead = path[(ring + 1).min(path.len() - 1)].0 - path[ring.saturating_sub(1)].0;
        let next = ahead.normalize_or(tangent);
        side = Quat::from_rotation_arc(tangent, next) * side;
        side = (side - next * side.dot(next)).normalize_or(next.any_orthonormal_vector());
        tangent = next;
        let up = tangent.cross(side);
        for segment in 0..TUBE_SEGMENTS {
            let angle = std::f32::consts::TAU * segment as f32 / TUBE_SEGMENTS as f32;
            let (sin, cos) = angle.sin_cos();
            vertices.push(center + (side * cos + up * sin) * radius);
        }
        if ring > 0 {
            let (before, here) = ((ring - 1) * TUBE_SEGMENTS, ring * TUBE_SEGMENTS);
            for a in 0..TUBE_SEGMENTS {
                let b = (a + 1) % TUBE_SEGMENTS;
                faces.push([before + a, before + b, here + b]);
                faces.push([before + a, here + b, here + a]);
            }
        }
    }
    smooth(out, &vertices, &faces, color);
}

/// Points along a centripetal Catmull-Rom curve through `points`, which never loops or
/// overshoots between them, with radii blended along the way.
fn curve(points: &[(Vec3, f32)]) -> Vec<(Vec3, f32)> {
    let [(start, _), (second, _), ..] = points else {
        return points.to_vec();
    };
    let [.., (before_end, _), (end, _)] = points else {
        return points.to_vec();
    };
    // Mirrored points past each end keep the curve straight as it leaves its ends.
    let knots: Vec<Vec3> = std::iter::once(2.0 * *start - *second)
        .chain(points.iter().map(|&(at, _)| at))
        .chain([2.0 * *end - *before_end])
        .collect();
    let mut path = vec![points[0]];
    for (window, pair) in knots.windows(4).zip(points.windows(2)) {
        let (&[p0, p1, p2, p3], &[(_, from), (_, to)]) = (window, pair) else {
            continue;
        };
        let gap = |a: Vec3, b: Vec3| a.distance(b).sqrt().max(1e-4);
        let (t1, t2) = (gap(p0, p1), gap(p0, p1) + gap(p1, p2));
        let t3 = t2 + gap(p2, p3);
        for step in 1..=TUBE_STEPS {
            let share = step as f32 / TUBE_STEPS as f32;
            let t = t1 + (t2 - t1) * share;
            let mix = |a: Vec3, b: Vec3, ta: f32, tb: f32| {
                a * ((tb - t) / (tb - ta)) + b * ((t - ta) / (tb - ta))
            };
            let (a1, a2, a3) = (
                mix(p0, p1, 0.0, t1),
                mix(p1, p2, t1, t2),
                mix(p2, p3, t2, t3),
            );
            let (b1, b2) = (mix(a1, a2, 0.0, t2), mix(a2, a3, t1, t3));
            path.push((mix(b1, b2, t1, t2), from + (to - from) * share));
        }
    }
    path
}

/// `path` resampled evenly along its length, at least as finely as before, with radii that
/// bulge into `count` pouches pinched between.
fn pouched(path: &[(Vec3, f32)], count: usize) -> Vec<(Vec3, f32)> {
    if path.len() < 2 {
        return path.to_vec();
    }
    let mut lengths = vec![0.0];
    for pair in path.windows(2) {
        lengths.push(lengths[lengths.len() - 1] + pair[0].0.distance(pair[1].0));
    }
    let total = lengths[lengths.len() - 1].max(1e-6);
    let rings = (count * POUCH_RINGS).max(path.len());
    let mut segment = 0;
    (0..=rings)
        .map(|ring| {
            let share = ring as f32 / rings as f32;
            let along = share * total;
            while segment + 2 < lengths.len() && lengths[segment + 1] < along {
                segment += 1;
            }
            let span = (lengths[segment + 1] - lengths[segment]).max(1e-6);
            let blend = ((along - lengths[segment]) / span).clamp(0.0, 1.0);
            let ((from, from_radius), (to, to_radius)) = (path[segment], path[segment + 1]);
            let bulge = (std::f32::consts::PI * count as f32 * share)
                .sin()
                .abs()
                .sqrt();
            let radius = from_radius + (to_radius - from_radius) * blend;
            (
                from.lerp(to, blend),
                radius * (POUCH_PINCH + (1.0 - POUCH_PINCH) * bulge),
            )
        })
        .collect()
}

/// A path that tangles through the box of `size` around `center`: on each axis, a slow
/// wave and a fast one at speeds that never line up, so it never repeats and stays mostly
/// near the middle.
fn coil(center: Vec3, size: Vec3) -> Vec<Vec3> {
    let wave = |t: f32, slow: f32, fast: f32, phase: f32| {
        0.65 * (slow * t + phase).sin() + 0.35 * (fast * t + 2.0 * phase).sin()
    };
    (0..COIL_POINTS)
        .map(|i| {
            let t = COIL_LENGTH * i as f32 / (COIL_POINTS - 1) as f32;
            let wander = Vec3::new(
                wave(t, 4.1, 9.3, 0.0),
                wave(t, 5.7, 11.1, 1.0),
                wave(t, 6.9, 13.7, 2.0),
            );
            center + size / 2.0 * wander
        })
        .collect()
}

/// The surface where balls of [x, y, z, radius] melt together: the zero crossing of their
/// smoothed distance field, meshed by surface nets as triangles of positions and normals.
fn blob(balls: &[[f64; 4]]) -> Vec<(Vec3, Vec3)> {
    let balls: Vec<(Vec3, f32)> = balls
        .iter()
        .map(|&[x, y, z, radius]| (point(&[x, y, z]), radius as f32))
        .collect();
    let mean = balls.iter().map(|ball| ball.1).sum::<f32>() / balls.len().max(1) as f32;
    let melt = BLOB_MELT * mean;
    let field = |p: Vec3| {
        balls
            .iter()
            .map(|&(center, radius)| p.distance(center) - radius)
            .reduce(|a, b| {
                let h = (0.5 + 0.5 * (b - a) / melt).clamp(0.0, 1.0);
                b + (a - b) * h - melt * h * (1.0 - h)
            })
            .unwrap_or(1.0)
    };
    let (low, high) = balls.iter().fold(
        (Vec3::INFINITY, Vec3::NEG_INFINITY),
        |(low, high), &(center, radius)| (low.min(center - radius), high.max(center + radius)),
    );
    let cell = (high - low).max_element() / BLOB_CELLS;
    if !cell.is_finite() || cell <= 0.0 {
        return Vec::new();
    }
    let low = low - Vec3::splat(melt + 2.0 * cell);
    let counts = ((high - low + Vec3::splat(melt + 2.0 * cell)) / cell)
        .ceil()
        .as_uvec3()
        .max(glam::UVec3::splat(2));
    let [nx, ny, nz] = counts.to_array().map(|count| count as usize);
    let index = |x: usize, y: usize, z: usize| x + nx * (y + ny * z);
    let at = |x: usize, y: usize, z: usize| low + Vec3::new(x as f32, y as f32, z as f32) * cell;
    let mut values = vec![0.0; nx * ny * nz];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                values[index(x, y, z)] = field(at(x, y, z));
            }
        }
    }
    // One vertex in each cell the surface crosses, at the mean of its edge crossings.
    let mut cells = vec![u32::MAX; nx * ny * nz];
    let mut vertices = Vec::new();
    for z in 0..nz - 1 {
        for y in 0..ny - 1 {
            for x in 0..nx - 1 {
                let corners: [(usize, usize, usize); 8] =
                    std::array::from_fn(|i| (x + (i & 1), y + ((i >> 1) & 1), z + ((i >> 2) & 1)));
                let value = |(x, y, z): (usize, usize, usize)| values[index(x, y, z)];
                let (mut sum, mut count) = (Vec3::ZERO, 0.0);
                for (a, b) in EDGES {
                    let (va, vb) = (value(corners[a]), value(corners[b]));
                    if (va < 0.0) != (vb < 0.0) {
                        let (pa, pb) = (corners[a], corners[b]);
                        let share = va / (va - vb);
                        sum += at(pa.0, pa.1, pa.2).lerp(at(pb.0, pb.1, pb.2), share);
                        count += 1.0;
                    }
                }
                if count > 0.0 {
                    cells[index(x, y, z)] = vertices.len() as u32;
                    vertices.push(sum / count);
                }
            }
        }
    }
    let normals: Vec<Vec3> = vertices
        .iter()
        .map(|&p| {
            let step = cell / 2.0;
            let slope = |axis: Vec3| field(p + axis * step) - field(p - axis * step);
            Vec3::new(slope(Vec3::X), slope(Vec3::Y), slope(Vec3::Z)).normalize_or(Vec3::Y)
        })
        .collect();
    // One quad across each grid edge the surface crosses, joining the four cells around it.
    let mut triangles = Vec::new();
    for z in 1..nz - 1 {
        for y in 1..ny - 1 {
            for x in 1..nx - 1 {
                let inside = values[index(x, y, z)] < 0.0;
                // The other end of each edge, and the four cells around it as offsets from
                // the cell below, behind, and left of this grid point.
                let around = [
                    ((x + 1, y, z), [(1, 0, 0), (1, 1, 0), (1, 1, 1), (1, 0, 1)]),
                    ((x, y + 1, z), [(0, 1, 0), (0, 1, 1), (1, 1, 1), (1, 1, 0)]),
                    ((x, y, z + 1), [(0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)]),
                ];
                for ((ex, ey, ez), offsets) in around {
                    if (values[index(ex, ey, ez)] < 0.0) == inside {
                        continue;
                    }
                    let quad = offsets
                        .map(|(dx, dy, dz)| cells[index(x - 1 + dx, y - 1 + dy, z - 1 + dz)]);
                    if quad.contains(&u32::MAX) {
                        continue;
                    }
                    let [a, b, c, d] = quad.map(|i| i as usize);
                    triangles.extend([a, b, c, a, c, d].map(|i| (vertices[i], normals[i])));
                }
            }
        }
    }
    triangles
}

/// The 12 edges of a grid cell, as pairs of its corners numbered x + 2y + 4z.
const EDGES: [(usize, usize); 12] = [
    (0, 1),
    (2, 3),
    (4, 5),
    (6, 7),
    (0, 2),
    (1, 3),
    (4, 6),
    (5, 7),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];
