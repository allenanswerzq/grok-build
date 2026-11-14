//! The renderer: every drawing, 2D and 3D, goes through one wgpu pass.
//!
//! Painters describe a frame as draw data: triangles in one vertex buffer, split into draws
//! that each pick a pipeline, a camera, and an optional clip rectangle. The GPU draws them in
//! order into a 4× multisampled buffer and resolves it straight into the overlay's Wayland
//! surface ([`Target`]), so frames never pass through memory.
//!
//! Colors are premultiplied sRGB, the format Wayland buffers use. 2D draws ignore depth, so
//! submission order decides what is on top. Each 3D scene owns a slice of the depth range,
//! later scenes nearer, so depth testing works inside a scene and never across scenes.

use std::ffi::c_void;
use std::ops::Range;
use std::ptr::NonNull;

use anyhow::{Context, Result, bail, ensure};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec2, Vec3, vec3};
use wgpu::rwh::{WaylandDisplayHandle, WaylandWindowHandle};
use wgpu::util::DeviceExt;

use crate::implementations::grok_build::screen_draw::Rgb;

const SAMPLES: u32 = 4;
/// The surface format every Vulkan Wayland driver offers, and the glyph atlas's format.
const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
const ATLAS: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// A rectangle in logical units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

/// An image of premultiplied RGBA rows.
pub(crate) struct Image {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: Vec<u8>,
}

impl Image {
    pub(super) fn transparent(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        }
    }
}

/// Premultiplied RGBA from a theme color and an alpha.
pub(super) fn premultiply(rgb: Rgb, alpha: u8) -> [f32; 4] {
    let alpha = f32::from(alpha) / 255.0;
    let [r, g, b] = rgb.map(|channel| f32::from(channel) / 255.0 * alpha);
    [r, g, b, alpha]
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub(super) struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 4],
    /// Atlas texel, for glyphs.
    texel: [f32; 2],
}

impl Vertex {
    pub(super) fn solid(position: Vec2, color: [f32; 4]) -> Self {
        Self::glyph(position, Vec2::ZERO, color)
    }

    pub(super) fn lit(position: Vec3, normal: Vec3, color: [f32; 4]) -> Self {
        Self {
            position: position.into(),
            normal: normal.into(),
            color,
            texel: [0.0; 2],
        }
    }

    pub(super) fn glyph(position: Vec2, texel: Vec2, color: [f32; 4]) -> Self {
        Self {
            position: position.extend(0.0).into(),
            normal: [0.0, 0.0, 1.0],
            color,
            texel: texel.into(),
        }
    }

    pub(super) fn position(&self) -> Vec3 {
        self.position.into()
    }

    /// The vertex at `alpha` of its opacity.
    pub(super) fn faded(mut self, alpha: f32) -> Self {
        self.color = self.color.map(|channel| channel * alpha);
        self
    }

    /// Turn the vertex by `rotation` about `center`.
    pub(super) fn turn(&mut self, center: Vec3, rotation: Quat) {
        self.position = (center + rotation * (self.position() - center)).into();
        self.normal = (rotation * Vec3::from(self.normal)).into();
    }
}

/// How a draw's triangles become pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pipeline {
    /// Solid color, painted in order: 2D shapes.
    Solid,
    /// Lit and depth tested: 3D scenes.
    Lit,
    /// Glyphs from the text atlas.
    Glyph,
}

