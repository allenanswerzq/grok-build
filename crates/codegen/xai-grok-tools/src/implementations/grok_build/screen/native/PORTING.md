# Native Rust overlay port

The public `draw_overlay` arguments and session-owned helper protocol are unchanged.
`read_screen` remains independent and argument-free.

## Implementation mapping

| Previous implementation | Rust implementation |
| --- | --- |
| Qt `Backend::request`, private JSON pipes | `host.rs`: bounded command channel, typed scene validation, newline responses |
| Qt Quick / LayerShellQt window | `host.rs`: Smithay layer surface, monitor lifecycle, fractional scaling, SHM buffers |
| SVG rectangles, arrows, literal text | `render.rs`: tiny-skia paths and cosmic-text shaping with system font fallback |
| Three.js geometries and materials | `scene.rs`: five mesh generators; `render.rs` + `mesh.wgsl`: depth-tested Vulkan rendering |
| OrbitControls and projected input bounds | `scene.rs`: camera, gesture state, near-plane fallback, logical hit regions |
| Qt close/reset/input mask | `host.rs` + `scene.rs`: on-demand focus and Wayland input regions, fixed press-to-release gesture |
| qmake, QML resources, npm bundle | Cargo `native-overlay` binary target; `just build` / `just overlay` with kache |

## Test mapping

| Previous test | Rust coverage |
| --- | --- |
| JS logical bounds / fractional scale | `model_starts_centered_and_input_bounds_use_logical_units`, `annotations_are_transparent_scaled_and_literal` |
| JS recovery after off-screen pan | `reset_recovers_camera_and_bounds_after_offscreen_pan` |
| JS scene replacement cleanup | `vulkan_renders_every_primitive_with_transparency_and_camera_controls`, real host replacement test |
| Qt annotation pixels / transparency | scaled raster test plus `real_surface_handles_annotations_orbit_zoom_pan_reset_and_close` |
| Qt primitive pixels / orbit / zoom / pan / Reset | real Vulkan pixel test plus real Wayland virtual-pointer test |
| Qt Close / reopen / Clear | real Wayland test; existing `overlay_desktop_live` tool-to-compositor test |
| Existing Rust ownership / protocol / cancellation tests | retained unchanged in `screen::desktop::tests` |

Additional deterministic cases cover matching-button release, dragging across toolbar
buttons, literal multilingual text, premultiplied colors, bounded allocation, and
very small valid mesh scales. Translation regressions cover all five primitives and
relative placement; a real Vulkan test checks identical pixels for a small mesh at
the origin and at large positive/negative positions. Narrow-output tests check every
button's corners. Capture fixtures verify timeout cleanup both with open pipes and
with closed pipes while the child remains alive.

## Deliberate differences

- A single Rust helper replaces the embedded browser and its renderer subprocesses.
  Session cancellation still terminates the whole process group.
- Meshes use simple directional/ambient lighting rather than Three.js PBR materials.
  Camera gestures preserve orbit/zoom/pan behavior, with native wheel sensitivity.
- The GPU renders offscreen with four-sample antialiasing. Frames are read back and
  copied into Wayland shared-memory buffers, with correct premultiplied sRGB edges.
  This avoids unsafe native-window interop but has a per-frame readback/copy cost.
  There is no idle animation or continuous repaint loop.
- Raster buffers use the compositor's full integer/fractional scale, rather than
  capping 3D rendering at 2×. Allocation is bounded to 128 MiB / 16,384 pixels per
  dimension; 3D also obeys the Vulkan device's texture limit. Errors are explicit.
- Fonts and glyph rasterization come from the system. Text stays literal, with a
  dark outline for readability, but does not promise identical browser typography.
- Extremely small scales that underflow GPU floats collapse naturally to sub-pixel
  geometry; their lighting normal remains finite.

## Preserved behavior

Coordinates and hit regions remain output-local logical units. An existing surface
retains its monitor when output is omitted. Unknown outputs and invalid requests
are rejected before changing the current scene. Annotation backgrounds pass clicks
through. 3D input follows projected bounds, covers the output when those bounds cross
the near plane, and freezes during a drag. Reset remains reachable after off-screen
pan. Resize and monitor removal dismiss the scene. The host does not steal focus;
Escape only acts after it receives keyboard focus. Model input cannot provide code,
HTML, shaders, remote assets, or URLs.

Mesh transforms and bounds are computed in f64 relative to a shared scene origin;
the camera and GPU vertices use centered local coordinates. Reset stacks below Close
when they cannot fit side by side. Live-test capture commands have deadlines around
pipe reads and process exit, bounded output, and process-group cleanup on failure.

## Verification

Use `just test-screen`, `just test-renderer`, `just lint-screen`, `just lint-native`,
`just test-native`, and `just test-live`. The native/live checks require an unlocked
Hyprland session and powered-on display; Vulkan tests require a working driver.

Verified on Hyprland with a 2560×1600 output at 2× scale:

- `just build` completed the normal CLI and Rust host with kache (also run by
  `just test-live`).
- `just test-screen`: 24 tests passed.
- `just test-native`: 12 deterministic/Vulkan tests and three command/Wayland
  integration tests passed, including translated small meshes, narrow toolbars,
  capture deadlines, Close, reopen, Clear, and empty-output selection.
- `just test-live`: the native checks above plus four tool/Chrome/default-agent
  tests passed. The agent used mock inference with the normal built CLI and
  default tools; this does not validate responses from a live model service.
- `just lint-screen` and `just lint-native` passed with warnings denied.
- The helper's linked libraries contain no Qt or WebEngine dependencies.

The full-scale Vulkan test measured about 48 ms per 2560×1600 render/readback
frame in the unoptimized debug build on this machine. That excludes compositor
presentation and is not a 60 fps guarantee. Rendering remains event-driven.
