//! FileMan-style winit lifecycle and Blade/egui rendering.

use blade_egui as be;
use blade_graphics as bg;
use std::{fs, io, path, sync, time};

use crate::{desktop, ui};
use anyhow::Context;

const INITIAL_SIZE: (u32, u32) = (1280, 760);

#[derive(Debug)]
enum Event {
    Repaint(time::Instant),
}

struct Runtime {
    window: winit::window::Window,
    context: bg::Context,
    surface: bg::Surface,
    config: bg::SurfaceConfig,
    surface_info: bg::SurfaceInfo,
    encoder: bg::CommandEncoder,
    last_sync: Option<bg::SyncPoint>,
    pending_view: Option<bg::TextureView>,
    painter: be::GuiPainter,
    input: egui_winit::State,
    size: winit::dpi::PhysicalSize<u32>,
}

impl Runtime {
    fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        ctx: &egui::Context,
    ) -> anyhow::Result<Self> {
        #[allow(unused_mut)]
        let mut attributes = winit::window::Window::default_attributes()
            .with_title("Starcom")
            .with_inner_size(winit::dpi::LogicalSize::new(INITIAL_SIZE.0, INITIAL_SIZE.1))
            .with_min_inner_size(winit::dpi::LogicalSize::new(640, 400));
        #[cfg(target_os = "linux")]
        {
            use winit::platform::{
                wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11,
            };
            attributes = WindowAttributesExtWayland::with_name(attributes, "starcom", "starcom");
            attributes = WindowAttributesExtX11::with_name(attributes, "starcom", "starcom");
        }
        let window = event_loop
            .create_window(attributes)
            .context("create Starcom window")?;
        // SAFETY: the returned graphics context and surface are confined to this
        // event-loop thread; the window outlives the explicitly destroyed surface.
        let context = unsafe {
            bg::Context::init(bg::ContextDesc {
                presentation: true,
                validation: cfg!(debug_assertions),
                ..Default::default()
            })
        }
        .map_err(|error| anyhow::anyhow!("GPU initialization failed: {error:?}"))?;
        let size = window.inner_size();
        let config = bg::SurfaceConfig {
            size: bg::Extent {
                width: size.width.max(1),
                height: size.height.max(1),
                depth: 1,
            },
            usage: bg::TextureUsage::TARGET,
            ..Default::default()
        };
        let surface = context
            .create_surface_configured(&window, config)
            .map_err(|error| anyhow::anyhow!("GPU surface creation failed: {error:?}"))?;
        let surface_info = surface.info();
        let painter = be::GuiPainter::new(surface_info, &context);
        let encoder = context.create_command_encoder(bg::CommandEncoderDesc {
            name: "starcom",
            buffer_count: 1,
        });
        let input = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        Ok(Self {
            window,
            context,
            surface,
            config,
            surface_info,
            encoder,
            last_sync: None,
            pending_view: None,
            painter,
            input,
            size,
        })
    }

    fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) -> anyhow::Result<()> {
        self.size = size;
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        self.finish_frame()?;
        self.config.size.width = size.width;
        self.config.size.height = size.height;
        self.context
            .reconfigure_surface(&mut self.surface, self.config);
        let info = self.surface.info();
        // The painter owns the font atlas. Replacing it silently would lose
        // textures unless egui reuploads them, so fail explicitly on this rare
        // platform change rather than displaying a corrupted window.
        anyhow::ensure!(
            info == self.surface_info,
            "surface format changed; reopen Starcom"
        );
        Ok(())
    }

    fn finish_frame(&mut self) -> anyhow::Result<()> {
        if let Some(ref sync) = self.last_sync {
            self.context
                .wait_for(sync, !0)
                .map_err(|error| anyhow::anyhow!("GPU frame wait failed: {error:?}"))?;
        }
        if let Some(view) = self.pending_view.take() {
            self.context.destroy_texture_view(view);
        }
        self.last_sync = None;
        Ok(())
    }

    fn paint(&mut self, ctx: &egui::Context, output: egui::FullOutput) -> anyhow::Result<()> {
        self.input
            .handle_platform_output(&self.window, output.platform_output);
        let jobs = ctx.tessellate(output.shapes, output.pixels_per_point);
        let screen = be::ScreenDescriptor {
            physical_size: (self.size.width, self.size.height),
            scale_factor: output.pixels_per_point,
        };
        self.finish_frame()?;
        self.encoder.start();
        self.painter
            .update_textures(&mut self.encoder, &output.textures_delta, &self.context);
        let frame = self.surface.acquire_frame();
        self.encoder.init_texture(frame.texture());
        let view = self.context.create_texture_view(
            frame.texture(),
            bg::TextureViewDesc {
                name: "starcom surface",
                format: self.surface_info.format,
                dimension: bg::ViewDimension::D2,
                subresources: &bg::TextureSubresources::default(),
            },
        );
        {
            let mut pass = self.encoder.render(
                "starcom",
                bg::RenderTargetSet {
                    colors: &[bg::RenderTarget {
                        view,
                        init_op: bg::InitOp::Clear(bg::TextureColor::OpaqueBlack),
                        finish_op: bg::FinishOp::Store,
                    }],
                    depth_stencil: None,
                },
            );
            self.painter.paint(&mut pass, &jobs, &screen, &self.context);
        }
        self.encoder.present(frame);
        let sync = self.context.submit(&mut self.encoder);
        self.painter.after_submit(&sync);
        self.last_sync = Some(sync);
        // Keep the attachment alive until the submitted GPU work completes.
        self.pending_view = Some(view);
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.finish_frame();
        self.context.destroy_command_encoder(&mut self.encoder);
        self.painter.destroy(&self.context);
        self.context.destroy_surface(&mut self.surface);
    }
}

