//! Validated geometry, camera interaction, and output-local input regions.

use glam::{DQuat, DVec3, EulerRot, Mat4, Vec2, Vec3};
use xai_grok_tools::implementations::grok_build::screen::{ModelPart, Primitive};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn contains(self, point: Vec2) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height
    }

    pub fn input(self) -> (i32, i32, i32, i32) {
        let x = self.x.floor() as i32;
        let y = self.y.floor() as i32;
        (
            x,
            y,
            (self.x + self.width).ceil() as i32 - x,
            (self.y + self.height).ceil() as i32 - y,
        )
    }
}

pub(super) fn close_button(width: f32) -> Rect {
    Rect {
        x: (width - 92.0).max(0.0),
        y: 12.0,
        width: 80.0,
        height: 30.0,
    }
}

pub(super) fn reset_button(width: f32) -> Rect {
    let close = close_button(width);
    let x = close.x - close.width - 8.0;
    if x < 12.0 {
        Rect {
            y: close.y + close.height + 8.0,
            ..close
        }
    } else {
        Rect { x, ..close }
    }
}

#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub(super) struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
}

pub(super) fn color(hex: &str) -> [u8; 3] {
    // Only called after OverlayScene::validate enforces ASCII #RRGGBB.
    let digits = hex.as_bytes();
    let nibble = |c: u8| match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    };
    [1, 3, 5].map(|i| nibble(digits[i]) * 16 + nibble(digits[i + 1]))
}

#[derive(Debug)]
pub(super) struct Model {
    pub vertices: Vec<Vertex>,
    pub min: Vec3,
    pub max: Vec3,
    pub camera: Camera,
}

impl Model {
    pub fn new(parts: &[ModelPart]) -> Self {
        let mut vertices = Vec::new();
        // Remove global translation before transforming meshes, then center the
        // whole scene in f64. Small details survive the final conversion to GPU f32.
        let origin = parts
            .first()
            .map_or(DVec3::ZERO, |part| DVec3::from_array(part.position));
        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = DVec3::splat(f64::NEG_INFINITY);
        for part in parts {
            let rotation = DQuat::from_euler(
                EulerRot::XYZ,
                part.rotation[0],
                part.rotation[1],
                part.rotation[2],
            );
            let scale = DVec3::from_array(part.scale);
            let position = DVec3::from_array(part.position) - origin;
            let rgb = color(&part.color).map(|v| {
                let srgb = f32::from(v) / 255.0;
                if srgb <= 0.04045 {
                    srgb / 12.92
                } else {
                    ((srgb + 0.055) / 1.055).powf(2.4)
                }
            });
            for (point, normal) in geometry(part.primitive) {
                let point = rotation * (point.as_dvec3() * scale) + position;
                // A sub-pixel scale may underflow f32; keep a defined lighting normal.
                let normal = (rotation * (normal.as_dvec3() / scale))
                    .try_normalize()
                    .unwrap_or(DVec3::Y)
                    .as_vec3();
                min = min.min(point);
                max = max.max(point);
                vertices.push((point, normal, rgb));
            }
        }
        let center = (min + max) * 0.5;
        let radius = ((max - min).length() * 0.5).max(0.1) as f32;
        let vertices = vertices
            .into_iter()
            .map(|(point, normal, color)| Vertex {
                position: (point - center).as_vec3().to_array(),
                normal: normal.to_array(),
                color,
            })
            .collect();
        Self {
            vertices,
            min: (min - center).as_vec3(),
            max: (max - center).as_vec3(),
            camera: Camera::new(Vec3::ZERO, radius),
        }
    }

