//! Wayland side: one overlay layer that shows the latest drawing.
//!
//! A stdin thread forwards request lines to a calloop event loop that also dispatches
//! Wayland events. A request is answered after the compositor presents a frame painted
//! from it, so the tool returns only once the drawing is on screen.
//!
//! An animated drawing paints its next frame each time the compositor presents one, so it
//! moves at the monitor's frame rate, until a timeline that plays once ends.
//!
//! Clicks pass through everywhere except the frames of scenes and 3D charts, both called
//! scenes here. Dragging a scene with the left button orbits its camera like a turntable:
//! sideways turns the yaw, and up and down turns the pitch, which stops straight above or
//! below so the scene never rolls over. Dragging takes the camera over from the scene's own
//! keys. Each move repaints at the compositor's frame rate. Scrolling over a scene zooms it:
//! up zooms in.

use std::ffi::c_void;
use std::io::{BufRead, Read, Write};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use super::gpu::Target;
use super::paint::{Painter, View};
use crate::implementations::grok_build::screen_draw::{
    Camera, OverlayRequest, Shape, animation::seconds,
};
use anyhow::{Context, Result, anyhow};
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
            protocol::{wl_output, wl_pointer, wl_seat, wl_surface},
        },
        protocols::wp::{
            cursor_shape::v1::client::wp_cursor_shape_device_v1::{
                Shape as Cursor, WpCursorShapeDeviceV1,
            },
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
        pointer::{
            PointerEvent, PointerEventKind, PointerHandler, cursor_shape::CursorShapeManager,
        },
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};

const MAX_REQUEST: u64 = 1024 * 1024;
const NAMESPACE: &str = "grok-screen-draw";
/// Linux's code for the left mouse button.
const LEFT_BUTTON: u32 = 0x110;
/// How far the camera turns per logical unit of dragging: across 360 units is half a turn.
const DRAG_DEGREES: f64 = 0.5;
/// How much one logical unit of scrolling zooms: a wheel notch of 15 units is about 16%.
const ZOOM_PER_UNIT: f32 = 0.01;
const ZOOMS: std::ops::RangeInclusive<f32> = 0.2..=10.0;

enum Input {
    Line(Vec<u8>),
    Failed(String),
    End,
}

pub(super) fn run() -> Result<()> {
    let connection = Connection::connect_to_env().context("Cannot connect to Wayland")?;
    let (globals, mut queue) = registry_queue_init::<Overlay>(&connection)?;
    let qh = queue.handle();
    // Made before the overlay so that it, and the connection it takes, outlive the overlay's
    // surfaces: dropping a GPU surface after its display has disconnected crashes.
    let mut event_loop = EventLoop::<Overlay>::try_new()?;
    let mut overlay = Overlay {
        display: NonNull::new(connection.backend().display_ptr().cast())
            .context("Wayland gave no display handle")?,
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh)?,
        shell: LayerShell::bind(&globals, &qh)
            .context("The compositor does not support layer-shell overlays")?,
        fractional: globals.bind(&qh, 1..=1, Ignore).ok(),
        viewporter: globals.bind(&qh, 1..=1, Ignore).ok(),
        cursors: CursorShapeManager::bind(&globals, &qh).ok(),
        pointer: None,
        painter: Painter::new(),
        drawing: None,
        error: None,
        exit: false,
    };
    // Learn output names before the first request asks for one.
    queue.roundtrip(&mut overlay)?;
    WaylandSource::new(connection, queue).insert(event_loop.handle())?;
    let (sender, receiver) = channel::sync_channel(4);
    std::thread::Builder::new()
        .name("draw-requests".to_owned())
        .spawn(move || {
            let mut reader = std::io::stdin().lock();
            loop {
                let input = read_request(&mut reader);
                let done = !matches!(input, Input::Line(_));
                if sender.send(input).is_err() || done {
                    break;
                }
            }
        })?;
    let request_qh = qh.clone();
    event_loop
        .handle()
        .insert_source(receiver, move |event, _, overlay| match event {
            channel::Event::Msg(Input::Line(line)) => {
                if let Err(error) = overlay.request(&line, &request_qh) {
                    overlay.error = Some(error);
                }
            }
            channel::Event::Msg(Input::Failed(error)) => overlay.error = Some(anyhow!(error)),
            channel::Event::Msg(Input::End) | channel::Event::Closed => overlay.exit = true,
        })
        .map_err(|error| anyhow!("Cannot watch drawing requests: {}", error.error))?;
    while !overlay.exit {
        event_loop.dispatch(Duration::from_millis(100), &mut overlay)?;
        if let Some(error) = overlay.error.take() {
            return Err(error);
        }
        overlay.paint(&qh)?;
    }
    Ok(())
}

