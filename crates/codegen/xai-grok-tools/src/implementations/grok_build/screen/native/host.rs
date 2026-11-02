//! Wayland layer surface, bounded command transport, and native input dispatch.

use super::{
    render::Renderer,
    scene::{Click, Interaction, Model, Rect, close_button, reset_button},
};
use anyhow::{Context, Result};
use glam::Vec2;
use serde_json::json;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_dispatch2, delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, channel},
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Proxy, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
            },
            viewporter::client::{wp_viewport, wp_viewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::io::{BufRead, Read, Write};
use std::time::Duration;
use xai_grok_tools::implementations::grok_build::screen::{
    DrawOverlayInput, OverlayAction, OverlayScene,
};

const MAX_REQUEST: u64 = 1024 * 1024;

enum Input {
    Line(Vec<u8>),
    Failed(String),
    End,
}

pub(super) fn run() -> Result<()> {
    let connection = Connection::connect_to_env().context("Cannot connect to Wayland")?;
    let (globals, mut queue) = registry_queue_init::<Host>(&connection)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell =
        LayerShell::bind(&globals, &qh).context("Compositor does not support desktop overlays")?;
    let shm = Shm::bind(&globals, &qh)?;
    let pool = SlotPool::new(4, &shm)?;
    let fractional = globals.bind(&qh, 1..=1, Ignore).ok();
    let viewporter = globals.bind(&qh, 1..=1, Ignore).ok();
    let seats = SeatState::new(&globals, &qh);
    // new_seat reports hotplugged seats only. Bind capabilities for seats that
    // were already present before processing their initial capability events.
    let devices = seats.seats().map(Seat::new).collect();
    let mut host = Host {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats,
        devices,
        compositor,
        shell,
        shm,
        pool,
        fractional,
        viewporter,
        current: None,
        renderer: Renderer::new(),
        error: None,
        exit: false,
        last_output: String::new(),
    };
    queue.roundtrip(&mut host)?;
    let mut event_loop = EventLoop::<Host>::try_new()?;
    WaylandSource::new(connection, queue).insert(event_loop.handle())?;
    let (sender, receiver) = channel::sync_channel(4);
    std::thread::Builder::new()
        .name("overlay-commands".to_owned())
        .spawn(move || {
            let mut reader = std::io::stdin().lock();
            loop {
                let mut line = Vec::new();
                let message = match reader
                    .by_ref()
                    .take(MAX_REQUEST + 1)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) => Input::End,
                    Ok(_) if line.len() as u64 > MAX_REQUEST => {
                        Input::Failed("Overlay request exceeds 1 MiB".to_owned())
                    }
                    Ok(_) if line.last() != Some(&b'\n') => {
                        Input::Failed("Incomplete overlay request".to_owned())
                    }
                    Ok(_) => Input::Line(line),
                    Err(error) => Input::Failed(format!("Cannot read overlay request: {error}")),
                };
                let done = !matches!(message, Input::Line(_));
                if sender.send(message).is_err() || done {
                    break;
                }
            }
        })?;
    let request_qh = qh.clone();
    event_loop
        .handle()
        .insert_source(receiver, move |event, _, host| match event {
            channel::Event::Msg(Input::Line(line)) => {
                if let Err(error) = host.request(&line, &request_qh) {
                    host.error = Some(error);
                }
            }
            channel::Event::Msg(Input::Failed(error)) => host.error = Some(anyhow::anyhow!(error)),
            channel::Event::Msg(Input::End) | channel::Event::Closed => host.exit = true,
        })
        .map_err(|error| anyhow::anyhow!("Cannot register command channel: {}", error.error))?;
    while !host.exit {
        event_loop.dispatch(Duration::from_millis(100), &mut host)?;
        if let Some(error) = host.error.take() {
            return Err(error);
        }
        host.paint(&qh)?;
    }
    Ok(())
}

struct Seat {
    seat: wl_seat::WlSeat,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
}