/// Maps a draw's vertices to clip space, and its normals to camera space for lighting.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub(super) struct Camera {
    pub(super) clip_from_world: [[f32; 4]; 4],
    pub(super) view_from_world: [[f32; 4]; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Draw {
    pipeline: Pipeline,
    camera: u32,
    clip: Option<Rect>,
    vertices: Range<u32>,
}

/// Everything one paint draws.
pub(super) struct Frame {
    /// Image size in pixels.
    pub(super) size: (u32, u32),
    /// Pixels per logical unit.
    pub(super) scale: f32,
    /// Maps logical units, origin top-left and y down, to clip space.
    pub(super) screen: Mat4,
    pub(super) vertices: Vec<Vertex>,
    pub(super) cameras: Vec<Camera>,
    pub(super) draws: Vec<Draw>,
    pub(super) atlas: Image,
}

impl Frame {
    /// The screen camera is camera 0.
    pub(super) fn new(size: (u32, u32), scale: f32) -> Self {
        let logical = Vec2::new(size.0 as f32, size.1 as f32) / scale;
        let screen = Mat4::from_translation(vec3(-1.0, 1.0, 0.0))
            * Mat4::from_scale(vec3(2.0 / logical.x, -2.0 / logical.y, 1.0));
        let mut frame = Self {
            size,
            scale,
            screen,
            vertices: Vec::new(),
            cameras: Vec::new(),
            draws: Vec::new(),
            atlas: Image::transparent(1, 1),
        };
        frame.camera(screen, Mat4::IDENTITY);
        frame
    }

    pub(super) fn camera(&mut self, clip_from_world: Mat4, view_from_world: Mat4) -> u32 {
        self.cameras.push(Camera {
            clip_from_world: clip_from_world.to_cols_array_2d(),
            view_from_world: view_from_world.to_cols_array_2d(),
        });
        self.cameras.len() as u32 - 1
    }

    /// Append triangles, joining the previous draw when they share its state.
    pub(super) fn push(
        &mut self,
        pipeline: Pipeline,
        camera: u32,
        clip: Option<Rect>,
        triangles: impl IntoIterator<Item = Vertex>,
    ) {
        let start = self.vertices.len() as u32;
        self.vertices.extend(triangles);
        let end = self.vertices.len() as u32;
        match self.draws.last_mut() {
            _ if start == end => {}
            Some(last)
                if (last.pipeline, last.camera, last.clip) == (pipeline, camera, clip)
                    && last.vertices.end == start =>
            {
                last.vertices.end = end;
            }
            _ => self.draws.push(Draw {
                pipeline,
                camera,
                clip,
                vertices: start..end,
            }),
        }
    }

    /// A draw's scissor rectangle in pixels, or `None` when nothing of it is visible.
    fn scissor(&self, draw: &Draw) -> Option<[u32; 4]> {
        let (width, height) = self.size;
        let Some(clip) = draw.clip else {
            return Some([0, 0, width, height]);
        };
        let pixel = |value: f32, limit: u32| (value * self.scale).clamp(0.0, limit as f32);
        let (left, top) = (pixel(clip.x, width).floor(), pixel(clip.y, height).floor());
        let right = pixel(clip.x + clip.width, width).ceil();
        let bottom = pixel(clip.y + clip.height, height).ceil();
        (right > left && bottom > top)
            .then(|| [left, top, right - left, bottom - top].map(|value| value as u32))
    }
}

/// The GPU device and pipelines, kept for the life of the overlay.
pub(super) struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    bindings: wgpu::BindGroupLayout,
    solid: wgpu::RenderPipeline,
    lit: wgpu::RenderPipeline,
    glyph: wgpu::RenderPipeline,
}

/// A Wayland surface the GPU draws on, and its multisampled color and depth buffers at the
/// configured size.
pub(crate) struct Target {
    surface: wgpu::Surface<'static>,
    size: (u32, u32),
    buffers: Option<(wgpu::TextureView, wgpu::TextureView)>,
}

