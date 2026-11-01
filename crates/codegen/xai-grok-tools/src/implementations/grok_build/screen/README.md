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
Hyprland. It creates its own transparent Wayland layer surface through
LayerShellQt. A small Qt Quick/WebEngine host runs our trusted bundled SVG/Three.js
renderer. Rust validates arguments, owns the child process, and communicates over
private stdin/stdout pipes. No Chrome instance, CDP endpoint, tab, or observation
ID is required. No machine configuration is modified.

`just build` builds both the CLI and native host. To rebuild only the native host
from the repository root:

```sh
just overlay
```

Dependencies are Qt 6.9 or newer with Quick, WebEngineQuick, and Wayland, plus
LayerShellQt, qmake6, make, and a C++ compiler. This machine already has them.
The recipes use kache for Rust and native compilation.
The binary lands in `target/debug/grok-overlay-host`. The Rust backend locates it
beside its executable, through PATH, or from `GROK_OVERLAY_HOST`.
Cargo builds stay independent of Qt so headless tool tests need no desktop packages.

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
from that output's upper-left corner; display scale is handled by Qt. A Chrome
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
and session teardown terminate the whole helper process group, including its
WebEngine subprocesses. An injected `ProcessScope` in session Resources also
allows a supervisor to terminate a retained host. Monitor removal dismisses the surface.
Operations time out after 20 seconds. Scenes contain 1–256 validated items and
requests are bounded to 1 MiB. No model-supplied code, HTML, URLs, shaders, or
external assets run; text uses `textContent`. The web view uses a private profile
and a restrictive CSP, with remote requests and navigation disabled.

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
They use synthetic local processes and need no Chrome or Qt. `grok-overlay-test`
is an opt-in native GUI test using only its own surface: native pixels,
transparency, input regions, all primitives, orbit, zoom, pan, reset,
and dismissal. Chrome tests create and dispose private browser contexts, covering
the real tool at integer/fractional scales and scroll positions, and watcher
upgrades/root replacement. The native Rust live
test verifies the real tool-to-host-to-compositor path without Chrome.
The agent test runs the built CLI's default agent with mock inference and an
isolated home, without a custom agent file or tool-enabling arguments. It
verifies two argument-free display reads deliver distinct observations,
visible Chrome AX text and a screen PNG to inference, then show and clear a native overlay. Tool errors
fail the test even when mock inference returns a final response. No real model
credentials are used.

`renderer.bundle.js` is committed; runtime use needs no npm or network access.
It bundles Three.js 0.185.0 and OrbitControls using esbuild 0.25.12. The upstream
MIT license is retained in `THREE-LICENSE` and bundle comments. Rebuild it from
the repository root with `just renderer`, then use `just overlay` to embed the
changed resource. Renderer recipes install the pinned npm dependencies with lifecycle
scripts disabled. `just test-renderer` runs renderer tests with real Three geometry and OrbitControls and a
GPU/DOM fixture, including off-screen pan recovery and scene cleanup.

Protocol references: [Chrome Accessibility](https://chromedevtools.github.io/devtools-protocol/tot/Accessibility/),
[Page](https://chromedevtools.github.io/devtools-protocol/tot/Page/),
[DOMSnapshot](https://chromedevtools.github.io/devtools-protocol/tot/DOMSnapshot/),
[Wayland layer-shell](https://wayland.app/protocols/wlr-layer-shell-unstable-v1),
[Qt WebEngineView](https://doc.qt.io/qt-6/qml-qtwebengine-webengineview.html), and
[Three.js OrbitControls](https://threejs.org/docs/pages/OrbitControls.html).