    pub fn interactive_bounds(&self, width: f32, height: f32) -> Option<Rect> {
        let view = self.camera.view();
        let projection = self.camera.projection(width / height);
        let mut low = Vec2::splat(f32::INFINITY);
        let mut high = Vec2::splat(f32::NEG_INFINITY);
        for x in [self.min.x, self.max.x] {
            for y in [self.min.y, self.max.y] {
                for z in [self.min.z, self.max.z] {
                    let point = view.transform_point3(Vec3::new(x, y, z));
                    if point.z >= -self.camera.near() {
                        return Some(Rect {
                            x: 0.0,
                            y: 0.0,
                            width,
                            height,
                        });
                    }
                    let projected = projection.project_point3(point);
                    let point = Vec2::new(
                        (projected.x + 1.0) * width * 0.5,
                        (1.0 - projected.y) * height * 0.5,
                    );
                    low = low.min(point);
                    high = high.max(point);
                }
            }
        }
        low = (low - Vec2::splat(12.0)).max(Vec2::ZERO);
        high = (high + Vec2::splat(12.0)).min(Vec2::new(width, height));
        (high.x > low.x && high.y > low.y).then_some(Rect {
            x: low.x,
            y: low.y,
            width: high.x - low.x,
            height: high.y - low.y,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Camera {
    target: Vec3,
    initial_target: Vec3,
    radius: f32,
    distance: f32,
    yaw: f32,
    pitch: f32,
}

impl Camera {
    pub fn new(center: Vec3, radius: f32) -> Self {
        Self {
            target: center,
            initial_target: center,
            radius,
            distance: radius * 3.5,
            yaw: std::f32::consts::FRAC_PI_4,
            pitch: (0.7f32 / 2.0f32.sqrt()).atan(),
        }
    }

    pub fn position(&self) -> Vec3 {
        self.target
            + Vec3::new(
                self.yaw.sin() * self.pitch.cos(),
                self.pitch.sin(),
                self.yaw.cos() * self.pitch.cos(),
            ) * self.distance
    }

    pub fn view(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.position(), self.target, Vec3::Y)
    }

    fn near(&self) -> f32 {
        self.radius / 1000.0
    }

    pub fn projection(&self, aspect: f32) -> Mat4 {
        glam::camera::rh::proj::directx::perspective(
            45f32.to_radians(),
            aspect,
            self.near(),
            self.radius * 100.0,
        )
    }

    pub fn orbit(&mut self, delta: Vec2, height: f32) {
        let factor = std::f32::consts::TAU / height;
        self.yaw -= delta.x * factor;
        self.pitch = (self.pitch + delta.y * factor).clamp(-1.55, 1.55);
    }

    pub fn zoom(&mut self, delta: f32) {
        self.distance =
            (self.distance * (delta * 0.001).exp()).clamp(self.radius * 0.2, self.radius * 30.0);
    }

    pub fn pan(&mut self, delta: Vec2, height: f32) {
        let inverse = self.view().inverse();
        let right = inverse.x_axis.truncate();
        let up = inverse.y_axis.truncate();
        let factor = 2.0 * self.distance * (22.5f32.to_radians()).tan() / height;
        self.target += (-right * delta.x + up * delta.y) * factor;
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.initial_target, self.radius);
    }
}

/// The gesture chosen at press time stays fixed until its matching release.
#[derive(Debug, Default)]
pub(super) struct Interaction {
    drag: Option<Drag>,
}

#[derive(Debug)]
struct Drag {
    button: u32,
    action: Gesture,
}

#[derive(Debug)]
enum Gesture {
    Close,
    Reset,
    Orbit { previous: Vec2 },
    Pan { previous: Vec2 },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Click {
    None,
    Close,
    Reset,
}

impl Interaction {
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn press(
        &mut self,
        button: u32,
        point: Vec2,
        width: f32,
        model: Option<&Model>,
        height: f32,
    ) {
        if self.drag.is_some() {
            return;
        }
        let action = if button == 0x110 && close_button(width).contains(point) {
            Gesture::Close
        } else if button == 0x110 && model.is_some() && reset_button(width).contains(point) {
            Gesture::Reset
        } else if model
            .and_then(|m| m.interactive_bounds(width, height))
            .is_some_and(|bounds| bounds.contains(point))
        {
            match button {
                0x110 => Gesture::Orbit { previous: point },
                0x111 => Gesture::Pan { previous: point },
                _ => return,
            }
        } else {
            return;
        };
        self.drag = Some(Drag { button, action });
    }

    pub fn motion(&mut self, point: Vec2, model: Option<&mut Model>, height: f32) -> bool {
        let (Some(drag), Some(model)) = (&mut self.drag, model) else {
            return false;
        };
        match &mut drag.action {
            Gesture::Orbit { previous } => {
                model.camera.orbit(point - *previous, height);
                *previous = point;
            }
            Gesture::Pan { previous } => {
                model.camera.pan(point - *previous, height);
                *previous = point;
            }
            _ => return false,
        }
        true
    }

    pub fn release(&mut self, button: u32, point: Vec2, width: f32) -> Click {
        if self.drag.as_ref().is_none_or(|drag| drag.button != button) {
            return Click::None;
        }
        match self.drag.take().map(|drag| drag.action) {
            Some(Gesture::Close) if close_button(width).contains(point) => Click::Close,
            Some(Gesture::Reset) if reset_button(width).contains(point) => Click::Reset,
            _ => Click::None,
        }
    }

    pub fn cancel(&mut self) {
        self.drag = None;
    }
}

fn geometry(primitive: Primitive) -> Vec<(Vec3, Vec3)> {
    let mut result = Vec::new();
    let mut triangle = |a: Vec3, b: Vec3, c: Vec3, normals: Option<[Vec3; 3]>| {
        let flat = (b - a).cross(c - a).normalize_or_zero();
        for (point, normal) in [a, b, c].into_iter().zip(normals.unwrap_or([flat; 3])) {
            result.push((point, normal));
        }
    };
    match primitive {
        Primitive::Box => {
            for (normal, u, v) in [
                (Vec3::X, Vec3::Y, Vec3::Z),
                (-Vec3::X, Vec3::Z, Vec3::Y),
                (Vec3::Y, Vec3::Z, Vec3::X),
                (-Vec3::Y, Vec3::X, Vec3::Z),
                (Vec3::Z, Vec3::X, Vec3::Y),
                (-Vec3::Z, Vec3::Y, Vec3::X),
            ] {
                let a = (normal - u - v) * 0.5;
                let b = (normal + u - v) * 0.5;
                let c = (normal + u + v) * 0.5;
                let d = (normal - u + v) * 0.5;
                triangle(a, b, c, None);
                triangle(a, c, d, None);
            }
        }
        Primitive::Sphere | Primitive::Torus => {
            let sample = |i: u32, j: u32| {
                let u = i as f32 / 24.0 * std::f32::consts::TAU;
                let v = j as f32 / 16.0
                    * if matches!(primitive, Primitive::Sphere) {
                        std::f32::consts::PI
                    } else {
                        std::f32::consts::TAU
                    };
                if matches!(primitive, Primitive::Sphere) {
                    let normal = Vec3::new(v.sin() * u.cos(), v.cos(), v.sin() * u.sin());
                    (normal * 0.5, normal)
                } else {
                    let normal = Vec3::new(v.cos() * u.cos(), v.cos() * u.sin(), v.sin());
                    (
                        Vec3::new(0.5 * u.cos(), 0.5 * u.sin(), 0.0) + normal * 0.15,
                        normal,
                    )
                }
            };
            for i in 0..24 {
                for j in 0..16 {
                    let (a, an) = sample(i, j);
                    let (b, bn) = sample(i + 1, j);
                    let (c, cn) = sample(i + 1, j + 1);
                    let (d, dn) = sample(i, j + 1);
                    triangle(a, b, c, Some([an, bn, cn]));
                    triangle(a, c, d, Some([an, cn, dn]));
                }
            }
        }
        Primitive::Cylinder | Primitive::Cone => {
            let cone = matches!(primitive, Primitive::Cone);
            for i in 0..24 {
                let a = i as f32 / 24.0 * std::f32::consts::TAU;
                let b = (i + 1) as f32 / 24.0 * std::f32::consts::TAU;
                let bottom_a = Vec3::new(a.cos() * 0.5, -0.5, a.sin() * 0.5);
                let bottom_b = Vec3::new(b.cos() * 0.5, -0.5, b.sin() * 0.5);
                let top_a = if cone {
                    Vec3::Y * 0.5
                } else {
                    bottom_a + Vec3::Y
                };
                let top_b = if cone {
                    Vec3::Y * 0.5
                } else {
                    bottom_b + Vec3::Y
                };
                triangle(bottom_a, top_a, bottom_b, None);
                if !cone {
                    triangle(bottom_b, top_a, top_b, None);
                    triangle(top_a, Vec3::Y * 0.5, top_b, Some([Vec3::Y; 3]));
                }
                triangle(
                    bottom_b,
                    Vec3::NEG_Y * 0.5,
                    bottom_a,
                    Some([Vec3::NEG_Y; 3]),
                );
            }
        }
    }
    result
}