impl Gpu {
    pub(super) fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .context("No Vulkan GPU is available for drawing")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("screen_draw"),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .context("Cannot open the GPU for drawing")?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("screen_draw"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("screen_draw"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("screen_draw"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x2
        ];
        let pipeline = |fragment: &str, depth: wgpu::CompareFunction| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fragment),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(depth != wgpu::CompareFunction::Always),
                    depth_compare: Some(depth),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: SAMPLES,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let solid = pipeline("solid", wgpu::CompareFunction::Always);
        let lit = pipeline("lit", wgpu::CompareFunction::Less);
        let glyph = pipeline("glyph", wgpu::CompareFunction::Always);
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            bindings,
            solid,
            lit,
            glyph,
        })
    }

    /// A target for the Wayland surface `window` on `display`.
    ///
    /// # Safety
    /// Both must stay valid until the target is dropped.
    pub(super) unsafe fn target(
        &self,
        display: NonNull<c_void>,
        window: NonNull<c_void>,
    ) -> Result<Target> {
        let handles = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(WaylandDisplayHandle::new(display).into()),
            raw_window_handle: WaylandWindowHandle::new(window).into(),
        };
        // SAFETY: the caller keeps both handles valid for the life of the target.
        let surface = unsafe { self.instance.create_surface_unsafe(handles) }
            .context("Cannot draw on the overlay surface")?;
        Ok(Target {
            surface,
            size: (0, 0),
            buffers: None,
        })
    }

    /// Draw `frame` on `target` and present it. Returns false when the surface skipped the
    /// frame; painting again later shows it.
    pub(super) fn render(&self, frame: &Frame, target: &mut Target) -> Result<bool> {
        let (width, height) = frame.size;
        let limit = self.device.limits().max_texture_dimension_2d;
        ensure!(
            width <= limit && height <= limit,
            "The drawing exceeds the GPU's {limit} pixel texture limit"
        );
        if target.size != frame.size {
            self.configure(target, frame.size)?;
        }
        let Some((color, depth)) = &target.buffers else {
            return Ok(false);
        };
        let output = match target.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Outdated => {
                // Configure again before the next try.
                target.size = (0, 0);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Validation => {
                bail!("The overlay surface was lost")
            }
        };
        let buffer = |contents: &[u8], usage| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents,
                    usage,
                })
        };
        let vertices = buffer(
            bytemuck::cast_slice(&frame.vertices),
            wgpu::BufferUsages::VERTEX,
        );
        let cameras = buffer(
            bytemuck::cast_slice(&frame.cameras),
            wgpu::BufferUsages::STORAGE,
        );
        let atlas = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("glyphs"),
                size: wgpu::Extent3d {
                    width: frame.atlas.width,
                    height: frame.atlas.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: ATLAS,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &frame.atlas.data,
        );
        let atlas = atlas.create_view(&Default::default());
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cameras.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas),
                },
            ],
        });
        let output_view = output.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("screen_draw"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
                    resolve_target: Some(&output_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &bindings, &[]);
            pass.set_vertex_buffer(0, vertices.slice(..));
            for draw in &frame.draws {
                let Some([x, y, width, height]) = frame.scissor(draw) else {
                    continue;
                };
                pass.set_scissor_rect(x, y, width, height);
                pass.set_pipeline(match draw.pipeline {
                    Pipeline::Solid => &self.solid,
                    Pipeline::Lit => &self.lit,
                    Pipeline::Glyph => &self.glyph,
                });
                // The instance index selects the draw's camera.
                pass.draw(draw.vertices.clone(), draw.camera..draw.camera + 1);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(output);
        Ok(true)
    }

    /// Configure `target` for `size` pixels, with multisampled buffers to match.
    fn configure(&self, target: &mut Target, (width, height): (u32, u32)) -> Result<()> {
        let capabilities = target.surface.get_capabilities(&self.adapter);
        ensure!(
            capabilities.formats.contains(&COLOR),
            "The overlay surface cannot show {COLOR:?} pixels"
        );
        let mut config = target
            .surface
            .get_default_config(&self.adapter, width, height)
            .context("The GPU cannot draw on the overlay surface")?;
        config.format = COLOR;
        config.view_formats = Vec::new();
        config.alpha_mode = [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ]
        .into_iter()
        .find(|mode| capabilities.alpha_modes.contains(mode))
        .context("The overlay surface cannot be transparent")?;
        // Frames are paced by the compositor's frame events, so presenting never waits.
        if capabilities
            .present_modes
            .contains(&wgpu::PresentMode::Mailbox)
        {
            config.present_mode = wgpu::PresentMode::Mailbox;
        }
        target.surface.configure(&self.device, &config);
        let buffer = |format| {
            self.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: SAMPLES,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        target.buffers = Some((buffer(COLOR), buffer(DEPTH)));
        target.size = (width, height);
        Ok(())
    }
}