struct App {
    client: desktop::Client,
    ui: ui::DesktopUi,
    ctx: egui::Context,
    runtime: Option<Runtime>,
    next_repaint: Option<time::Instant>,
    error: Option<anyhow::Error>,
}

impl App {
    fn schedule(&mut self, when: time::Instant) {
        self.next_repaint = Some(
            self.next_repaint
                .map_or(when, |previous| previous.min(when)),
        );
    }
}

impl winit::application::ApplicationHandler<Event> for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.runtime.is_some() {
            return;
        }
        match Runtime::new(event_loop, &self.ctx) {
            Ok(runtime) => {
                log::info!("Starcom desktop initialized");
                runtime.window.request_redraw();
                self.runtime = Some(runtime);
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        let Some(ref mut runtime) = self.runtime else {
            return;
        };
        if runtime.window.id() != window_id {
            return;
        }
        let response = runtime.input.on_window_event(&runtime.window, &event);
        if response.repaint {
            runtime.window.request_redraw();
        }
        match event {
            winit::event::WindowEvent::CloseRequested => {
                self.client.disconnect();
                event_loop.exit();
            }
            winit::event::WindowEvent::Resized(size) => {
                if let Err(error) = runtime.resize(size) {
                    self.error = Some(error);
                    event_loop.exit();
                } else {
                    runtime.window.request_redraw();
                }
            }
            winit::event::WindowEvent::RedrawRequested => {
                if runtime.size.width == 0 || runtime.size.height == 0 {
                    return;
                }
                if self
                    .next_repaint
                    .is_some_and(|when| when <= time::Instant::now())
                {
                    self.next_repaint = None;
                }
                let input = runtime.input.take_egui_input(&runtime.window);
                let mut action = ui::Action::None;
                let output = self.ctx.run_ui(input, |root| {
                    let candidate = self.ui.show(root, &mut self.client.lock());
                    if !matches!(candidate, ui::Action::None) {
                        action = candidate;
                    }
                });
                if let Err(error) = runtime.paint(&self.ctx, output) {
                    self.error = Some(error);
                    event_loop.exit();
                    return;
                }
                let outcome = match action {
                    ui::Action::None => Ok(()),
                    ui::Action::Disconnect => {
                        self.client.disconnect();
                        Ok(())
                    }
                    ui::Action::Demo => self.client.demo(),
                    ui::Action::Connect(connection) => self.client.connect(connection),
                };
                if let Err(error) = outcome {
                    self.client.lock().error = Some(error.to_string());
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, event: Event) {
        match event {
            Event::Repaint(when) => self.schedule(when),
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
        if let Some(when) = self.next_repaint {
            if when <= time::Instant::now() {
                self.next_repaint = None;
                if let Some(ref runtime) = self.runtime {
                    runtime.window.request_redraw();
                }
            } else {
                event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(when));
            }
        }
    }
}

pub(crate) fn configure(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
    let mut style = (*ctx.global_style()).clone();
    style.animation_time = 0.0;
    style.spacing.item_spacing = egui::vec2(7.0, 6.0);
    style.visuals.panel_fill = egui::Color32::from_rgb(27, 30, 36);
    ctx.set_global_style(style);
}

pub fn run(startup: desktop::Startup) -> anyhow::Result<()> {
    let event_loop = winit::event_loop::EventLoop::<Event>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let ctx = egui::Context::default();
    configure(&ctx);
    ctx.set_request_repaint_callback(move |info| {
        if let Some(when) = time::Instant::now().checked_add(info.delay) {
            let _ = proxy.send_event(Event::Repaint(when));
        }
    });
    let wake_ctx = ctx.clone();
    let client = desktop::Client::new(sync::Arc::new(move || wake_ctx.request_repaint()))?;
    if startup == desktop::Startup::Demo {
        client.demo()?;
    }
    let mut app = App {
        client,
        ui: ui::DesktopUi::default(),
        ctx,
        runtime: None,
        next_repaint: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    app.runtime.take();
    if let Some(error) = app.error {
        return Err(error);
    }
    Ok(())
}

/// Render the actual desktop UI with deterministic demo data. No SSH, display
/// server, screen capture, or invented image is involved; this uses Blade itself.
pub fn save_snapshot(
    state: &mut desktop::State,
    ui: &mut ui::DesktopUi,
    path: &path::Path,
) -> anyhow::Result<()> {
    // SAFETY: offscreen resources are used on this thread and destroyed only
    // after waiting for GPU completion before reading the shared buffer.
    let context = unsafe { bg::Context::init(bg::ContextDesc::default()) }
        .map_err(|error| anyhow::anyhow!("offscreen GPU initialization failed: {error:?}"))?;
    let size = bg::Extent {
        width: INITIAL_SIZE.0,
        height: INITIAL_SIZE.1,
        depth: 1,
    };
    // The painter emits linear light; an sRGB target encodes PNG bytes
    // correctly instead of exporting dark linear values as sRGB.
    let format = bg::TextureFormat::Rgba8UnormSrgb;
    let mut painter = be::GuiPainter::new(
        bg::SurfaceInfo {
            format,
            alpha: bg::AlphaMode::PreMultiplied,
        },
        &context,
    );
    let mut encoder = context.create_command_encoder(bg::CommandEncoderDesc {
        name: "desktop snapshot",
        buffer_count: 1,
    });
    let texture = context.create_texture(bg::TextureDesc {
        name: "desktop snapshot",
        format,
        size,
        array_layer_count: 1,
        mip_level_count: 1,
        sample_count: 1,
        dimension: bg::TextureDimension::D2,
        usage: bg::TextureUsage::TARGET | bg::TextureUsage::COPY,
        external: None,
    });
    let view = context.create_texture_view(
        texture,
        bg::TextureViewDesc {
            name: "desktop snapshot",
            format,
            dimension: bg::ViewDimension::D2,
            subresources: &bg::TextureSubresources::default(),
        },
    );
    let stride = (size.width * 4).div_ceil(256) * 256;
    let buffer = context.create_buffer(bg::BufferDesc {
        name: "desktop readback",
        size: u64::from(stride) * u64::from(size.height),
        memory: bg::Memory::Shared,
    });
    let result = (|| {
        let ctx = egui::Context::default();
        configure(&ctx);
        let mut textures = egui::TexturesDelta::default();
        let mut shapes = Vec::new();
        // Let panel sizes and sticky-bottom scroll positions settle. Preserve
        // all font texture deltas, not just those from the final layout pass.
        for pass in 0..3 {
            let screen = egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size.width as f32, size.height as f32),
            );
            let input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(pass as f64 / 60.0),
                viewports: [(
                    egui::ViewportId::ROOT,
                    egui::ViewportInfo {
                        native_pixels_per_point: Some(1.0),
                        inner_rect: Some(screen),
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |root| {
                ui.show(root, state);
            });
            textures.append(output.textures_delta);
            shapes = output.shapes;
        }
        let jobs = ctx.tessellate(shapes, 1.0);
        encoder.start();
        encoder.init_texture(texture);
        painter.update_textures(&mut encoder, &textures, &context);
        {
            let mut pass = encoder.render(
                "desktop snapshot",
                bg::RenderTargetSet {
                    colors: &[bg::RenderTarget {
                        view,
                        init_op: bg::InitOp::Clear(bg::TextureColor::OpaqueBlack),
                        finish_op: bg::FinishOp::Store,
                    }],
                    depth_stencil: None,
                },
            );
            painter.paint(
                &mut pass,
                &jobs,
                &be::ScreenDescriptor {
                    physical_size: (size.width, size.height),
                    scale_factor: 1.0,
                },
                &context,
            );
        }
        {
            let mut transfer = encoder.transfer("desktop readback");
            transfer.copy_texture_to_buffer(
                bg::TexturePiece {
                    texture,
                    mip_level: 0,
                    array_layer: 0,
                    origin: [0, 0, 0],
                },
                buffer.into(),
                stride,
                size,
            );
        }
        let sync = context.submit(&mut encoder);
        painter.after_submit(&sync);
        context
            .wait_for(&sync, !0)
            .map_err(|error| anyhow::anyhow!("snapshot GPU wait failed: {error:?}"))?;
        // SAFETY: buffer is Shared, sized for every padded row, and the GPU
        // write has completed. Copy into owned memory before destroying it.
        let mapped = unsafe {
            std::slice::from_raw_parts(buffer.data(), stride as usize * size.height as usize)
        };
        let mut rgba = Vec::with_capacity(size.width as usize * size.height as usize * 4);
        for row in mapped.chunks_exact(stride as usize) {
            rgba.extend_from_slice(&row[..size.width as usize * 4]);
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut png = png::Encoder::new(
            io::BufWriter::new(fs::File::create(path)?),
            size.width,
            size.height,
        );
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.set_compression(png::Compression::High);
        let mut writer = png.write_header()?;
        writer.write_image_data(&rgba)?;
        writer.finish()?;
        Ok(())
    })();
    context.destroy_texture_view(view);
    context.destroy_texture(texture);
    context.destroy_buffer(buffer);
    painter.destroy(&context);
    context.destroy_command_encoder(&mut encoder);
    result
}