impl Seat {
    fn new(seat: wl_seat::WlSeat) -> Self {
        Self {
            seat,
            keyboard: None,
            pointer: None,
        }
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        if let Some(keyboard) = self.keyboard.take()
            && keyboard.version() >= 3
        {
            keyboard.release();
        }
        self.pointer = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FramePurpose {
    Repaint,
    Request,
}

struct Surface {
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    viewport: Option<wp_viewport::WpViewport>,
    layer: LayerSurface,
    output: Option<wl_output::WlOutput>,
    output_name: String,
    scene: OverlayScene,
    model: Option<Model>,
    size: (u32, u32),
    scale: f32,
    configured: bool,
    dirty: bool,
    frame: Option<FramePurpose>,
    pending: bool,
    interaction: Interaction,
}

impl Drop for Surface {
    fn drop(&mut self) {
        if let Some(fractional) = &self.fractional {
            fractional.destroy();
        }
        if let Some(viewport) = &self.viewport {
            viewport.destroy();
        }
    }
}

impl Surface {
    fn areas(&self) -> Vec<Rect> {
        let (width, height) = (self.size.0 as f32, self.size.1 as f32);
        let mut areas = vec![close_button(width)];
        if let Some(model) = &self.model {
            areas.push(reset_button(width));
            if let Some(bounds) = model.interactive_bounds(width, height) {
                areas.push(bounds);
            }
        }
        areas
    }
}

struct Host {
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    devices: Vec<Seat>,
    compositor: CompositorState,
    shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    current: Option<Surface>,
    renderer: Renderer,
    last_output: String,
    error: Option<anyhow::Error>,
    exit: bool,
}

fn reply(value: serde_json::Value) -> Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

impl Host {
    fn request(&mut self, line: &[u8], qh: &QueueHandle<Self>) -> Result<()> {
        let mut request: DrawOverlayInput = match serde_json::from_slice(line) {
            Ok(request) => request,
            Err(error) => {
                return reply(
                    json!({"error":format!("Invalid overlay JSON: {error}"),"kind":"rejected"}),
                );
            }
        };
        // The original host also accepts an empty name for an unspecified output.
        if request.output.as_deref() == Some("") {
            request.output = None;
        }
        if self.current.as_ref().is_some_and(|surface| surface.pending) {
            return reply(json!({"error":"A drawing is already pending","kind":"rejected"}));
        }
        let selected = if let Some(name) = request.output.as_deref() {
            match self.outputs.outputs().find(|output| {
                self.outputs
                    .info(output)
                    .is_some_and(|info| info.name.as_deref() == Some(name))
            }) {
                Some(output) => Some(output),
                None => {
                    return reply(
                        json!({"error":format!("Unknown desktop output: {name}"),"kind":"rejected"}),
                    );
                }
            }
        } else {
            None
        };
        match request.action {
            OverlayAction::Clear => {
                if request.output.as_ref().is_some_and(|name| {
                    self.current
                        .as_ref()
                        .is_some_and(|surface| *name != surface.output_name)
                }) {
                    return reply(
                        json!({"error":"This session has no overlay on that output","kind":"rejected"}),
                    );
                }
                self.dismiss()?;
                reply(json!({"output":self.last_output,"state":"cleared"}))
            }
            OverlayAction::Show { scene } => {
                if let Err(error) = scene.validate() {
                    return reply(json!({"error":error.to_string(),"kind":"rejected"}));
                }
                let model = if let OverlayScene::Model3d { parts } = &scene {
                    Some(Model::new(parts))
                } else {
                    None
                };
                if let Some(surface) = self.current.as_mut().filter(|surface| {
                    selected
                        .as_ref()
                        .is_none_or(|output| surface.output.as_ref() == Some(output))
                }) {
                    surface.scene = scene;
                    surface.model = model;
                    surface.pending = true;
                    surface.dirty = true;
                    surface.interaction.cancel();
                    return Ok(());
                }
                self.current = None;
                let wl_surface = self.compositor.create_surface(qh);
                let layer = self.shell.create_layer_surface(
                    qh,
                    wl_surface,
                    Layer::Overlay,
                    Some("grok-overlay"),
                    selected.as_ref(),
                );
                layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                layer.set_size(0, 0);
                layer.set_exclusive_zone(-1);
                layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
                let (fractional, viewport) = match (&self.fractional, &self.viewporter) {
                    (Some(manager), Some(viewporter)) => (
                        Some(manager.get_fractional_scale(layer.wl_surface(), qh, FractionalData)),
                        Some(viewporter.get_viewport(layer.wl_surface(), qh, Ignore)),
                    ),
                    _ => (None, None),
                };
                let output_name = selected
                    .as_ref()
                    .and_then(|output| self.outputs.info(output))
                    .and_then(|info| info.name)
                    .unwrap_or_default();
                layer.commit();
                self.current = Some(Surface {
                    fractional,
                    viewport,
                    layer,
                    output: selected,
                    output_name,
                    scene,
                    model,
                    size: (0, 0),
                    scale: 1.0,
                    configured: false,
                    dirty: true,
                    frame: None,
                    pending: true,
                    interaction: Interaction::default(),
                });
                Ok(())
            }
        }
    }

    fn dismiss(&mut self) -> Result<()> {
        if let Some(mut surface) = self.current.take() {
            self.last_output = std::mem::take(&mut surface.output_name);
            if surface.pending {
                reply(json!({"error":"Drawing was dismissed before completion","kind":"fatal"}))?;
            }
        }
        Ok(())
    }

    fn paint(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let Some(surface) = self.current.as_mut() else {
            return Ok(());
        };
        if !surface.configured || !surface.dirty || surface.frame.is_some() {
            return Ok(());
        }
        let (width, height) = surface.size;
        let image = self.renderer.draw(
            &surface.scene,
            surface.model.as_ref(),
            width,
            height,
            surface.scale,
        )?;
        let (pixels_w, pixels_h) = (image.width() as i32, image.height() as i32);
        // Prefer the compositor's RGBA-byte format, avoiding a full-frame
        // channel swap. ARGB8888 is the mandatory portable fallback.
        let rgba = cfg!(target_endian = "little")
            && self.shm.formats().contains(&wl_shm::Format::Abgr8888);
        let format = if rgba {
            wl_shm::Format::Abgr8888
        } else {
            wl_shm::Format::Argb8888
        };
        let (buffer, bytes) = self
            .pool
            .create_buffer(pixels_w, pixels_h, pixels_w * 4, format)?;
        if rgba {
            bytes.copy_from_slice(image.data());
        } else {
            for (src, dst) in image.data().chunks_exact(4).zip(bytes.chunks_exact_mut(4)) {
                dst.copy_from_slice(
                    &u32::from_be_bytes([src[3], src[0], src[1], src[2]]).to_ne_bytes(),
                );
            }
        }
        let wl = surface.layer.wl_surface();
        if let Some(viewport) = &surface.viewport {
            wl.set_buffer_scale(1);
            viewport.set_destination(width as i32, height as i32);
        } else {
            wl.set_buffer_scale(surface.scale as i32);
        }
        if !surface.interaction.is_dragging() {
            let region = Region::new(&self.compositor)?;
            for rect in surface.areas() {
                let (x, y, w, h) = rect.input();
                region.add(x, y, w, h);
            }
            wl.set_input_region(Some(region.wl_region()));
        }
        wl.damage_buffer(0, 0, pixels_w, pixels_h);
        wl.frame(qh, FrameCallbackData(wl.clone()));
        buffer.attach_to(wl)?;
        surface.layer.commit();
        surface.dirty = false;
        surface.frame = Some(if surface.pending {
            FramePurpose::Request
        } else {
            FramePurpose::Repaint
        });
        Ok(())
    }

    fn pointer_event(&mut self, event: &PointerEvent) -> Result<()> {
        let Some(surface) = self
            .current
            .as_mut()
            .filter(|s| s.layer.wl_surface() == &event.surface)
        else {
            return Ok(());
        };
        let point = Vec2::new(event.position.0 as f32, event.position.1 as f32);
        let (width, height) = (surface.size.0 as f32, surface.size.1 as f32);
        match event.kind {
            PointerEventKind::Press { button, .. } => {
                surface
                    .interaction
                    .press(button, point, width, surface.model.as_ref(), height);
            }
            PointerEventKind::Release { button, .. } => {
                match surface.interaction.release(button, point, width) {
                    Click::Close => return self.dismiss(),
                    Click::Reset => {
                        if let Some(model) = &mut surface.model {
                            model.camera.reset();
                        }
                    }
                    Click::None => {}
                }
                surface.dirty = true;
            }
            PointerEventKind::Motion { .. } => {
                surface.dirty |= surface
                    .interaction
                    .motion(point, surface.model.as_mut(), height);
            }
            PointerEventKind::Axis { vertical, .. } => {
                if let Some(model) = &mut surface.model {
                    model.camera.zoom(if vertical.value120 != 0 {
                        vertical.value120 as f32
                    } else if vertical.absolute != 0.0 {
                        vertical.absolute as f32 * 8.0
                    } else {
                        vertical.discrete as f32 * 120.0
                    });
                    surface.dirty = true;
                }
            }
            PointerEventKind::Leave { .. } => {
                surface.interaction.cancel();
                surface.dirty = true;
            }
            _ => {}
        }
        Ok(())
    }
}

impl CompositorHandler for Host {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        wl: &wl_surface::WlSurface,
        scale: i32,
    ) {
        if let Some(surface) = self
            .current
            .as_mut()
            .filter(|s| s.layer.wl_surface() == wl && s.fractional.is_none())
        {
            surface.scale = scale.max(1) as f32;
            surface.dirty = true;
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, _: u32) {
        if let Some(surface) = self.current.as_mut().filter(|s| s.layer.wl_surface() == wl) {
            let frame = surface.frame.take();
            // Input may dirty the next frame while this one is being presented.
            // Acknowledge the request's frame, not an earlier repaint, without
            // starving acknowledgement during continuous mouse movement.
            if surface.pending
                && frame == Some(FramePurpose::Request)
                && !surface.output_name.is_empty()
            {
                surface.pending = false;
                if let Err(error) = reply(json!({"output":surface.output_name,"state":"visible"})) {
                    self.error = Some(error);
                }
            }
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        wl: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if let Some(surface) = self.current.as_mut().filter(|s| s.layer.wl_surface() == wl) {
            surface.output = Some(output.clone());
            surface.dirty |= surface.pending;
            if let Some(info) = self.outputs.info(output) {
                surface.output_name = info.name.unwrap_or_default();
            }
        }
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Host {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(surface) = self
            .current
            .as_mut()
            .filter(|s| s.output.as_ref() == Some(&output))
            && let Some(name) = self.outputs.info(&output).and_then(|info| info.name)
        {
            surface.output_name = name;
            surface.dirty |= surface.pending;
        }
    }
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self
            .current
            .as_ref()
            .is_some_and(|s| s.output.as_ref() == Some(&output))
            && let Err(error) = self.dismiss()
        {
            self.error = Some(error);
        }
    }
}

impl LayerShellHandler for Host {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.current.as_ref().is_some_and(|s| s.layer == *layer)
            && let Err(error) = self.dismiss()
        {
            self.error = Some(error);
        }
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(surface) = self.current.as_mut().filter(|s| s.layer == *layer) {
            if surface.configured && surface.size != configure.new_size {
                if let Err(error) = self.dismiss() {
                    self.error = Some(error);
                }
                return;
            }
            if configure.new_size.0 == 0 || configure.new_size.1 == 0 {
                self.error = Some(anyhow::anyhow!("Compositor returned an empty overlay size"));
                return;
            }
            surface.size = configure.new_size;
            surface.configured = true;
            surface.dirty = true;
        }
    }
}

impl SeatHandler for Host {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.devices.push(Seat::new(seat));
    }
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        let Some(device) = self.devices.iter_mut().find(|d| d.seat == seat) else {
            return;
        };
        match capability {
            Capability::Keyboard if device.keyboard.is_none() => {
                match self.seats.get_keyboard(qh, &seat, None) {
                    Ok(keyboard) => device.keyboard = Some(keyboard),
                    Err(error) => self.error = Some(error.into()),
                }
            }
            Capability::Pointer if device.pointer.is_none() => {
                match self.seats.get_pointer_with_theme::<Self, ()>(
                    qh,
                    &seat,
                    self.shm.wl_shm(),
                    self.compositor.create_surface(qh),
                    ThemeSpec::default(),
                ) {
                    Ok(pointer) => device.pointer = Some(pointer),
                    Err(error) => self.error = Some(error.into()),
                }
            }
            _ => {}
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if let Some(device) = self.devices.iter_mut().find(|d| d.seat == seat) {
            if capability == Capability::Keyboard
                && let Some(keyboard) = device.keyboard.take()
                && keyboard.version() >= 3
            {
                keyboard.release();
            }
            if capability == Capability::Pointer {
                device.pointer = None;
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.devices.retain(|d| d.seat != seat);
    }
}

impl PointerHandler for Host {
    fn pointer_frame(
        &mut self,
        connection: &Connection,
        _: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if matches!(event.kind, PointerEventKind::Enter { .. })
                && let Some(themed) = self
                    .devices
                    .iter()
                    .filter_map(|device| device.pointer.as_ref())
                    .find(|themed| themed.pointer() == pointer)
                && let Err(error) = themed.set_cursor(connection, CursorIcon::Default)
            {
                eprintln!("Cannot set overlay cursor: {error}");
            }
            if let Err(error) = self.pointer_event(event) {
                self.error = Some(error);
                break;
            }
        }
    }
}

impl KeyboardHandler for Host {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if event.keysym == Keysym::Escape
            && let Err(error) = self.dismiss()
        {
            self.error = Some(error);
        }
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

struct FractionalData;
impl Dispatch2<wp_fractional_scale_v1::WpFractionalScaleV1, Host> for FractionalData {
    fn event(
        &self,
        state: &mut Host,
        fractional: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(surface) = state
                .current
                .as_mut()
                .filter(|s| s.fractional.as_ref() == Some(fractional))
        {
            surface.scale = scale as f32 / 120.0;
            surface.dirty = true;
        }
    }
}
impl ShmHandler for Host {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl ProvidesRegistryState for Host {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
// These protocol objects have no events relevant to the overlay.
struct Ignore;
impl<I: Proxy> Dispatch2<I, Host> for Ignore {
    fn event(&self, _: &mut Host, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<Host>) {}
}
delegate_dispatch2!(Host);
delegate_registry!(Host);