fn read_request(reader: &mut impl BufRead) -> Input {
    let mut line = Vec::new();
    match reader
        .by_ref()
        .take(MAX_REQUEST + 1)
        .read_until(b'\n', &mut line)
    {
        Ok(0) => Input::End,
        Ok(_) if line.len() as u64 > MAX_REQUEST => {
            Input::Failed("Drawing request exceeds 1 MiB".to_owned())
        }
        Ok(_) if line.last() != Some(&b'\n') => {
            Input::Failed("Incomplete drawing request".to_owned())
        }
        Ok(_) => Input::Line(line),
        Err(error) => Input::Failed(format!("Cannot read drawing request: {error}")),
    }
}

/// Answer the waiting request: `{}` when the drawing shows, otherwise the reason it cannot.
fn reply(error: Option<&str>) -> Result<()> {
    let mut output = std::io::stdout().lock();
    match error {
        Some(error) => serde_json::to_writer(&mut output, &json!({ "error": error }))?,
        None => output.write_all(b"{}")?,
    }
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

/// The visible drawing: a full-screen overlay layer on one output.
struct Drawing {
    /// Dropped first, while its surface still exists.
    target: Target,
    layer: LayerSurface,
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    viewport: Option<wp_viewport::WpViewport>,
    output: wl_output::WlOutput,
    request: OverlayRequest,
    /// The shapes as last painted, at their current point in the timeline.
    shown: Vec<Shape>,
    /// When the timeline started, and its length in seconds.
    start: Instant,
    seconds: f64,
    /// The last paint showed the final frame, so nothing moves until something changes.
    settled: bool,
    /// Each scene's view, which dragging and scrolling change, in scene order; reset by
    /// every request.
    views: Vec<View>,
    /// The scene being dragged and the pointer's last position.
    drag: Option<(usize, (f64, f64))>,
    /// Logical size from the compositor's configure event.
    size: (u32, u32),
    /// Output pixels per logical unit.
    scale: f64,
    configured: bool,
    /// The request changed since the last paint.
    dirty: bool,
    /// A painted frame awaits presentation.
    in_flight: bool,
    /// The tool still waits for this request's answer.
    unanswered: bool,
}

impl Drawing {
    /// Show the request from the start of its timeline.
    fn restart(&mut self) {
        for shape in &mut self.request.shapes {
            if let Shape::Chart(chart) = shape {
                chart.pin_axes();
            }
        }
        let shapes = &self.request.shapes;
        self.views = views(shapes);
        self.shown = shapes.clone();
        self.start = Instant::now();
        self.seconds = seconds(shapes);
        self.settled = false;
        self.drag = None;
        self.dirty = true;
        self.unanswered = true;
    }

    /// Each scene's frame, as (x, y, width, height), and camera, as last painted.
    fn scenes(&self) -> impl Iterator<Item = ((f64, f64, f64, f64), Camera)> + '_ {
        self.shown.iter().filter_map(Shape::orbit)
    }

    /// The topmost scene under `(x, y)`, by its place among the scenes.
    fn scene_at(&self, (x, y): (f64, f64)) -> Option<usize> {
        self.scenes()
            .enumerate()
            .filter(|&(_, ((left, top, width, height), _))| {
                (left..=left + width).contains(&x) && (top..=top + height).contains(&y)
            })
            .map(|(index, _)| index)
            .last()
    }

    /// Orbit the dragged scene's camera by the pointer's move to `to`. The scene follows the
    /// pointer: dragging right turns its front to the right, and dragging down tips its top
    /// toward the user.
    fn turn(&mut self, to: (f64, f64)) {
        let Some((scene, from)) = self.drag else {
            return;
        };
        let shown = self.scenes().nth(scene).map(|(_, camera)| camera);
        if let (Some(view), Some(shown)) = (self.views.get_mut(scene), shown) {
            let camera = view.camera.get_or_insert(shown);
            camera.yaw = (camera.yaw - (to.0 - from.0) * DRAG_DEGREES).rem_euclid(360.0);
            camera.pitch = (camera.pitch + (to.1 - from.1) * DRAG_DEGREES).clamp(-90.0, 90.0);
        }
        self.drag = Some((scene, to));
        self.dirty = true;
    }

    /// Zoom the scene under `at` by a scroll of `amount` logical units; scrolling up,
    /// a negative amount, zooms in.
    fn zoom(&mut self, at: (f64, f64), amount: f64) {
        let Some(view) = self
            .scene_at(at)
            .and_then(|scene| self.views.get_mut(scene))
        else {
            return;
        };
        let zoom = view.zoom * (-amount as f32 * ZOOM_PER_UNIT).exp();
        view.zoom = zoom.clamp(*ZOOMS.start(), *ZOOMS.end());
        self.dirty = true;
    }
}

