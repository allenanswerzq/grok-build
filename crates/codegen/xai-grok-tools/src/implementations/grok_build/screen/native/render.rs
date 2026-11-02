//! Rust raster text/annotations and GPU-lit meshes, presented through Wayland SHM.

use super::scene::{Model, Rect, Vertex, close_button, color, reset_button};
use anyhow::{Context, Result, ensure};
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use glam::Vec2;
use std::time::Duration;
use tiny_skia::{Paint, PathBuilder, Pixmap, Stroke, Transform};
use wgpu::util::DeviceExt;
use xai_grok_tools::implementations::grok_build::screen::{Annotation, OverlayScene};

pub(super) struct Renderer {
    fonts: FontSystem,
    glyphs: SwashCache,
    gpu: Option<Gpu>,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            fonts: FontSystem::new(),
            glyphs: SwashCache::new(),
            gpu: None,
        }
    }

    pub fn draw(
        &mut self,
        scene: &OverlayScene,
        model: Option<&Model>,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<Pixmap> {
        if model.is_some() {
            ensure!(
                width >= 160 && height >= 120,
                "Viewport too small for a 3D overlay"
            );
        }
        let pixels_w = (width as f32 * scale).ceil() as u32;
        let pixels_h = (height as f32 * scale).ceil() as u32;
        ensure!(
            pixels_w > 0
                && pixels_h > 0
                && pixels_w <= 16_384
                && pixels_h <= 16_384
                && u64::from(pixels_w) * u64::from(pixels_h) <= 33_554_432,
            "Overlay output exceeds the 128 MiB raster budget"
        );
        let mut image =
            Pixmap::new(pixels_w, pixels_h).context("Cannot allocate overlay pixels")?;
        if let Some(model) = model {
            if self.gpu.is_none() {
                self.gpu = Some(Gpu::new()?);
            }
            if let Some(gpu) = &self.gpu {
                gpu.draw(model, &mut image, width as f32 / height as f32)?;
            }
        }
        if let OverlayScene::Annotations { items } = scene {
            for item in items {
                let mut path = PathBuilder::new();
                let rgb = match item {
                    Annotation::Rectangle {
                        x,
                        y,
                        width,
                        height,
                        color,
                    } => {
                        if let Some(rect) = tiny_skia::Rect::from_xywh(
                            *x as f32,
                            *y as f32,
                            *width as f32,
                            *height as f32,
                        ) {
                            let (x, y, w, h) = (rect.x(), rect.y(), rect.width(), rect.height());
                            let r = 4.0_f32.min(w * 0.5).min(h * 0.5);
                            path.move_to(x + r, y);
                            path.line_to(x + w - r, y);
                            path.quad_to(x + w, y, x + w, y + r);
                            path.line_to(x + w, y + h - r);
                            path.quad_to(x + w, y + h, x + w - r, y + h);
                            path.line_to(x + r, y + h);
                            path.quad_to(x, y + h, x, y + h - r);
                            path.line_to(x, y + r);
                            path.quad_to(x, y, x + r, y);
                            path.close();
                        }
                        color
                    }
                    Annotation::Arrow {
                        x,
                        y,
                        to_x,
                        to_y,
                        color,
                    } => {
                        path.move_to(*x as f32, *y as f32);
                        path.line_to(*to_x as f32, *to_y as f32);
                        let angle = (to_y - y).atan2(to_x - x);
                        path.move_to(
                            (to_x - 12.0 * (angle - 0.45).cos()) as f32,
                            (to_y - 12.0 * (angle - 0.45).sin()) as f32,
                        );
                        path.line_to(*to_x as f32, *to_y as f32);
                        path.line_to(
                            (to_x - 12.0 * (angle + 0.45).cos()) as f32,
                            (to_y - 12.0 * (angle + 0.45).sin()) as f32,
                        );
                        color
                    }
                    Annotation::Text { x, y, text, color } => {
                        self.text(
                            &mut image,
                            text,
                            Vec2::new(*x as f32, *y as f32 - 20.0),
                            20.0,
                            super::scene::color(color),
                            scale,
                        );
                        continue;
                    }
                };
                if let Some(path) = path.finish() {
                    let [r, g, b] = color(rgb);
                    let mut paint = Paint::default();
                    paint.set_color_rgba8(r, g, b, 255);
                    image.stroke_path(
                        &path,
                        &paint,
                        &Stroke {
                            width: 3.0,
                            ..Stroke::default()
                        },
                        Transform::from_scale(scale, scale),
                        None,
                    );
                }
            }
        }
        self.button(&mut image, close_button(width as f32), "Close ×", scale);
        if model.is_some() {
            self.button(&mut image, reset_button(width as f32), "Reset", scale);
        }
        Ok(image)
    }

    fn button(&mut self, image: &mut Pixmap, rect: Rect, label: &str, scale: f32) {
        if let Some(rectangle) = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
        {
            let mut paint = Paint::default();
            paint.set_color_rgba8(32, 41, 54, 255);
            image.fill_rect(rectangle, &paint, Transform::from_scale(scale, scale), None);
        }
        self.text(
            image,
            label,
            Vec2::new(rect.x + 12.0, rect.y + 5.0),
            14.0,
            [255; 3],
            scale,
        );
    }

    fn text(
        &mut self,
        image: &mut Pixmap,
        text: &str,
        position: Vec2,
        size: f32,
        rgb: [u8; 3],
        scale: f32,
    ) {
        let mut buffer = Buffer::new(
            &mut self.fonts,
            Metrics::new(size * scale, size * scale * 1.25),
        );
        buffer.set_size(Some(image.width() as f32), None);
        buffer.set_text(
            text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None,
        );
        let width = image.width();
        let height = image.height();
        // A thin dark outline keeps labels readable against both light and dark desktops.
        let outline = [-1.0, 0.0, 1.0].into_iter().flat_map(|x| {
            [-1.0, 0.0, 1.0]
                .into_iter()
                .filter(move |y| x != 0.0 || *y != 0.0)
                .map(move |y| (x, y, Color::rgb(0, 0, 0)))
        });
        for (dx, dy, tint) in outline.chain(std::iter::once((
            0.0,
            0.0,
            Color::rgb(rgb[0], rgb[1], rgb[2]),
        ))) {
            buffer.draw(
                &mut self.fonts,
                &mut self.glyphs,
                tint,
                |gx, gy, w, h, color| {
                    let start_x = gx + ((position.x + dx) * scale).round() as i32;
                    let start_y = gy + ((position.y + dy) * scale).round() as i32;
                    let alpha = u32::from(color.a());
                    for py in start_y.max(0)..(start_y + h as i32).min(height as i32) {
                        for px in start_x.max(0)..(start_x + w as i32).min(width as i32) {
                            let offset = ((py as u32 * width + px as u32) * 4) as usize;
                            let pixel = &mut image.data_mut()[offset..offset + 4];
                            for (dst, src) in
                                pixel[..3].iter_mut().zip([color.r(), color.g(), color.b()])
                            {
                                *dst = ((u32::from(src) * alpha
                                    + u32::from(*dst) * (255 - alpha)
                                    + 127)
                                    / 255) as u8;
                            }
                            pixel[3] =
                                (alpha + (u32::from(pixel[3]) * (255 - alpha) + 127) / 255) as u8;
                        }
                    }
                },
            );
        }
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
}

impl Gpu {
    fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .context("No Vulkan adapter is available for 3D overlays")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Grok overlay"),
            ..Default::default()
        }))
        .context("Cannot create overlay graphics device")?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Overlay mesh shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mesh.wgsl").into()),
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Lit overlay meshes"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: 4,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Ok(Self {
            device,
            queue,
            pipeline,
        })
    }

    fn draw(&self, model: &Model, image: &mut Pixmap, aspect: f32) -> Result<()> {
        let (width, height) = (image.width(), image.height());
        let limit = self.device.limits().max_texture_dimension_2d;
        ensure!(
            width <= limit && height <= limit,
            "3D output exceeds Vulkan texture limit {limit}"
        );
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let descriptor = |format, usage| wgpu::TextureDescriptor {
            label: Some("Overlay raster"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        };
        let texture = self.device.create_texture(&descriptor(
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        ));
        let multisampled = self.device.create_texture(&wgpu::TextureDescriptor {
            sample_count: 4,
            ..descriptor(
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
        });
        let depth = self.device.create_texture(&wgpu::TextureDescriptor {
            sample_count: 4,
            ..descriptor(
                wgpu::TextureFormat::Depth32Float,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
        });
        let vertices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Scene vertices"),
                contents: bytemuck::cast_slice(&model.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let matrix = model.camera.projection(aspect) * model.camera.view();
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Camera"),
                contents: bytemuck::cast_slice(&matrix.to_cols_array()),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let binding = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera binding"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let row_bytes = (width * 4).div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Presented overlay pixels"),
            size: u64::from(row_bytes) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = texture.create_view(&Default::default());
        let multisampled_view = multisampled.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Transparent desktop scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &multisampled_view,
                    resolve_target: Some(&view),
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &binding, &[]);
            pass.set_vertex_buffer(0, vertices.slice(..));
            pass.draw(0..model.vertices.len() as u32, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        let submission = self.queue.submit([encoder.finish()]);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(5)),
            })
            .context("Overlay GPU did not complete its frame")?;
        receive
            .recv_timeout(Duration::from_secs(1))
            .context("Missing overlay GPU pixels")??;
        {
            let pixels = readback.slice(..).get_mapped_range()?;
            for (row, target) in pixels
                .chunks_exact(row_bytes as usize)
                .zip(image.data_mut().chunks_exact_mut(width as usize * 4))
            {
                target.copy_from_slice(&row[..width as usize * 4]);
            }
        }
        readback.unmap();
        Ok(())
    }
}
