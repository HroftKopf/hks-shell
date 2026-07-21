//! GPU rendering of the glass surface (wgpu).
//!
//! The client draws only the surface silhouette mask (shape + tint + rim) as
//! premultiplied alpha; the actual blur/refraction is applied by the niri-glass
//! compositor via the layer-rule. This module is shared by every hks-shell
//! surface (launcher, bar, popups) so the glass code lives in exactly one place.

use std::ffi::c_void;
use std::ptr::NonNull;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};

use glyphon::{
    Attrs, Buffer, Cache as GlyphonCache, Color as TextColor, CustomGlyph, Family, FontSystem,
    Metrics, Resolution, Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport,
};

mod icons;
use std::collections::HashMap;

use icons::{IconSource, rasterize_icon, resolve_icon};

// Search-bar text layout (logical px).
const TEXT_LEFT: f32 = 68.0; // query text aligns with the result-name column
const TEXT_TOP: f32 = 12.0;
const FONT_SIZE: f32 = 20.0;
const LINE_HEIGHT: f32 = 26.0;

/// Search-bar height, and per-result row height. `pub` so the app sizes the
/// panel to fit the result count using the same layout numbers.
/// Output scale we render at. The buffer is rendered at RENDER_SCALE× the
/// logical size (physical resolution) and a wp_viewport maps it back to the
/// logical size, so the fractional-scale output shows a crisp 1:1 image.
pub const RENDER_SCALE: f32 = 1.2;

/// Logical px -> physical (buffer) px.
fn physical(logical: u32) -> u32 {
    ((logical as f32) * RENDER_SCALE).round().max(1.0) as u32
}

pub const BAR_H: f32 = 50.0;
pub const ROW_H: f32 = 48.0;
const RESULT_FONT: f32 = 15.0;
const SUB_FONT: f32 = 13.0;
const RESULT_LEFT: f32 = 68.0; // text start (leaves room for the icon)
const RESULTS_TOP: f32 = 58.0;
// cosmic-text centers the line within ROW_H; small nudge for font-metric bias.
const NAME_DY: f32 = 3.0;
const ICON_LEFT: f32 = 20.0;
const ICON_SIZE: f32 = 33.0; // grey tile size
const ICON_INNER: f32 = 24.0; // app logo size, inset within the tile

/// Tunable glass appearance — the former `GlassStyle`, now feeding a GPU uniform.
#[derive(Clone, Copy)]
pub struct GlassParams {
    pub radius: f32,
    pub edge_feather: f32,
    pub material_fade_width: f32,
    pub edge_alpha_scale: f32,
    pub base_alpha: f32,
    pub border_width: f32,
    pub highlight_strength: f32,
    pub tint_rgb: [f32; 3],
}