/// Each scene's view in `shapes` before the user orbits or zooms, in order.
fn views(shapes: &[Shape]) -> Vec<View> {
    shapes
        .iter()
        .filter_map(|shape| match shape {
            Shape::Scene { objects, .. } => Some(View::new(objects, &shape.times())),
            _ => shape.orbit().map(|_| View::new(&[], &[])),
        })
        .collect()
}

impl Drop for Drawing {
    fn drop(&mut self) {
        if let Some(fractional) = &self.fractional {
            fractional.destroy();
        }
        if let Some(viewport) = &self.viewport {
            viewport.destroy();
        }
    }
}

struct Overlay {
    /// The Wayland connection's display, which outlives every drawing.
    display: NonNull<c_void>,
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    compositor: CompositorState,
    shell: LayerShell,
    fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    cursors: Option<CursorShapeManager>,
    pointer: Option<Pointer>,
    painter: Painter,
    drawing: Option<Drawing>,
    error: Option<anyhow::Error>,
    exit: bool,
}

/// The mouse, and the device that sets its cursor shape when the compositor supports it.
struct Pointer {
    pointer: wl_pointer::WlPointer,
    shape: Option<WpCursorShapeDeviceV1>,
    /// Cursor shapes must name the latest enter event.
    entered: u32,
}

impl Pointer {
    fn show(&self, cursor: Cursor) {
        if let Some(shape) = &self.shape {
            shape.set_shape(self.entered, cursor);
        }
    }
}

