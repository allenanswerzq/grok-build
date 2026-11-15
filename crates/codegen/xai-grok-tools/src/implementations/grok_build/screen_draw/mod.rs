//! `screen_draw`: draw boxes, lines and arrows, text, small 3D scenes, and charts on the
//! screen.
//!
//! `xai-screen draw` does the whole job: it checks the drawing, finds the focused
//! monitor, reads the desktop's look, and shows the drawing above all windows. This tool passes
//! each input on as the model wrote it. The process lives only while a drawing is visible:
//! clearing, cancellation, a broken process, or the end of the session kills it, which removes
//! the drawing.

use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use xai_tool_protocol::{ToolCapabilities, ToolId, ToolScope};
use xai_tool_runtime::{ListToolsContext, Tool, ToolCallContext, ToolError, ToolErrorKind};
use xai_tool_types::ToolDescription;

use super::screen::{ScreenProcess, failure};
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::types::tool_metadata::{ToolMetadata, shared_resources};
use crate::util::ProcessScope;

/// Covers starting `xai-screen`, loading fonts, and the compositor showing the first frame.
const SHOW_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REPLY: u64 = 64 * 1024;

/// The complete drawing for the active monitor. `xai-screen` checks it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScreenDrawInput {
    /// Shapes that replace the current drawing; [] clears it. Each is an object whose kind is
    /// box, line, text, scene, or chart, with the fields the description lists.
    pub shapes: Vec<Map<String, Value>>,
    /// How shapes with keys play: once (the default), loop, or bounce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<String>,
}

/// What the screen shows after a call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScreenDrawOutput {
    Drawn {
        monitor: String,
        width: f64,
        height: f64,
        shapes: usize,
    },
    Cleared,
}

impl std::fmt::Display for ScreenDrawOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Drawn {
                monitor,
                width,
                height,
                shapes,
            } => write!(
                f,
                "Drew {shapes} {} on monitor {monitor} ({width} × {height} logical units). \
                 The drawing stays until the next screen_draw call or the end of the session.",
                if *shapes == 1 { "shape" } else { "shapes" }
            ),
            Self::Cleared => f.write_str("Cleared the drawing."),
        }
    }
}

impl xai_tool_runtime::ToolOutput for ScreenDrawOutput {}

impl From<ScreenDrawOutput> for ToolOutput {
    fn from(output: ScreenDrawOutput) -> Self {
        Self::Text(output.to_string().into())
    }
}

const DESCRIPTION: &str = r#"Draw on the user's screen to point at or explain something.

Screen:
- Shapes appear on the active monitor, above all windows.
- Clicks and typing pass through to the windows below.
- The user can drag a scene or 3D chart to orbit it, and scroll over it to zoom.
- Positions use screen_read's logical units: (x, y) from the monitor's top-left corner.
- To mark a point in a screen_read window screenshot, add the window's x and y.
- Each call replaces the previous drawing. Call with an empty list to clear it.
- The drawing also clears when the session ends.

Fields (? marks optional):
- box: x, y, width, height, label?, color?, fill?, border?, line_width?, opacity?, keys?
- line: x, y, to_x, to_y, color?, arrows? (none, start, end, both), pattern? (solid, dashed,
  dotted), line_width?, opacity?, keys?
- text: x, y, text, color?, size?, fill?, border?, opacity?, keys?
- scene: x, y, width, height, objects, label?, camera? {yaw?, pitch?}, background?, fill?,
  border?, line_width?, opacity?, keys?
- chart: x, y, width, height, plot, label?, categories?, rows?, series?, links?, x_axis?,
  y_axis?, z_axis?, stacked?, camera?, fill?, border?, line_width?, opacity?, keys?
- Scene objects, by kind: cuboid {center, size, rotation?}, sphere {center, radius},
  ellipsoid {center, size, rotation?}, cylinder {from, to, radius, rotation?}, tube {points,
  radius, radii?, pouches?, rotation?}, coil {center, size, radius, rotation?}, blob {balls,
  rotation?}, mesh {vertices, faces, rotation?}, line {from, to, rotation?}, label {point,
  text}; each also takes color? and keys?.
- Solid objects (all but line and label) also take material? (plastic, flesh, or glass).
- Points and sizes are [x, y, z]; a blob ball is [x, y, z, radius].
- A mesh face is [a, b, c], three indices into vertices.
- A series is {name?, color?, values? | points? | grid?}; a link is {from, to, value}; an axis
  is {label?, min?, max?}.
