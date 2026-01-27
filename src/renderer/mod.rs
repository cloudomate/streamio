//! wgpu-based offscreen 3D renderer for horizon visualization
//!
//! Renders 3D geological surfaces to a texture that can be read back
//! and streamed via GStreamer.

use anyhow::Result;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use parking_lot::RwLock;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Vertex format for horizon mesh
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub depth: f32, // For color mapping
    _padding: f32,
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        0 => Float32x3,  // position
        1 => Float32x3,  // normal
        2 => Float32,    // depth
    ];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// Camera state controlled by client input
#[derive(Debug, Clone)]
pub struct Camera {
    pub azimuth: f32,      // Horizontal rotation (degrees)
    pub elevation: f32,    // Vertical rotation (degrees)
    pub distance: f32,     // Distance from focal point
    pub focal_point: Vec3, // Point camera looks at
    pub fov: f32,          // Field of view (degrees)
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            azimuth: 45.0,
            elevation: 30.0,
            distance: 5.0,
            focal_point: Vec3::ZERO,
            fov: 45.0,
        }
    }
}

impl Camera {
    pub fn view_matrix(&self) -> Mat4 {
        let az = self.azimuth.to_radians();
        let el = self.elevation.to_radians();

        let eye = self.focal_point
            + Vec3::new(
                self.distance * el.cos() * az.sin(),
                self.distance * el.cos() * az.cos(),
                self.distance * el.sin(),
            );

        Mat4::look_at_rh(eye, self.focal_point, Vec3::Z)
    }

    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov.to_radians(), aspect, 0.1, 100.0)
    }

    /// Handle rotation input (mouse drag)
    pub fn rotate(&mut self, dx: f32, dy: f32) {
        self.azimuth += dx * 0.5;
        self.elevation = (self.elevation + dy * 0.5).clamp(-89.0, 89.0);
    }

    /// Handle zoom input (mouse wheel)
    pub fn zoom(&mut self, delta: f32) {
        self.distance = (self.distance * (1.0 - delta * 0.1)).clamp(1.0, 20.0);
    }

    /// Handle pan input (middle mouse drag)
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let scale = self.distance * 0.002;
        self.focal_point.x -= dx * scale;
        self.focal_point.y += dy * scale;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Uniform buffer for shader
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    depth_range: [f32; 2],
    _padding: [f32; 2],
}

/// Offscreen renderer using wgpu
pub struct HorizonRenderer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    render_pipeline: wgpu::RenderPipeline,
    render_texture: wgpu::Texture,
    render_texture_view: wgpu::TextureView,
    #[allow(dead_code)]
    depth_texture: wgpu::Texture,
    depth_texture_view: wgpu::TextureView,
    output_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
    pub width: u32,
    pub height: u32,
    pub camera: Arc<RwLock<Camera>>,
    depth_min: f32,
    depth_max: f32,
}