impl Overlay {
    fn request(&mut self, line: &[u8], qh: &QueueHandle<Self>) -> Result<()> {
        let request: OverlayRequest = match serde_json::from_slice(line) {
            Ok(request) => request,
            Err(error) => return reply(Some(&format!("Invalid drawing request: {error}"))),
        };
        let named = |output: &wl_output::WlOutput| {
            self.outputs
                .info(output)
                .is_some_and(|info| info.name.as_deref() == Some(request.monitor.as_str()))
        };
        let Some(output) = self.outputs.outputs().find(named) else {
            return reply(Some(&format!("Unknown monitor {}", request.monitor)));
        };
        if let Some(drawing) = self.drawing.as_mut().filter(|d| d.output == output) {
            drawing.request = request;
            drawing.restart();
            return catch_scenes(&self.compositor, drawing);
        }
        // The drawing moves to another monitor.
        self.drawing = None;
        let surface = self.compositor.create_surface(qh);
        let layer = self.shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some(NAMESPACE),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let (fractional, viewport) = match (&self.fractional, &self.viewporter) {
            (Some(manager), Some(viewporter)) => (
                Some(manager.get_fractional_scale(layer.wl_surface(), qh, FractionalData)),
                Some(viewporter.get_viewport(layer.wl_surface(), qh, Ignore)),
            ),
            _ => (None, None),
        };
        let scale = self
            .outputs
            .info(&output)
            .map_or(1, |info| info.scale_factor);
        let window = NonNull::new(layer.wl_surface().id().as_ptr().cast())
            .context("The overlay surface has no handle")?;
        // SAFETY: the display outlives every drawing, and a drawing drops its target before
        // its surface.
        let target = match unsafe { self.painter.target(self.display, window) } {
            Ok(target) => target,
            Err(error) => return reply(Some(&format!("{error:#}"))),
        };
        let mut drawing = Drawing {
            target,
            layer,
            fractional,
            viewport,
            output,
            request,
            shown: Vec::new(),
            start: Instant::now(),
            seconds: 0.0,
            settled: false,
            views: Vec::new(),
            drag: None,
            size: (0, 0),
            scale: f64::from(scale.max(1)),
            configured: false,
            dirty: true,
            in_flight: false,
            unanswered: true,
        };
        drawing.restart();
        catch_scenes(&self.compositor, &drawing)?;
        drawing.layer.commit();
        self.drawing = Some(drawing);
        Ok(())
    }

    /// Remove the drawing. A request still waiting learns why.
    fn dismiss(&mut self, reason: &str) {
        if let Some(drawing) = self.drawing.take()
            && drawing.unanswered
            && let Err(error) = reply(Some(reason))
        {
            self.error = Some(error);
        }
    }

    fn paint(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let Some(d) = (self.drawing.as_mut()).filter(|d| d.configured && d.dirty && !d.in_flight)
        else {
            return Ok(());
        };
        let elapsed = d.start.elapsed().as_secs_f64();
        let (t, settled) = d.request.repeat.time(elapsed, d.seconds);
        if d.seconds > 0.0 {
            let style = &d.request.style;
            d.shown = d.request.shapes.iter().map(|s| s.at(t, style)).collect();
            catch_scenes(&self.compositor, d)?;
        }
        d.settled = settled;
        // Surface state that the GPU's present commits with the frame.
        let surface = d.layer.wl_surface();
        if let Some(viewport) = &d.viewport {
            surface.set_buffer_scale(1);
            viewport.set_destination(d.size.0 as i32, d.size.1 as i32);
        } else {
            surface.set_buffer_scale(d.scale as i32);
        }
        surface.frame(qh, FrameCallbackData(surface.clone()));
        let (style, shapes) = (&d.request.style, &d.shown);
        match (self.painter).paint(&mut d.target, style, shapes, &d.views, d.size, d.scale) {
            Ok(shown) => {
                d.dirty = !shown;
                d.in_flight = shown;
            }
            Err(error) => self.dismiss(&format!("{error:#}")),
        }
        Ok(())
    }

    fn drawing_on(&mut self, surface: &wl_surface::WlSurface) -> Option<&mut Drawing> {
        self.drawing
            .as_mut()
            .filter(|drawing| drawing.layer.wl_surface() == surface)
    }
}

/// Let the mouse reach the drawing only over scene frames, so they can be dragged; clicks
/// and scrolling elsewhere reach the windows below. Applies on the next commit.
fn catch_scenes(compositor: &CompositorState, drawing: &Drawing) -> Result<()> {
    let region = Region::new(compositor)?;
    for ((x, y, width, height), _) in drawing.scenes() {
        let (left, top) = (x.floor(), y.floor());
        let (right, bottom) = ((x + width).ceil(), (y + height).ceil());
        region.add(
            left as i32,
            top as i32,
            (right - left) as i32,
            (bottom - top) as i32,
        );
    }
    drawing
        .layer
        .wl_surface()
        .set_input_region(Some(region.wl_region()));
    Ok(())
}