- A key is {at, ease?, ...fields it sets}; ease is linear, in_out, or step.
- color is a theme color (accent, red, yellow, green, or blue) or any #rrggbb hex color.
- Shapes and their objects reject fields they do not list.
- An invalid drawing fails with the shape and rule it breaks; fix it and call again.

Shapes:
- A line runs from (x, y) to (to_x, to_y).
- A line's arrows puts a head on its start, end, or both ends.
- A line's pattern makes it solid, dashed, or dotted.
- A scene is a 3D view of solid objects, lines, and triangle meshes; y points up.
- An ellipsoid is a sphere stretched to fill a box of size.
- A tube curves smoothly through 2 to 64 points and has rounded ends.
- A tube's radii gives each point its own radius, so the tube can swell and taper.
- A tube's pouches (1 to 64) bulges it evenly along its length, like a large intestine.
- A coil is one long tube tangled through a box of size, like a small intestine; it counts as
  128 tube points.
- A blob melts 1 to 48 balls into one smooth surface, for organic forms like organs or bodies.
- A label pins text to a 3D point with a dot and leader line; it follows the point as the
  scene turns and always shows in front.
- Meshes shade smoothly across gentle bends and keep creases sharp.
- plastic is smooth and shiny (the default), flesh is moist with fine bumps and mottling, and
  glass is a see-through shell with bright edges.
- background colors the scene's backdrop; a dark one such as #1d2b40 makes realistic colors
  stand out.
- For lifelike scenes, shape natural forms with blobs, tubes, and coils in realistic #rrggbb
  colors and flesh material; give tubes radii or pouches; and keep cuboids for built things.
- Tubes curve smoothly between their points, so 5 to 12 points describe most bends.
- To show anatomy, wrap organs in a glass blob of the body and name parts with labels.
- A scene's camera circles its objects by yaw and pitch in degrees and fits them in the frame.
- Boxes, scenes, and text panels take a fill opacity from 0 to 1 and an optional border.
- An outline with a faint fill highlights what is under a box; fill 1 covers it.
- A scene with fill 0 and no border floats its objects over the windows.
- Text without a border shows its words in its color.
- Boxes, lines, and scenes take a line_width, and text takes a size, both in logical units.
- The defaults follow the window border and a 14-unit font.
- Theme colors follow the user's theme. accent, the default, matches the focused window's
  border.

Animation:
- To animate a shape, give it keys. Each key has a time `at` in seconds and the values it sets.
- Shape keys can set x, y, width, height, to_x, to_y, color, fill, opacity, line_width, size,
  text, and a scene's camera yaw and pitch.
- Scene object keys can set center, size, radius, radii, points, balls, from, to, color, and
  rotation; label keys can set color.
- rotation turns an object in degrees about the x, y, then z axis around its center.
- A key's ease sets how fields move from their previous values: linear at constant speed (the
  default), in_out easing in and out, or step all at once when the key arrives.
- Opacity 0 hides a shape, so step keys on opacity make flipbook frames.
- The timeline ends at the latest key, at most 60 seconds.
- repeat plays the timeline once and keeps the last frame (once, the default), again from the
  start (loop), or forward and back (bounce).

Charts:
- A chart lays out its own axes, ticks, gridlines, legend, and colors in its frame.
- Its plot is bar, line, area, scatter, pie, donut, candlestick, sankey, heatmap, surface,
  scatter_3d, or bar_3d.
- A chart takes 1 to 8 series, except a sankey, which takes links instead.
- Bar, line, area, pie, and donut series give one value per category.
- Line, area, and scatter series may give [x, y] points instead; scatter_3d series give [x, y, z].
- Pie and donut take one series.
- Heatmap, surface, and bar_3d take one series with a grid of equal rows.
- A grid's columns run along x, its rows along z, and its values along y (up).
- A heatmap shows its grid from above, with values as colors.
- A candlestick takes one series with an [open, high, low, close] point per category.
- Rising candles are green and falling ones red.
- A sankey's links are each {from, to, value} between named nodes.
- A sankey node sits one column right of its farthest source.
- Each sankey column stacks its nodes in the order links first name them.
- categories names the x categories, pie slices, or grid columns; rows names grid rows.
- x_axis, y_axis, and z_axis each take a label and an optional min and max.
- Unset axis ends fit the data.
- stacked stacks bar, line, and area series that give values.
- A series takes a name for the legend and a color.
- Without colors, series take theme colors in turn and grids shade by value.
- Keys that set series change a chart's data over time.
- The axes fit the data of every key, so they hold still."#;