impl HorizonRenderer {
    pub async fn new(width: u32, height: u32) -> Result<Self> {
        // Create wgpu instance
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::METAL,
            ..Default::default()
        });

        // Request adapter (GPU)
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None, // Offscreen rendering
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow::anyhow!("Failed to find GPU adapter"))?;

        tracing::info!("Using GPU: {:?}", adapter.get_info().name);

        // Create device and queue
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("Horizon Renderer Device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: Default::default(),
                },
                None,
            )
            .await?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        // Create render texture (offscreen target)
        let render_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Render Target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let render_texture_view = render_texture.create_view(&Default::default());

        // Create depth texture
        let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Depth Texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_texture_view = depth_texture.create_view(&Default::default());

        // Create output buffer for reading pixels
        let output_buffer_size = (width * height * 4) as u64;
        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Output Buffer"),
            size: output_buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // Create shader module
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Horizon Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        // Create uniform buffer
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Uniform Buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create bind group layout and bind group
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Uniform Bind Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Uniform Bind Group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // Create pipeline layout
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Render Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Create render pipeline
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Horizon Render Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::desc()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // Create sample horizon mesh
        let (vertices, indices, depth_min, depth_max) = Self::create_sample_horizon();

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Vertex Buffer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Index Buffer"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        Ok(Self {
            device,
            queue,
            render_pipeline,
            render_texture,
            render_texture_view,
            depth_texture,
            depth_texture_view,
            output_buffer,
            uniform_buffer,
            uniform_bind_group,
            vertex_buffer,
            index_buffer,
            num_indices: indices.len() as u32,
            width,
            height,
            camera: Arc::new(RwLock::new(Camera::default())),
            depth_min,
            depth_max,
        })
    }

    /// Create a sample 3D horizon surface for demonstration
    fn create_sample_horizon() -> (Vec<Vertex>, Vec<u32>, f32, f32) {
        let nx = 100usize;
        let ny = 100usize;

        let mut vertices = Vec::with_capacity(nx * ny);
        let mut indices = Vec::new();

        let mut depth_min = f32::MAX;
        let mut depth_max = f32::MIN;

        // Generate height values
        for j in 0..ny {
            for i in 0..nx {
                let x = (i as f32 / nx as f32 - 0.5) * 4.0;
                let y = (j as f32 / ny as f32 - 0.5) * 4.0;

                // Sample geological-like surface
                let z = 0.3 * (2.0 * x).sin() * (2.0 * y).cos()
                    + 0.1 * (5.0 * x + 2.0).sin()
                    + 0.05 * ((i * 31 + j * 17) as f32 * 0.1).sin(); // Pseudo-random

                depth_min = depth_min.min(z);
                depth_max = depth_max.max(z);

                vertices.push(Vertex {
                    position: [x, y, z],
                    normal: [0.0, 0.0, 1.0], // Will compute proper normals
                    depth: z,
                    _padding: 0.0,
                });
            }
        }

        // Compute normals
        for j in 0..ny {
            for i in 0..nx {
                let idx = j * nx + i;

                let get_pos = |ii: usize, jj: usize| -> Vec3 {
                    let v = &vertices[jj.min(ny - 1) * nx + ii.min(nx - 1)];
                    Vec3::from(v.position)
                };

                let _center = get_pos(i, j);
                let left = get_pos(i.saturating_sub(1), j);
                let right = get_pos((i + 1).min(nx - 1), j);
                let down = get_pos(i, j.saturating_sub(1));
                let up = get_pos(i, (j + 1).min(ny - 1));

                let dx = right - left;
                let dy = up - down;
                let normal = dx.cross(dy).normalize();

                vertices[idx].normal = normal.into();
            }
        }

        // Generate indices for triangle mesh
        for j in 0..(ny - 1) {
            for i in 0..(nx - 1) {
                let idx = (j * nx + i) as u32;

                // First triangle
                indices.push(idx);
                indices.push(idx + 1);
                indices.push(idx + nx as u32);

                // Second triangle
                indices.push(idx + 1);
                indices.push(idx + nx as u32 + 1);
                indices.push(idx + nx as u32);
            }
        }

        (vertices, indices, depth_min, depth_max)
    }

    /// Render a frame and return the pixel data as RGBA bytes
    pub async fn render_frame(&self) -> Result<Vec<u8>> {
        // Update uniforms - compute view_proj while holding the lock, then drop it
        let view_proj = {
            let camera = self.camera.read();
            let aspect = self.width as f32 / self.height as f32;
            camera.projection_matrix(aspect) * camera.view_matrix()
        };

        let uniforms = Uniforms {
            view_proj: view_proj.to_cols_array_2d(),
            depth_range: [self.depth_min, self.depth_max],
            _padding: [0.0; 2],
        };

        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        // Render pass
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.render_texture_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.1,
                            b: 0.15,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_texture_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..self.num_indices, 0, 0..1);
        }

        // Copy texture to buffer
        let bytes_per_row = 4 * self.width;
        let padded_bytes_per_row = (bytes_per_row + 255) & !255; // Align to 256

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &self.render_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.output_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));

        // Read buffer
        let buffer_slice = self.output_buffer.slice(..);
        let (tx, rx) = futures::channel::oneshot::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });

        self.device.poll(wgpu::Maintain::Wait);
        rx.await??;

        let data = buffer_slice.get_mapped_range();

        // Remove padding and extract RGBA data
        let mut output = Vec::with_capacity((self.width * self.height * 4) as usize);
        for row in 0..self.height {
            let start = (row * padded_bytes_per_row) as usize;
            let end = start + (self.width * 4) as usize;
            output.extend_from_slice(&data[start..end]);
        }

        drop(data);
        self.output_buffer.unmap();

        Ok(output)
    }

    /// Handle input event from client
    pub fn handle_input(&self, event: &InputEvent) {
        let mut camera = self.camera.write();
        match event {
            InputEvent::Rotate { dx, dy } => camera.rotate(*dx, *dy),
            InputEvent::Zoom { delta } => camera.zoom(*delta),
            InputEvent::Pan { dx, dy } => camera.pan(*dx, *dy),
            InputEvent::Reset => camera.reset(),
            InputEvent::SetCamera { azimuth, elevation, distance, focal_point } => {
                camera.azimuth = *azimuth;
                camera.elevation = *elevation;
                camera.distance = *distance;
                camera.focal_point = Vec3::from_array(*focal_point);
            }
            InputEvent::LoadHorizon { url: _ } => {
                // TODO: Implement horizon loading from URL
                tracing::info!("LoadHorizon requested (not yet implemented)");
            }
        }
    }
}

/// Input events from client
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    // Camera controls
    Rotate { dx: f32, dy: f32 },
    Zoom { delta: f32 },
    Pan { dx: f32, dy: f32 },
    Reset,

    // Camera state (for external viewer integration)
    SetCamera {
        azimuth: f32,
        elevation: f32,
        distance: f32,
        focal_point: [f32; 3],
    },

    // Data loading (for external viewer integration)
    LoadHorizon {
        url: String,  // URL to horizon data (SEG-Y, OpenVDS, etc.)
    },
}