impl CompositorHandler for Overlay {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        scale: i32,
    ) {
        // Fractional scaling, when present, reports the exact scale instead.
        if let Some(drawing) = self.drawing_on(surface)
            && drawing.fractional.is_none()
        {
            drawing.scale = f64::from(scale.max(1));
            drawing.dirty = true;
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

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        let Some(drawing) = self.drawing_on(surface) else {
            return;
        };
        drawing.in_flight = false;
        // A dirty drawing changed after this frame was painted; answer after the next one.
        let answer = drawing.unanswered && !drawing.dirty;
        if answer {
            drawing.unanswered = false;
        }
        if !drawing.settled {
            drawing.dirty = true;
        }
        if answer && let Err(error) = reply(None) {
            self.error = Some(error);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
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

impl OutputHandler for Overlay {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.drawing.as_ref().is_some_and(|d| d.output == output) {
            self.dismiss("The monitor was disconnected");
        }
    }
}

impl LayerShellHandler for Overlay {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.drawing.as_ref().is_some_and(|d| d.layer == *layer) {
            self.dismiss("The compositor closed the drawing");
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
        let Some(drawing) = self.drawing.as_mut().filter(|d| d.layer == *layer) else {
            return;
        };
        let (width, height) = configure.new_size;
        if width == 0 || height == 0 {
            self.dismiss("The compositor gave the drawing no size");
            return;
        }
        drawing.size = (width, height);
        drawing.configured = true;
        drawing.dirty = true;
    }
}

impl SeatHandler for Overlay {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Pointer || self.pointer.is_some() {
            return;
        }
        // Without a pointer, scenes simply stay as drawn.
        let Ok(pointer) = self.seats.get_pointer(qh, &seat) else {
            return;
        };
        let shape = self
            .cursors
            .as_ref()
            .map(|cursors| cursors.get_shape_device(&pointer, qh));
        self.pointer = Some(Pointer {
            pointer,
            shape,
            entered: 0,
        });
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(Pointer { pointer, shape, .. }) = self.pointer.take()
        {
            if let Some(shape) = shape {
                shape.destroy();
            }
            pointer.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Overlay {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(drawing) = self
                .drawing
                .as_mut()
                .filter(|drawing| drawing.layer.wl_surface() == &event.surface)
            else {
                continue;
            };
            let pointer = self.pointer.as_mut();
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(pointer) = pointer {
                        pointer.entered = serial;
                        pointer.show(Cursor::Grab);
                    }
                }
                PointerEventKind::Press {
                    button: LEFT_BUTTON,
                    ..
                } => {
                    drawing.drag = drawing
                        .scene_at(event.position)
                        .map(|scene| (scene, event.position));
                    if let Some(pointer) = pointer.filter(|_| drawing.drag.is_some()) {
                        pointer.show(Cursor::Grabbing);
                    }
                }
                PointerEventKind::Motion { .. } => drawing.turn(event.position),
                PointerEventKind::Axis { vertical, .. } => {
                    drawing.zoom(event.position, vertical.absolute);
                }
                PointerEventKind::Release {
                    button: LEFT_BUTTON,
                    ..
                } => {
                    drawing.drag = None;
                    if let Some(pointer) = pointer {
                        pointer.show(Cursor::Grab);
                    }
                }
                _ => {}
            }
        }
    }
}

impl ProvidesRegistryState for Overlay {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

/// Receives the exact output scale, such as 1.25 or 2, for the drawing's surface.
struct FractionalData;

impl Dispatch2<wp_fractional_scale_v1::WpFractionalScaleV1, Overlay> for FractionalData {
    fn event(
        &self,
        overlay: &mut Overlay,
        fractional: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<Overlay>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(drawing) = overlay
                .drawing
                .as_mut()
                .filter(|d| d.fractional.as_ref() == Some(fractional))
        {
            drawing.scale = f64::from(scale) / 120.0;
            drawing.dirty = true;
        }
    }
}

/// Protocol objects whose events the overlay does not need.
struct Ignore;

impl<I: Proxy> Dispatch2<I, Overlay> for Ignore {
    fn event(&self, _: &mut Overlay, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<Overlay>) {
    }
}

delegate_dispatch2!(Overlay);
delegate_registry!(Overlay);