/// Registered tool that draws on the active monitor.
#[derive(Debug, Default)]
pub struct ScreenDrawTool;

impl ToolMetadata for ScreenDrawTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }
    fn description_template(&self) -> &str {
        DESCRIPTION
    }
}

impl Tool for ScreenDrawTool {
    type Args = ScreenDrawInput;
    type Output = ScreenDrawOutput;
    fn id(&self) -> ToolId {
        ToolId::new("screen_draw").expect("valid tool id")
    }
    fn description(&self, _: &ListToolsContext) -> ToolDescription {
        ToolDescription::new("screen_draw", self.sanitized_description_template())
    }
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(ToolScope::Write),
            ..Default::default()
        }
    }
    async fn run(
        &self,
        ctx: ToolCallContext,
        input: ScreenDrawInput,
    ) -> Result<ScreenDrawOutput, ToolError> {
        let screen = {
            let resources = shared_resources(&ctx)?;
            let mut resources = resources.lock().await;
            if resources.get::<Screen>().is_none() {
                let scope = resources
                    .get::<ProcessScope>()
                    .cloned()
                    .unwrap_or_else(ProcessScope::new);
                resources.insert(Screen {
                    scope,
                    overlay: Arc::default(),
                });
            }
            resources.require::<Screen>()?.clone()
        };
        screen.draw(&input).await
    }
}

/// A session's `xai-screen draw`, kept running while its drawing shows.
#[derive(Clone)]
struct Screen {
    scope: ProcessScope,
    overlay: Arc<Mutex<Option<ScreenProcess>>>,
}

impl Screen {
    async fn draw(&self, input: &ScreenDrawInput) -> Result<ScreenDrawOutput, ToolError> {
        let mut slot = self.overlay.lock().await;
        // The process stays out of the slot until it answers. Clearing, a failed or canceled
        // call, or a dead process drops it, which kills it and removes its drawing.
        let running = slot
            .take()
            .and_then(|mut overlay| overlay.running().then_some(overlay));
        if input.shapes.is_empty() {
            return Ok(ScreenDrawOutput::Cleared);
        }
        let mut overlay = match running {
            Some(overlay) => overlay,
            None => ScreenProcess::spawn("draw", &self.scope)?,
        };
        let reply = tokio::time::timeout(SHOW_TIMEOUT, async {
            overlay.send(input).await?;
            overlay.receive::<Drawn>(MAX_REPLY).await
        })
        .await
        .map_err(|_| {
            ToolError::new(
                ToolErrorKind::Timeout,
                "Screen: the drawing did not appear in time",
            )
        })??;
        // A drawing `xai-screen` refuses leaves the previous one on screen.
        *slot = Some(overlay);
        let Drawn {
            monitor,
            width,
            height,
        } = reply.map_err(failure)?;
        Ok(ScreenDrawOutput::Drawn {
            monitor,
            width,
            height,
            shapes: input.shapes.len(),
        })
    }
}

/// Where `xai-screen` showed a drawing.
#[derive(Debug, Deserialize)]
struct Drawn {
    monitor: String,
    width: f64,
    height: f64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ScreenDrawInput, ScreenDrawOutput};

    #[test]
    fn input_passes_shapes_on_unchanged_and_rejects_unknown_fields() {
        let drawing =
            json!({"shapes": [{"kind": "box", "x": 1, "border": false}], "repeat": "loop"});
        let input: ScreenDrawInput = serde_json::from_value(drawing.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), drawing);
        let extra = json!({"shapes": [], "monitor": "eDP-1"});
        assert!(serde_json::from_value::<ScreenDrawInput>(extra).is_err());
    }

    #[test]
    fn output_tells_the_model_what_shows() {
        let drawn = ScreenDrawOutput::Drawn {
            monitor: "eDP-1".to_owned(),
            width: 1280.0,
            height: 800.0,
            shapes: 1,
        };
        assert!(
            drawn
                .to_string()
                .starts_with("Drew 1 shape on monitor eDP-1 (1280 × 800 logical units).")
        );
        assert_eq!(
            ScreenDrawOutput::Cleared.to_string(),
            "Cleared the drawing."
        );
    }
}
