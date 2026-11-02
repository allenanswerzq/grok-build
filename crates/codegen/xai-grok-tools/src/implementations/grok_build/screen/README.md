# Screen reading and desktop overlays

`read_screen` observes the active desktop display. `draw_overlay` displays an independent
desktop overlay. Both use Grok Build's existing tool registry, typed inputs,
permissions, and model/TUI output handling. Both are enabled in Grok Build's
default toolset, including its plan-mode variants. Explicit tool IDs are
`GrokBuild:read_screen` and `GrokBuild:draw_overlay`.

Build and launch the normal TUI from the repository root:

```sh
just run
```

## Read the active screen

Call `read_screen` with an empty argument object:

```json
{}
```

There are no application, page target, or display arguments. On Hyprland the
reader selects the active powered-on output and captures its actual pixels with
`grim`. The image includes the desktop, browser controls, native dialogs, and
other visible applications. Hyprland supplies visible window context; Chrome
accessibility is attached automatically for its visible selected pages through
managed CDP on `http://127.0.0.1:9222`. DOM keyboard focus is not used to choose a
page, since Grok's terminal commonly has focus during a read.

Chrome disconnection or ambiguous internal page mapping reduces accessibility
coverage; it does not prevent the screen screenshot or ask the agent to choose a
target ID. Accessibility can include controls occluded by another window, so the
screenshot is authoritative. The output reports display scale and logical
viewport dimensions. Chrome accessibility bounds remain page-local CSS
coordinates and are explicitly labeled; they are not silently treated as screen
coordinates. Only the active output is captured.

Display capture requires `hyprctl` and `grim`. Each native command is bounded in
time and output size and is killed on cancellation. PNGs are bounded to 16 MiB.
Visible window context is bounded to eight windows; combined Chrome semantics
are bounded to 1,500 rows and 60 KB. Accessibility has a separate five-second
budget; capture and monitor checks have an overall 20-second timeout. A display
or workspace change during capture is reported for a fresh read.

The internal Chrome provider also exposes `ChromeApplication::read_page` for
page-level capture validation; these target IDs are not tool arguments.

AX, layout, and screenshot are sampled separately. A DOM/input/focus revision,
scroll, document identity, and viewport check detects some capture races;
animation, video, and compositor-only changes cannot be made atomic. The tree is
bounded to 1,500 semantic rows and 60 KB, with a separate 20,000-node traversal
limit. Empty generic wrappers and duplicate glyph boxes are omitted; visible
branches come first, with semantic ancestors and unmapped content retained.
Known off-screen controls are omitted unless needed as ancestors. Main-document
control bounds use CSS viewport coordinates. The snapshot's root rectangle is
matched against its fractional DOM rectangle to establish scale before
subtracting scroll; this also handles browser zoom and CDP emulation. When that
reference is unavailable or inconsistent, bounds are omitted. Capture operations time out after
20 seconds; CDP responses are limited to 16 MiB.

`ScreenReader::read_screen` is the argument-free reader boundary. Insert a
`ScreenBackend` into session Resources to replace the desktop observation provider.
It performs no desktop rendering.

## Draw independently on the desktop

The Linux backend requires a layer-shell-capable Wayland compositor, tested on
Hyprland. The Rust `grok-overlay-host` uses Smithay Client Toolkit to own a
transparent overlay surface. `tiny-skia` draws annotations, `cosmic-text` shapes
literal text with system fonts, and `wgpu` renders the five built-in 3D meshes
through Vulkan. Rust owns the process and communicates over private stdin/stdout
pipes. No Chrome instance, CDP endpoint, tab, or observation ID is required.
No machine configuration is modified.

`just build` builds both the CLI and native host with kache. To rebuild only the
native host:

```sh
just overlay
```

Building the overlay needs Rust, Wayland development libraries, pkg-config, and
libxkbcommon. Runtime needs a Wayland session and system fonts; 3D additionally
needs a Vulkan driver. All dependencies are locked in `Cargo.lock`; there is no
Qt, C++ host, npm build, embedded web view, or downloaded renderer bundle.
The binary lands in `target/debug/grok-overlay-host`. The backend locates it
beside its executable, through PATH, or from `GROK_OVERLAY_HOST`.
The native dependencies are behind `native-overlay`; normal CLI and headless
provider tests do not require that feature.

```json
{"action":"show","output":"eDP-1","scene":{"kind":"annotations","items":[{"kind":"rectangle","x":100,"y":100,"width":300,"height":160,"color":"#ff3366"},{"kind":"text","x":100,"y":80,"text":"Look here","color":"#ffffff"}]}}
```

```json
{"action":"show","scene":{"kind":"model3d","parts":[{"primitive":"sphere","position":[0,0,0],"rotation":[0,0,0],"scale":[2,2,2],"color":"#88aaff"},{"primitive":"torus","position":[0,0,0],"rotation":[1.3,0,0],"scale":[3,3,3],"color":"#ff6633"}]}}
```

```json
{"action":"clear"}
```