impl Default for GlassParams {
    fn default() -> Self {
        Self {
            radius: 25.0,
            edge_feather: 3.5,
            material_fade_width: 20.0,
            // Keep the darkening nearly uniform to the edge (higher = less of a
            // light rim where the bright frost would otherwise show through).
            edge_alpha_scale: 0.7,
            // Rei's idea: darken the backdrop (frost * ~0.8) instead of a grey
            // fill. A near-black tint at this alpha is an alpha-over multiply,
            // so refraction stays visible — just dimmed — giving contrast for
            // white content on any background.
            base_alpha: 0.30,
            border_width: 10.0,
            highlight_strength: 0.03,
            tint_rgb: [0.02, 0.02, 0.04],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    resolution: [f32; 2],
    radius: f32,
    edge_feather: f32,
    material_fade_width: f32,
    edge_alpha_scale: f32,
    base_alpha: f32,
    border_width: f32,
    highlight_strength: f32,
    // Selected-row highlight band (sel_height <= 0 => no selection).
    sel_top: f32,
    sel_height: f32,
    row_count: f32,
    tint: [f32; 3],
    _pad3: f32,
}

impl Uniforms {
    fn new(
        params: &GlassParams,
        width: u32,
        height: u32,
        sel_top: f32,
        sel_height: f32,
        row_count: f32,
    ) -> Self {
        Self {
            resolution: [width as f32, height as f32],
            radius: params.radius,
            edge_feather: params.edge_feather,
            material_fade_width: params.material_fade_width,
            edge_alpha_scale: params.edge_alpha_scale,
            base_alpha: params.base_alpha,
            border_width: params.border_width,
            highlight_strength: params.highlight_strength,
            sel_top,
            sel_height,
            row_count,
            tint: params.tint_rgb,
            _pad3: 0.0,
        }
    }
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    params: GlassParams,
    sel_top: f32,
    sel_height: f32,
    row_count: f32,

    // Text rendering (glyphon).
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    text_atlas: TextAtlas,
    text_renderer: TextRenderer,
    text_buffer: Buffer,
    results_buffer: Buffer,
    subs_buffer: Buffer,
    icon_anchor: Buffer,
    text_color: TextColor,

    icon_ids: HashMap<String, u16>,
    icon_sources: Vec<IconSource>,
    icon_glyphs: Vec<CustomGlyph>,
}

impl Renderer {
    /// Create a renderer bound to an existing Wayland surface.
    ///
    /// # Safety
    /// `display_ptr` / `surface_ptr` must be valid pointers to the live
    /// `wl_display` / `wl_surface` and outlive this renderer.
    pub fn new(
        display_ptr: *mut c_void,
        surface_ptr: *mut c_void,
        width: u32,
        height: u32,
        params: GlassParams,
    ) -> Self {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());

        let raw_display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(display_ptr).expect("null wl_display pointer"),
        ));
        let raw_window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(surface_ptr).expect("null wl_surface pointer"),
        ));

        let surface = unsafe {
            instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(raw_display),
                    raw_window_handle: raw_window,
                })
                .expect("failed to create wgpu surface")
        };

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("no suitable GPU adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("hks-shell device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .expect("failed to request device");

        let caps = surface.get_capabilities(&adapter);
        // Prefer a plain (non-sRGB) UNORM format so shader colors map directly.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .unwrap_or(caps.formats[0]);
        // Prefer premultiplied alpha for the translucent surface.
        let alpha_mode = caps
            .alpha_modes
            .iter()
            .copied()
            .find(|m| *m == wgpu::CompositeAlphaMode::PreMultiplied)
            .unwrap_or(caps.alpha_modes[0]);

        let mut config = surface
            .get_default_config(&adapter, physical(width), physical(height))
            .expect("surface not supported by adapter");
        config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        config.format = format;
        config.alpha_mode = alpha_mode;
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        // Text rendering setup (glyphon). Bundle Inter so the shell looks the
        // same regardless of what fonts the system has installed.
        let mut font_system = FontSystem::new();
        font_system
            .db_mut()
            .load_font_data(include_bytes!("../../assets/fonts/Inter-Bold.ttf").to_vec());
        let swash_cache = SwashCache::new();
        let glyphon_cache = GlyphonCache::new(&device);
        let viewport = Viewport::new(&device, &glyphon_cache);
        let mut text_atlas = TextAtlas::new(&device, &queue, &glyphon_cache, format);
        let text_renderer =
            TextRenderer::new(&mut text_atlas, &device, wgpu::MultisampleState::default(), None);
        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        text_buffer.set_size(Some(width as f32), Some(height as f32));
        text_buffer.shape_until_scroll(&mut font_system, false);
        let mut results_buffer = Buffer::new(&mut font_system, Metrics::new(RESULT_FONT, ROW_H));
        results_buffer.set_size(Some(width as f32), Some(height as f32));
        results_buffer.shape_until_scroll(&mut font_system, false);
        let mut subs_buffer = Buffer::new(&mut font_system, Metrics::new(SUB_FONT, ROW_H));
        subs_buffer.set_size(Some(width as f32), Some(height as f32));
        subs_buffer.shape_until_scroll(&mut font_system, false);
        // Empty buffer at (0,0) that carries the icon custom-glyphs so their
        // positions are absolute (glyph coords are relative to the TextArea).
        let mut icon_anchor = Buffer::new(&mut font_system, Metrics::new(RESULT_FONT, ROW_H));
        icon_anchor.set_size(Some(width as f32), Some(height as f32));
        let text_color = TextColor::rgb(55, 55, 65);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glass shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../assets/shaders/glass.wgsl").into(),
            ),
        });

        let uniforms = Uniforms::new(&params, config.width, config.height, 0.0, 0.0, 0.0);
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("glass bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass bind group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glass pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glass pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            uniform_buffer,
            bind_group,
            params,
            sel_top: 0.0,
            sel_height: 0.0,
            row_count: 0.0,

            font_system,
            swash_cache,
            viewport,
            text_atlas,
            text_renderer,
            text_buffer,
            results_buffer,
            subs_buffer,
            icon_anchor,
            text_color,

            icon_ids: HashMap::new(),
            icon_sources: Vec::new(),
            icon_glyphs: Vec::new(),
        }
    }

    /// Set the result rows: title, (possibly empty) subtitle, and optional icon
    /// name per row.
    pub fn set_results(
        &mut self,
        titles: &[String],
        subtitles: &[String],
        icons: &[Option<String>],
    ) {
        let attrs = Attrs::new().family(Family::Name("Inter"));
        self.results_buffer
            .set_text(&titles.join("\n"), &attrs, Shaping::Advanced, None);
        self.results_buffer
            .shape_until_scroll(&mut self.font_system, false);
        self.subs_buffer
            .set_text(&subtitles.join("\n"), &attrs, Shaping::Advanced, None);
        self.subs_buffer
            .shape_until_scroll(&mut self.font_system, false);

        self.row_count = titles.len() as f32;
        self.icon_glyphs.clear();
        for (i, icon) in icons.iter().enumerate() {
            let Some(name) = icon else { continue };
            let Some(id) = self.icon_id(name) else { continue };
            self.icon_glyphs.push(CustomGlyph {
                id,
                left: ICON_LEFT + (ICON_SIZE - ICON_INNER) * 0.5,
                top: RESULTS_TOP + i as f32 * ROW_H + (ROW_H - ICON_INNER) * 0.5,
                width: ICON_INNER,
                height: ICON_INNER,
                color: None,
                snap_to_physical_pixel: true,
                metadata: 0,
            });
        }
    }

    /// Get (resolving+decoding on first use) a stable custom-glyph id for an
    /// icon name; `None` if it can't be resolved.
    fn icon_id(&mut self, name: &str) -> Option<u16> {
        if let Some(&id) = self.icon_ids.get(name) {
            return Some(id);
        }
        let source = resolve_icon(name)?;
        let id = self.icon_sources.len() as u16;
        self.icon_sources.push(source);
        self.icon_ids.insert(name.to_string(), id);
        Some(id)
    }

    /// Set the search-bar text (the query, or a dimmer placeholder when empty).
    pub fn set_text(&mut self, text: &str, placeholder: bool) {
        // White content over the darkened backdrop.
        self.text_color = if placeholder {
            TextColor::rgba(255, 255, 255, 180)
        } else {
            TextColor::rgba(255, 255, 255, 255)
        };
        self.text_buffer.set_text(
            text,
            &Attrs::new().family(Family::Name("Inter")),
            Shaping::Advanced,
            None,
        );
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = physical(width);
        self.config.height = physical(height);
        self.surface.configure(&self.device, &self.config);
        self.text_buffer
            .set_size(Some(width as f32), Some(height as f32));
        self.results_buffer
            .set_size(Some(width as f32), Some(height as f32));
        self.subs_buffer
            .set_size(Some(width as f32), Some(height as f32));
        self.icon_anchor
            .set_size(Some(width as f32), Some(height as f32));
        self.write_uniforms();
    }

    fn write_uniforms(&self) {
        let uniforms = Uniforms::new(
            &self.params,
            self.config.width,
            self.config.height,
            self.sel_top,
            self.sel_height,
            self.row_count,
        );
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    /// Set the highlighted result row (row 0 is the first row under the bar),
    /// or clear the highlight with `None`.
    pub fn set_selection(&mut self, selected: Option<usize>) {
        match selected {
            Some(i) => {
                self.sel_top = RESULTS_TOP + i as f32 * ROW_H;
                self.sel_height = ROW_H;
            }
            None => self.sel_height = 0.0,
        }
        self.write_uniforms();
    }

    pub fn render(&mut self) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                t
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            _ => return,
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Prepare text for this frame.
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );
        let icon_sources = &self.icon_sources;
        self.text_renderer
            .prepare_with_custom(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.text_atlas,
                &self.viewport,
                [
                    // Soft drop shadow: same text, dark, offset down.
                    TextArea {
                        buffer: &self.text_buffer,
                        left: TEXT_LEFT,
                        top: TEXT_TOP + 1.5,
                        scale: RENDER_SCALE,
                        bounds: TextBounds {
                            left: 0,
                            top: 0,
                            right: self.config.width as i32,
                            bottom: self.config.height as i32,
                        },
                        default_color: TextColor::rgba(0, 0, 0, 90),
                        custom_glyphs: &[],
                    },
                    // Main text on top.
                    TextArea {
                        buffer: &self.text_buffer,
                        left: TEXT_LEFT,
                        top: TEXT_TOP,
                        scale: RENDER_SCALE,
                        bounds: TextBounds {
                            left: 0,
                            top: 0,
                            right: self.config.width as i32,
                            bottom: self.config.height as i32,
                        },
                        default_color: self.text_color,
                        custom_glyphs: &[],
                    },
                    // Result row titles.
                    TextArea {
                        buffer: &self.results_buffer,
                        left: RESULT_LEFT,
                        top: RESULTS_TOP + NAME_DY,
                        scale: RENDER_SCALE,
                        bounds: TextBounds {
                            left: 0,
                            top: 0,
                            right: self.config.width as i32,
                            bottom: self.config.height as i32,
                        },
                        default_color: TextColor::rgba(255, 255, 255, 235),
                        custom_glyphs: &[],
                    },
                    // Row icons (custom glyphs), anchored at (0,0) so the glyph
                    // positions set in set_results() are absolute.
                    TextArea {
                        buffer: &self.icon_anchor,
                        left: 0.0,
                        top: 0.0,
                        scale: RENDER_SCALE,
                        bounds: TextBounds {
                            left: 0,
                            top: 0,
                            right: self.config.width as i32,
                            bottom: self.config.height as i32,
                        },
                        default_color: TextColor::rgba(255, 255, 255, 255),
                        custom_glyphs: &self.icon_glyphs,
                    },
                ],
                &mut self.swash_cache,
                |req| rasterize_icon(icon_sources, req),
            )
            .expect("text prepare failed");

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glass encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glass pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);

            // Text on top of the glass, same pass.
            self.text_renderer
                .render(&self.text_atlas, &self.viewport, &mut pass)
                .expect("text render failed");
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.text_atlas.trim();
    }
}