`output` names a desktop monitor, independently of application targets. Omit it
to retain a visible overlay's monitor. When opening a surface, the compositor
chooses its active output. Annotation coordinates are logical units
from that output's upper-left corner; the host handles integer
and fractional display scale using Wayland's fractional-scale/viewporter protocols,
with integer buffer scaling as a fallback. A Chrome
page's viewport coordinates alone do not locate it on the desktop. Precise
annotations of application controls will require a native window observation and
coordinate mapping in a subsequent provider.

3D supports box, sphere, cylinder, cone, and torus parts, with scene-unit positions
and scales and XYZ rotations in radians. Objects render over the desktop with a
transparent background and no viewer frame. Drag near the objects to orbit,
wheel to zoom, or right-drag to pan. Reset restores the initial camera and brings
off-screen objects back; Reset and Close remain reachable. Escape dismisses when
the overlay receives keyboard focus; the host
does not take focus automatically or install a global Escape binding.

One overlay is owned per agent session. Show replaces it; Clear only affects that
session's overlay. An unknown output or other rejected request preserves the
existing scene; transport and rendering failures terminate the host. Background
clicks pass through except at the Close/Reset buttons and
the projected bounding rectangle around 3D objects. When the model bounds cross
the camera's near plane, interaction covers the output. Resizing dismisses the
current scene. Cancellation, protocol failure,
and session teardown terminate the whole helper process group. An injected `ProcessScope` in session Resources also
allows a supervisor to terminate a retained host. Monitor removal dismisses the surface.
Operations time out after 20 seconds. Scenes contain 1–256 validated items and
requests are bounded to 1 MiB. No model-supplied code, HTML, URLs, shaders, or
external assets run. Text is shaped as literal Unicode rather than interpreted as
markup. Rendering does not make network requests.

The host renders only when the scene, scale, or interaction changes. 3D frames
are rendered offscreen on the GPU, read back, and presented in shared-memory
Wayland buffers alongside raster text. This keeps the presentation path in safe
Rust and works without GPU window-handle interop, at the cost of a readback and
copy per changed frame. Raster allocation is capped at 128 MiB per frame and
16,384 pixels per dimension; 3D also obeys the graphics device's texture limit.

`OverlayRenderer::draw_overlay` is the desktop backend boundary. Insert an
`OverlayBackend` into session Resources to replace the Linux implementation.
It has no dependency on the application reader registry. Neither tool clicks or
types into other applications.

## Manual experiments and tests

Keep the desktop unlocked and the display powered on during native and
managed-Chrome live tests. On this Hyprland session, DPMS-off stopped subsequent
Chrome screenshots from completing. The agent test checks display power and,
when available, Omarchy's lock state before starting so this prerequisite fails
clearly. Tests do not change idle or display settings.

From the repository root:

```sh
just inspect-screen /tmp/screen.png
just inspect-overlay /tmp/scene.json
just inspect-overlay /tmp/scene.json eDP-1
```

`scene.json` contains only the `scene` object from an example above. The overlay
example keeps its session alive; Enter clears and exits. These development
examples call tools directly; agent sessions use the normal permission pipeline.

```sh
just test-screen
just test-renderer
just test-native
just test-live
```

Normal tests cover independent provider routing, argument validation, permissions,
vision output, bounded process protocol, host reuse, teardown, cancellation, and
supervisor shutdown.
They use synthetic local processes and need no desktop. `just test-renderer`
checks logical geometry, scaled raster pixels, multilingual literal text,
premultiplied transparency, bounded allocations, and pointer gesture rules.
`just test-native` additionally renders every primitive with real Vulkan,
checks camera changes against actual pixels, and exercises a real Wayland layer
with virtual-pointer orbit, zoom, pan, Reset, Close, and reopening. Button presses
target the test's own overlay and the original cursor position is restored.
Chrome tests create and dispose private browser contexts, covering
the real tool at integer/fractional scales and scroll positions, and watcher
upgrades/root replacement. The native Rust live
test verifies the real tool-to-host-to-compositor path without Chrome.
The agent test runs the built CLI's default agent with mock inference and an
isolated home, without a custom agent file or tool-enabling arguments. It
verifies two argument-free display reads deliver distinct observations,
visible Chrome AX text and a screen PNG to inference, then show and clear a native overlay. Tool errors
fail the test even when mock inference returns a final response. No real model
credentials are used.

Porting decisions and old-to-new test mappings are in [native/PORTING.md](native/PORTING.md).

Protocol references: [Chrome Accessibility](https://chromedevtools.github.io/devtools-protocol/tot/Accessibility/),
[Page](https://chromedevtools.github.io/devtools-protocol/tot/Page/),
[DOMSnapshot](https://chromedevtools.github.io/devtools-protocol/tot/DOMSnapshot/),
[Wayland layer-shell](https://wayland.app/protocols/wlr-layer-shell-unstable-v1),
[Smithay Client Toolkit](https://docs.rs/smithay-client-toolkit/0.21.1/smithay_client_toolkit/),
and [wgpu](https://docs.rs/wgpu/30.0.1/wgpu/).
