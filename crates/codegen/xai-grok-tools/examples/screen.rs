//! Check the screen tools on this desktop.
//!
//! - `screen draw` shows every drawing case in turn; press Enter for the next one.
//! - `screen draw N` shows only case N.
//! - `screen read DIRECTORY` reads the visible windows and saves one image per window.
//!
//! Each case is exactly what a model would send to `screen_draw`: a `shapes` list, or the
//! whole input when it also sets `repeat`. It is printed in the terminal while it is on
//! screen; only the caption at the top is added. Cases 12 to 21 animate, and cases 22 to 31
//! chart data. The cases fit a monitor of 1280 × 800 logical units. The example also serves
//! as its own overlay process, so it needs no separately built `grok-screen-overlay`.
//!
//! ```sh
//! just build xai-grok-tools --example screen
//! ./target/debug/examples/screen draw
//! ```

use std::ops::RangeInclusive;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use xai_grok_tools::implementations::grok_build::screen_draw::{
    ScreenDrawInput, ScreenDrawTool, overlay,
};
use xai_grok_tools::implementations::grok_build::screen_read::{ScreenReadInput, ScreenReadTool};
use xai_grok_tools::types::resources::Resources;
use xai_grok_tools::types::tool_metadata::test_ctx;
use xai_tool_runtime::{Tool, ToolCallContext};

const CASES: [(&str, fn() -> Value); 31] = [
    ("Box colors", case_1_box_colors),
    ("Box fill and border", case_2_box_fill_and_border),
    ("Lines and arrows", case_3_lines_and_arrows),
    ("Line widths", case_4_line_widths),
    ("Text colors and lines", case_5_text_colors_and_lines),
    (
        "Text size, fill, and border",
        case_6_text_size_fill_and_border,
    ),
    ("Scene objects", case_7_scene_objects),
    ("Scene cameras", case_8_scene_cameras),
    (
        "Scene fill, border, and line width",
        case_9_scene_fill_border_and_line_width,
    ),
    ("Layering", case_10_layering),
    ("Everything together", case_11_together),
    ("Motion and easing", case_12_motion_and_easing),
    (
        "Size, color, fill, and line",
        case_13_size_color_fill_and_line,
    ),
    ("Fading and changing text", case_14_fading_and_changing_text),
    ("Flipbook frames", case_15_flipbook_frames),
    ("Spinning scene", case_16_spinning_scene),
    (
        "Objects moving and turning",
        case_17_objects_moving_and_turning,
    ),
    ("Playing once", case_18_playing_once),
    ("Orbits", case_19_orbits),
    ("Growing bar chart", case_20_growing_bar_chart),
    ("Guided tour", case_21_guided_tour),
    ("Bar charts", case_22_bar_charts),
    ("Line and area charts", case_23_line_and_area_charts),
    ("Scatter plot", case_24_scatter_plot),
    ("Pie and donut", case_25_pie_and_donut),
    ("Heatmap", case_26_heatmap),
    ("Surface", case_27_surface),
    ("3D bars and points", case_28_3d_bars_and_points),
    ("Changing data", case_29_changing_data),
    ("Candlestick", case_30_candlestick),
    ("Sankey", case_31_sankey),
];

/// Every theme color. A box's label sits just above it.
fn case_1_box_colors() -> Value {
    json!([
        {
            "kind": "box", "x": 55, "y": 200, "width": 210, "height": 160,
            "label": "accent", "color": "accent",
        },
        {
            "kind": "box", "x": 295, "y": 200, "width": 210, "height": 160,
            "label": "red", "color": "red",
        },
        {
            "kind": "box", "x": 535, "y": 200, "width": 210, "height": 160,
            "label": "yellow", "color": "yellow",
        },
        {
            "kind": "box", "x": 775, "y": 200, "width": 210, "height": 160,
            "label": "green", "color": "green",
        },
        {
            "kind": "box", "x": 1015, "y": 200, "width": 210, "height": 160,
            "label": "blue", "color": "blue",
        },
    ])
}

/// Fill is an opacity from 0 to 1; the default is a faint 0.08. The border is optional.
fn case_2_box_fill_and_border() -> Value {
    json!([
        {
            "kind": "box", "x": 55, "y": 200, "width": 210, "height": 160,
            "label": "default",
        },
        {
            "kind": "box", "x": 295, "y": 200, "width": 210, "height": 160,
            "label": "fill 0.4", "color": "red", "fill": 0.4,
        },
        {
            "kind": "box", "x": 535, "y": 200, "width": 210, "height": 160,
            "label": "fill 1", "color": "yellow", "fill": 1,
        },
        {
            "kind": "box", "x": 775, "y": 200, "width": 210, "height": 160,
            "label": "fill 0", "color": "green", "fill": 0,
        },
        {
            "kind": "box", "x": 1015, "y": 200, "width": 210, "height": 160,
            "label": "no border", "color": "blue", "fill": 0.4, "border": false,
        },
    ])
}

/// A line runs from (x, y) to (to_x, to_y). The top row shows each arrows choice, the
/// middle row each pattern, and the star arrows pointing out in eight directions.
fn case_3_lines_and_arrows() -> Value {
    json!([
        {
            "kind": "text", "x": 55, "y": 170,
            "text": "arrows: none", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 55, "y": 230, "to_x": 325, "to_y": 230,
        },
        {
            "kind": "text", "x": 355, "y": 170,
            "text": "arrows: start", "color": "red", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 355, "y": 230, "to_x": 625, "to_y": 230,
            "color": "red", "arrows": "start",
        },
        {
            "kind": "text", "x": 655, "y": 170,
            "text": "arrows: end", "color": "yellow", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 655, "y": 230, "to_x": 925, "to_y": 230,
            "color": "yellow", "arrows": "end",
        },
        {
            "kind": "text", "x": 955, "y": 170,
            "text": "arrows: both", "color": "green", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 955, "y": 230, "to_x": 1225, "to_y": 230,
            "color": "green", "arrows": "both",
        },
        {
            "kind": "text", "x": 55, "y": 290,
            "text": "pattern: solid", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 55, "y": 350, "to_x": 325, "to_y": 350,
        },
        {
            "kind": "text", "x": 355, "y": 290,
            "text": "pattern: dashed", "color": "red", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 355, "y": 350, "to_x": 625, "to_y": 350,
            "color": "red", "pattern": "dashed",
        },
        {
            "kind": "text", "x": 655, "y": 290,
            "text": "pattern: dotted", "color": "yellow", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 655, "y": 350, "to_x": 925, "to_y": 350,
            "color": "yellow", "pattern": "dotted",
        },
        {
            "kind": "text", "x": 955, "y": 290,
            "text": "dashed, both arrows", "color": "blue", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 955, "y": 350, "to_x": 1225, "to_y": 350,
            "color": "blue", "pattern": "dashed", "arrows": "both",
        },
        {
            "kind": "line", "x": 670, "y": 600, "to_x": 790, "to_y": 600,
            "arrows": "end",
        },
        {
            "kind": "line", "x": 661, "y": 621, "to_x": 746, "to_y": 706,
            "color": "red", "arrows": "end",
        },
        {
            "kind": "line", "x": 640, "y": 630, "to_x": 640, "to_y": 750,
            "color": "yellow", "arrows": "end",
        },
        {
            "kind": "line", "x": 619, "y": 621, "to_x": 534, "to_y": 706,
            "color": "green", "arrows": "end", "pattern": "dotted",
        },
        {
            "kind": "line", "x": 610, "y": 600, "to_x": 490, "to_y": 600,
            "color": "blue", "arrows": "end", "pattern": "dashed",
        },
        {
            "kind": "line", "x": 619, "y": 579, "to_x": 534, "to_y": 494,
            "arrows": "end", "pattern": "dotted",
        },
        {
            "kind": "line", "x": 640, "y": 570, "to_x": 640, "to_y": 450,
            "color": "red", "arrows": "end", "pattern": "dashed",
        },
        {
            "kind": "line", "x": 661, "y": 579, "to_x": 746, "to_y": 494,
            "color": "yellow", "arrows": "both",
        },
    ])
}

/// Boxes without labels and lines, from thin to thick. The default follows the window
/// border; lines default to 1.5 times it.
fn case_4_line_widths() -> Value {
    json!([
        {
            "kind": "box", "x": 55, "y": 200, "width": 270, "height": 160,
            "line_width": 1,
        },
        {
            "kind": "box", "x": 355, "y": 200, "width": 270, "height": 160,
            "color": "red", "line_width": 3,
        },
        {
            "kind": "box", "x": 655, "y": 200, "width": 270, "height": 160,
            "color": "green", "line_width": 6,
        },
        {
            "kind": "box", "x": 955, "y": 200, "width": 270, "height": 160,
            "color": "blue", "line_width": 10,
        },
        {
            "kind": "text", "x": 55, "y": 420,
            "text": "line_width 1", "fill": 0, "border": false,
        },
        {
            "kind": "text", "x": 355, "y": 420,
            "text": "line_width 3", "color": "red", "fill": 0, "border": false,
        },
        {
            "kind": "text", "x": 655, "y": 420,
            "text": "line_width 6", "color": "green", "fill": 0, "border": false,
        },
        {
            "kind": "text", "x": 955, "y": 420,
            "text": "line_width 10", "color": "blue", "fill": 0, "border": false,
        },
        {
            "kind": "line", "x": 55, "y": 520, "to_x": 325, "to_y": 520,
            "line_width": 1, "arrows": "end",
        },
        {
            "kind": "line", "x": 355, "y": 520, "to_x": 625, "to_y": 520,
            "color": "red", "line_width": 3, "arrows": "end",
        },
        {
            "kind": "line", "x": 655, "y": 520, "to_x": 925, "to_y": 520,
            "color": "green", "line_width": 6, "arrows": "end",
        },
        {
            "kind": "line", "x": 955, "y": 520, "to_x": 1225, "to_y": 520,
            "color": "blue", "line_width": 10, "arrows": "end",
        },
    ])
}

/// Text starts at its top-left corner and may span lines. Its color outlines the panel.
fn case_5_text_colors_and_lines() -> Value {
    json!([
        {
            "kind": "text", "x": 55, "y": 200,
            "text": "Default text uses the accent color",
        },
        {
            "kind": "text", "x": 55, "y": 260,
            "text": "Red text", "color": "red",
        },
        {
            "kind": "text", "x": 55, "y": 320,
            "text": "Yellow text", "color": "yellow",
        },
        {
            "kind": "text", "x": 55, "y": 380,
            "text": "Green text", "color": "green",
        },
        {
            "kind": "text", "x": 55, "y": 440,
            "text": "Blue text", "color": "blue",
        },
        {
            "kind": "text", "x": 640, "y": 200,
            "text": "Text may span lines.\nEvery line starts at the same x,\nso short notes stay tidy.",
        },
        {
            "kind": "text", "x": 640, "y": 340,
            "text": "Long text wraps at 30 times its size, so this sentence breaks onto a second line.",
        },
        {
            "kind": "text", "x": 640, "y": 440,
            "text": "Symbols: → ← ↑ ↓ ✓ ✗ ± ° ×", "color": "yellow",
        },
    ])
}

/// Size is in logical units; the default is 14. The panel takes a fill and border like a
/// box, and text without a border shows its words in its color.
fn case_6_text_size_fill_and_border() -> Value {
    json!([
        {
            "kind": "text", "x": 55, "y": 200,
            "text": "size 10", "size": 10,
        },
        {
            "kind": "text", "x": 55, "y": 250,
            "text": "size 14, the default",
        },
        {
            "kind": "text", "x": 55, "y": 310,
            "text": "size 24", "size": 24,
        },
        {
            "kind": "text", "x": 55, "y": 390,
            "text": "size 40", "size": 40,
        },
        {
            "kind": "text", "x": 640, "y": 200,
            "text": "fill 0.4: windows show through", "color": "red", "fill": 0.4,
        },
        {
            "kind": "text", "x": 640, "y": 280,
            "text": "fill 1: a solid panel", "color": "green", "fill": 1,
        },
        {
            "kind": "text", "x": 640, "y": 360,
            "text": "no border: the words take the color", "color": "yellow", "border": false,
        },
        {
            "kind": "text", "x": 640, "y": 440,
            "text": "fill 0, no border: bare words", "color": "blue", "fill": 0, "border": false,
        },
    ])
}

/// Each kind of scene object, lit and fitted to its frame.
fn case_7_scene_objects() -> Value {
    json!([
        {
            "kind": "scene", "x": 55, "y": 200, "width": 210, "height": 260,
            "label": "cuboid",
            "objects": [
                { "kind": "cuboid", "center": [0, 0, 0], "size": [2, 1, 1.4] },
            ],
        },
        {
            "kind": "scene", "x": 295, "y": 200, "width": 210, "height": 260,
            "label": "sphere",
            "objects": [
                { "kind": "sphere", "center": [0, 0, 0], "radius": 1, "color": "red" },
            ],
        },
        {
            "kind": "scene", "x": 535, "y": 200, "width": 210, "height": 260,
            "label": "cylinder",
            "objects": [
                {
                    "kind": "cylinder", "from": [0, -1, 0], "to": [0, 1, 0], "radius": 0.5,
                    "color": "yellow",
                },
            ],
        },
        {
            "kind": "scene", "x": 775, "y": 200, "width": 210, "height": 260,
            "label": "lines: x, y, z",
            "objects": [
                { "kind": "line", "from": [0, 0, 0], "to": [1, 0, 0], "color": "red" },
                { "kind": "line", "from": [0, 0, 0], "to": [0, 1, 0], "color": "green" },
                { "kind": "line", "from": [0, 0, 0], "to": [0, 0, 1], "color": "blue" },
            ],
        },
        {
            "kind": "scene", "x": 1015, "y": 200, "width": 210, "height": 260,
            "label": "mesh",
            "objects": [
                {
                    "kind": "mesh", "color": "blue",
                    "vertices": [[-1, 0, -1], [1, 0, -1], [1, 0, 1], [-1, 0, 1], [0, 1.5, 0]],
                    "faces": [[3, 2, 4], [2, 1, 4], [1, 0, 4], [0, 3, 4], [0, 1, 2], [0, 2, 3]],
                },
            ],
        },
    ])
}

/// One scene from five cameras. The default camera is yaw 35, pitch 25. Drag any scene to
/// orbit it, and scroll over it to zoom.
fn case_8_scene_cameras() -> Value {
    json!([
        {
            "kind": "scene", "x": 55, "y": 200, "width": 210, "height": 280,
            "label": "front: 0, 0", "camera": { "yaw": 0, "pitch": 0 },
            "objects": robot_arm(),
        },
        {
            "kind": "scene", "x": 295, "y": 200, "width": 210, "height": 280,
            "label": "default: 35, 25",
            "objects": robot_arm(),
        },
        {
            "kind": "scene", "x": 535, "y": 200, "width": 210, "height": 280,
            "label": "side: 90, 0", "camera": { "yaw": 90, "pitch": 0 },
            "objects": robot_arm(),
        },
        {
            "kind": "scene", "x": 775, "y": 200, "width": 210, "height": 280,
            "label": "top: 0, 90", "camera": { "yaw": 0, "pitch": 90 },
            "objects": robot_arm(),
        },
        {
            "kind": "scene", "x": 1015, "y": 200, "width": 210, "height": 280,
            "label": "below: 35, -30", "camera": { "yaw": 35, "pitch": -30 },
            "objects": robot_arm(),
        },
    ])
}

/// The backdrop is the theme background at the fill opacity; the default is 0.92. Line
/// width sets the frame and the line objects. A scene needs no label.
fn case_9_scene_fill_border_and_line_width() -> Value {
    json!([
        {
            "kind": "scene", "x": 55, "y": 200, "width": 270, "height": 320,
            "label": "default",
            "objects": robot_arm(),
        },
        {
            "kind": "scene", "x": 355, "y": 200, "width": 270, "height": 320,
            "label": "fill 1", "fill": 1,
            "objects": robot_arm(),
        },
        {
            "kind": "scene", "x": 655, "y": 200, "width": 270, "height": 320,
            "label": "fill 0.4, line_width 5", "fill": 0.4, "line_width": 5,
            "objects": robot_arm(),
        },
        {
            "kind": "scene", "x": 955, "y": 200, "width": 270, "height": 320,
            "fill": 0, "border": false,
            "objects": robot_arm(),
        },
    ])
}

/// Boxes, lines, and scenes draw in list order, so later ones cover earlier ones. Text
/// and labels draw last, so words stay on top even when listed first.
fn case_10_layering() -> Value {
    json!([
        {
            "kind": "text", "x": 420, "y": 360,
            "text": "Listed first, drawn last", "size": 20,
        },
        {
            "kind": "box", "x": 200, "y": 200, "width": 420, "height": 260,
            "label": "first", "color": "red", "fill": 1,
        },
        {
            "kind": "box", "x": 460, "y": 330, "width": 420, "height": 260,
            "label": "second: covers first", "color": "blue", "fill": 1,
        },
        {
            "kind": "line", "x": 150, "y": 650, "to_x": 1000, "to_y": 250,
            "color": "yellow", "line_width": 6, "arrows": "end",
        },
    ])
}

/// A scene with a callout: a box, text inside it, and a dashed arrow into the scene.
fn case_11_together() -> Value {
    json!([
        {
            "kind": "scene", "x": 55, "y": 150, "width": 700, "height": 580,
            "label": "house",
            "objects": [
                { "kind": "cuboid", "center": [0, 0.5, 0], "size": [2, 1, 2], "color": "blue" },
                {
                    "kind": "mesh", "color": "red",
                    "vertices": [
                        [-1.2, 1, -1.2], [1.2, 1, -1.2], [1.2, 1, 1.2], [-1.2, 1, 1.2], [0, 2, 0],
                    ],
                    "faces": [[3, 2, 4], [2, 1, 4], [1, 0, 4], [0, 3, 4], [0, 1, 2], [0, 2, 3]],
                },
                {
                    "kind": "cuboid", "center": [0.6, 1.7, -0.4], "size": [0.25, 0.6, 0.25],
                    "color": "yellow",
                },
                {
                    "kind": "cylinder", "from": [2, 0, 0.8], "to": [2, 0.9, 0.8], "radius": 0.12,
                    "color": "yellow",
                },
                { "kind": "sphere", "center": [2, 1.3, 0.8], "radius": 0.5, "color": "green" },
                { "kind": "line", "from": [-1.8, 0, 1.8], "to": [2.8, 0, 1.8] },
            ],
        },
        {
            "kind": "box", "x": 840, "y": 300, "width": 385, "height": 110,
            "label": "callout", "color": "yellow",
        },
        {
            "kind": "text", "x": 860, "y": 320,
            "text": "Boxes, text, and arrows\ncan point into a scene.", "color": "yellow",
        },
        {
            "kind": "line", "x": 840, "y": 355, "to_x": 640, "to_y": 420,
            "color": "yellow", "arrows": "end", "pattern": "dashed",
        },
    ])
}

/// Each box crosses the screen and back in 4 seconds: linear moves at a constant speed,
/// in_out starts and stops gently, and step jumps when each key arrives.
fn case_12_motion_and_easing() -> Value {
    json!({
        "repeat": "loop",
        "shapes": [
            {
                "kind": "box", "x": 100, "y": 200, "width": 140, "height": 80,
                "label": "linear", "color": "blue",
                "keys": [
                    { "at": 2, "x": 1040 },
                    { "at": 4, "x": 100 },
                ],
            },
            {
                "kind": "box", "x": 100, "y": 380, "width": 140, "height": 80,
                "label": "in_out", "color": "green",
                "keys": [
                    { "at": 2, "x": 1040, "ease": "in_out" },
                    { "at": 4, "x": 100, "ease": "in_out" },
                ],
            },
            {
                "kind": "box", "x": 100, "y": 560, "width": 140, "height": 80,
                "label": "step", "color": "yellow",
                "keys": [
                    { "at": 2, "x": 1040, "ease": "step" },
                    { "at": 4, "x": 100, "ease": "step" },
                ],
            },
        ],
    })
}

/// A box grows and turns red, a line draws itself out, a dashed line thickens, and a fill
/// pulses. Colors switch when their key arrives.
fn case_13_size_color_fill_and_line() -> Value {
    json!({
        "repeat": "loop",
        "shapes": [
            {
                "kind": "box", "x": 100, "y": 160, "width": 100, "height": 80,
                "label": "grows",
                "keys": [
                    { "at": 1.5, "width": 400, "height": 200, "color": "red", "ease": "in_out" },
                    { "at": 3, "width": 100, "height": 80, "color": "accent", "ease": "in_out" },
                ],
            },
            {
                "kind": "box", "x": 100, "y": 500, "width": 400, "height": 180,
                "label": "fill pulses", "color": "green", "fill": 0,
                "keys": [
                    { "at": 1.5, "fill": 0.8, "ease": "in_out" },
                    { "at": 3, "fill": 0, "ease": "in_out" },
                ],
            },
            {
                "kind": "line", "x": 640, "y": 200, "to_x": 641, "to_y": 200,
                "arrows": "end", "color": "yellow",
                "keys": [
                    { "at": 1.5, "to_x": 1180, "to_y": 420, "ease": "in_out" },
                    { "at": 3, "to_x": 641, "to_y": 200, "ease": "in_out" },
                ],
            },
            {
                "kind": "line", "x": 640, "y": 600, "to_x": 1180, "to_y": 600,
                "line_width": 1, "pattern": "dashed", "color": "blue",
                "keys": [
                    { "at": 1.5, "line_width": 12, "ease": "in_out" },
                    { "at": 3, "line_width": 1, "ease": "in_out" },
                ],
            },
        ],
    })
}

/// A note fades in, holds, and fades out; a countdown changes its text and color; another
/// note glides back and forth.
fn case_14_fading_and_changing_text() -> Value {
    json!({
        "repeat": "loop",
        "shapes": [
            {
                "kind": "text", "x": 100, "y": 180,
                "text": "Fading in and out", "size": 32, "opacity": 0,
                "keys": [
                    { "at": 1, "opacity": 1, "ease": "in_out" },
                    { "at": 3, "opacity": 1 },
                    { "at": 4, "opacity": 0, "ease": "in_out" },
                ],
            },
            {
                "kind": "text", "x": 100, "y": 360,
                "text": "3", "size": 64, "color": "red",
                "keys": [
                    { "at": 1, "text": "2", "color": "yellow" },
                    { "at": 2, "text": "1" },
                    { "at": 3, "text": "Go!", "color": "green" },
                ],
            },
            {
                "kind": "text", "x": 640, "y": 560,
                "text": "A note that glides", "border": false, "color": "blue",
                "keys": [
                    { "at": 2, "x": 1000, "ease": "in_out" },
                    { "at": 4, "x": 640, "ease": "in_out" },
                ],
            },
        ],
    })
}

/// Three frames in one place, a second each: step keys on opacity show one at a time.
fn case_15_flipbook_frames() -> Value {
    let frame = |label: &str, color: &str, shown: Value| {
        json!({
            "kind": "box", "x": 440, "y": 220, "width": 400, "height": 300,
            "label": label, "color": color, "fill": 0.5,
            "opacity": shown["opacity"], "keys": shown["keys"],
        })
    };
    let hide = |at: f64| json!({ "at": at, "opacity": 0, "ease": "step" });
    let show = |at: f64| json!({ "at": at, "opacity": 1, "ease": "step" });
    json!({
        "repeat": "loop",
        "shapes": [
            frame("frame 1: stop", "red", json!({ "opacity": 1, "keys": [hide(1.0), show(3.0)] })),
            frame("frame 2: go", "green", json!({ "opacity": 0, "keys": [show(1.0), hide(2.0)] })),
            frame("frame 3: slow", "yellow", json!({ "opacity": 0, "keys": [show(2.0), hide(3.0)] })),
            {
                "kind": "text", "x": 440, "y": 560,
                "text": "frame 1",
                "keys": [
                    { "at": 1, "text": "frame 2" },
                    { "at": 2, "text": "frame 3" },
                    { "at": 3, "text": "frame 1" },
                ],
            },
        ],
    })
}

/// The camera circles the robot arm once every 6 seconds. Drag it to take the camera over.
fn case_16_spinning_scene() -> Value {
    json!({
        "repeat": "loop",
        "shapes": [
            {
                "kind": "scene", "x": 340, "y": 130, "width": 600, "height": 600,
                "label": "yaw 0 to 360", "camera": { "yaw": 0, "pitch": 20 },
                "objects": robot_arm(),
                "keys": [
                    { "at": 6, "yaw": 360 },
                ],
            },
        ],
    })
}

/// A cube spins, a ball bounces, a block slides, grows, and changes color, and a post tips
/// over and back. The scene keeps its size because it fits around everywhere they go.
fn case_17_objects_moving_and_turning() -> Value {
    let bounce = |at: f64, y: f64| json!({ "at": at, "center": [-0.8, y, 0], "ease": "in_out" });
    json!({
        "repeat": "loop",
        "shapes": [
            {
                "kind": "scene", "x": 140, "y": 130, "width": 1000, "height": 620,
                "label": "objects with keys",
                "objects": [
                    {
                        "kind": "cuboid", "center": [0, -0.1, 0], "size": [7, 0.2, 3.4],
                        "color": "blue",
                    },
                    {
                        "kind": "cuboid", "center": [-2.5, 0.5, 0], "size": [0.8, 0.8, 0.8],
                        "keys": [
                            { "at": 4, "rotation": [0, 360, 0] },
                        ],
                    },
                    {
                        "kind": "sphere", "center": [-0.8, 0.4, 0], "radius": 0.4, "color": "red",
                        "keys": [
                            bounce(1.0, 2.2),
                            bounce(2.0, 0.4),
                            bounce(3.0, 2.2),
                            bounce(4.0, 0.4),
                        ],
                    },
                    {
                        "kind": "cuboid", "center": [0.8, 0.3, 1], "size": [0.6, 0.6, 0.6],
                        "color": "green",
                        "keys": [
                            {
                                "at": 2, "center": [0.8, 0.5, -1], "size": [1, 1, 1],
                                "color": "yellow", "ease": "in_out",
                            },
                            {
                                "at": 4, "center": [0.8, 0.3, 1], "size": [0.6, 0.6, 0.6],
                                "color": "green", "ease": "in_out",
                            },
                        ],
                    },
                    {
                        "kind": "cylinder", "from": [2.5, 0, 0], "to": [2.5, 1.6, 0],
                        "radius": 0.15, "color": "yellow",
                        "keys": [
                            { "at": 2, "rotation": [0, 0, 80], "ease": "in_out" },
                            { "at": 4, "rotation": [0, 0, 0], "ease": "in_out" },
                        ],
                    },
                ],
            },
        ],
    })
}

/// Without repeat, a timeline plays once and keeps its last frame: a box slides and fades
/// in, an arrow draws out to it, then a note appears.
fn case_18_playing_once() -> Value {
    json!([
        {
            "kind": "box", "x": 100, "y": 300, "width": 300, "height": 160,
            "label": "Save button", "opacity": 0,
            "keys": [
                { "at": 1, "x": 600, "opacity": 1, "ease": "in_out" },
            ],
        },
        {
            "kind": "line", "x": 320, "y": 620, "to_x": 321, "to_y": 620,
            "arrows": "end", "opacity": 0,
            "keys": [
                { "at": 1, "opacity": 1, "ease": "step" },
                { "at": 2, "to_x": 640, "to_y": 470, "ease": "in_out" },
            ],
        },
        {
            "kind": "text", "x": 100, "y": 600,
            "text": "Click here to save", "opacity": 0,
            "keys": [
                { "at": 2, "opacity": 0 },
                { "at": 2.5, "opacity": 1, "ease": "in_out" },
            ],
        },
    ])
}

/// A planet circles the sun every 6 seconds while its moon circles it three times. Many
/// close keys trace a smooth curve.
fn case_19_orbits() -> Value {
    const STEPS: usize = 64;
    let circle = |radius: f64, turns: f64, t: f64| {
        let angle = std::f64::consts::TAU * turns * t / 6.0;
        [radius * angle.cos(), 0.0, radius * angle.sin()]
    };
    let planet = |t: f64| circle(3.0, 1.0, t);
    let moon = |t: f64| {
        let (planet, moon) = (planet(t), circle(0.7, 3.0, t));
        [planet[0] + moon[0], 0.0, planet[2] + moon[2]]
    };
    let round = |point: [f64; 3]| point.map(|value| (value * 1000.0).round() / 1000.0);
    let keys = |place: &dyn Fn(f64) -> [f64; 3]| -> Vec<Value> {
        (1..=STEPS)
            .map(|step| 6.0 * step as f64 / STEPS as f64)
            .map(|at| json!({ "at": at, "center": round(place(at)) }))
            .collect()
    };
    json!({
        "repeat": "loop",
        "shapes": [
            {
                "kind": "scene", "x": 290, "y": 110, "width": 700, "height": 640,
                "label": "sun, planet, and moon", "camera": { "yaw": 0, "pitch": 35 },
                "objects": [
                    { "kind": "sphere", "center": [0, 0, 0], "radius": 0.9, "color": "yellow" },
                    {
                        "kind": "sphere", "center": round(planet(0.0)), "radius": 0.35,
                        "color": "green", "keys": keys(&planet),
                    },
                    {
                        "kind": "sphere", "center": round(moon(0.0)), "radius": 0.12,
                        "keys": keys(&moon),
                    },
                ],
            },
        ],
    })
}

/// A chart builds itself and stays: each bar grows from the axis in turn, then its value
/// fades in above it. A key that repeats a value holds it until the next key starts.
fn case_20_growing_bar_chart() -> Value {
    let mut shapes = vec![
        json!({
            "kind": "text", "x": 220, "y": 150,
            "text": "Commits per day", "size": 32, "border": false,
        }),
        json!({
            "kind": "line", "x": 180, "y": 640, "to_x": 1100, "to_y": 640,
        }),
    ];
    let days = [
        ("Mon", 42),
        ("Tue", 75),
        ("Wed", 58),
        ("Thu", 96),
        ("Fri", 120),
    ];
    for (index, (day, value)) in days.into_iter().enumerate() {
        let (x, start, height) = (220 + 180 * index, 0.4 * index as f64, 3 * value);
        shapes.extend([
            json!({
                "kind": "box", "x": x, "y": 639, "width": 120, "height": 1,
                "color": "blue", "fill": 0.6,
                "keys": [
                    { "at": start, "y": 639, "height": 1 },
                    { "at": start + 1.0, "y": 640 - height, "height": height, "ease": "in_out" },
                ],
            }),
            json!({
                "kind": "text", "x": x + 38, "y": 652,
                "text": day, "border": false,
            }),
            json!({
                "kind": "text", "x": x + 40, "y": 600 - height,
                "text": value.to_string(), "border": false, "opacity": 0,
                "keys": [
                    { "at": start + 1.0, "opacity": 0 },
                    { "at": start + 1.4, "opacity": 1 },
                ],
            }),
        ]);
    }
    Value::Array(shapes)
}

/// A highlight walks along the top bar and holds on each stop while a note explains it,
/// the way a model would show the user around the screen.
fn case_21_guided_tour() -> Value {
    let ease = "in_out";
    json!({
        "repeat": "loop",
        "shapes": [
            {
                "kind": "box", "x": 8, "y": 4, "width": 130, "height": 32,
                "color": "yellow", "fill": 0.2,
                "keys": [
                    { "at": 1.5, "x": 8, "width": 130 },
                    { "at": 2.1, "x": 540, "width": 200, "ease": ease },
                    { "at": 3.6, "x": 540, "width": 200 },
                    { "at": 4.2, "x": 1000, "width": 272, "ease": ease },
                    { "at": 5.7, "x": 1000, "width": 272 },
                    { "at": 6.3, "x": 8, "width": 130, "ease": ease },
                    { "at": 7.5, "x": 8 },
                ],
            },
            {
                "kind": "line", "x": 48, "y": 140, "to_x": 48, "to_y": 42,
                "arrows": "end", "color": "yellow",
                "keys": [
                    { "at": 1.5, "x": 48, "to_x": 48 },
                    { "at": 2.1, "x": 580, "to_x": 580, "ease": ease },
                    { "at": 3.6, "x": 580, "to_x": 580 },
                    { "at": 4.2, "x": 1040, "to_x": 1040, "ease": ease },
                    { "at": 5.7, "x": 1040, "to_x": 1040 },
                    { "at": 6.3, "x": 48, "to_x": 48, "ease": ease },
                ],
            },
            {
                "kind": "text", "x": 8, "y": 150,
                "text": "Workspaces: Super + 1 to 9", "color": "yellow",
                "keys": [
                    { "at": 1.5, "x": 8, "text": "Clock: click for the calendar" },
                    { "at": 2.1, "x": 540, "ease": ease },
                    { "at": 3.6, "x": 540, "text": "Tray: Wi-Fi, sound, and power" },
                    { "at": 4.2, "x": 1000, "ease": ease },
                    { "at": 5.7, "x": 1000, "text": "Workspaces: Super + 1 to 9" },
                    { "at": 6.3, "x": 8, "ease": ease },
                ],
            },
        ],
    })
}

/// A base, a mast, a joint, an angled arm, and a plumb line.
fn robot_arm() -> Value {
    json!([
        { "kind": "cuboid", "center": [0, 0, 0], "size": [2.4, 0.4, 1.4], "color": "blue" },
        { "kind": "cylinder", "from": [0, 0.2, 0], "to": [0, 1.8, 0], "radius": 0.16 },
        { "kind": "sphere", "center": [0, 1.95, 0], "radius": 0.3, "color": "red" },
        {
            "kind": "cylinder", "from": [0, 1.95, 0], "to": [1.3, 2.6, 0], "radius": 0.12,
            "color": "yellow",
        },
        { "kind": "line", "from": [1.3, 2.6, 0], "to": [1.3, 0.2, 0], "color": "green" },
    ])
}

/// Grouped bars, the same bars stacked, and bars below zero.
fn case_22_bar_charts() -> Value {
    let quarters = json!(["Q1", "Q2", "Q3", "Q4"]);
    let sales = json!([
        { "name": "2025", "values": [120, 150, 170, 210] },
        { "name": "2026", "values": [140, 190, 160, 260] },
    ]);
    json!([
        {
            "kind": "chart", "x": 60, "y": 120, "width": 560, "height": 320,
            "label": "grouped", "plot": "bar",
            "categories": quarters,
            "series": sales,
            "y_axis": { "label": "Revenue ($k)" },
        },
        {
            "kind": "chart", "x": 660, "y": 120, "width": 560, "height": 320,
            "label": "stacked", "plot": "bar",
            "categories": quarters,
            "series": sales,
            "stacked": true,
        },
        {
            "kind": "chart", "x": 60, "y": 490, "width": 1160, "height": 280,
            "label": "profit by month", "plot": "bar",
            "categories": [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ],
            "series": [
                { "values": [12, -8, 15, 22, -14, 30, 26, -5, 18, 34, 41, 28], "color": "green" },
            ],
            "x_axis": { "label": "2026" },
        },
    ])
}

/// Lines over categories, stacked areas, and smooth curves over numbers.
fn case_23_line_and_area_charts() -> Value {
    let months = json!(["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug"]);
    let curve = |wave: fn(f64) -> f64| -> Vec<[f64; 2]> {
        (0..=60)
            .map(|step| step as f64 * std::f64::consts::TAU / 60.0)
            .map(|x| [round(x), round(wave(x))])
            .collect()
    };
    json!([
        {
            "kind": "chart", "x": 60, "y": 120, "width": 560, "height": 320,
            "label": "visitors", "plot": "line",
            "categories": months,
            "series": [
                { "name": "web", "values": [320, 410, 380, 520, 610, 580, 700, 760] },
                { "name": "app", "values": [150, 180, 260, 300, 340, 420, 480, 590] },
            ],
            "y_axis": { "label": "Visitors per day" },
        },
        {
            "kind": "chart", "x": 660, "y": 120, "width": 560, "height": 320,
            "label": "storage by type", "plot": "area",
            "categories": months,
            "stacked": true,
            "series": [
                { "name": "photos", "values": [40, 44, 47, 52, 60, 66, 70, 78] },
                { "name": "video", "values": [20, 28, 30, 41, 45, 52, 61, 66] },
                { "name": "documents", "values": [10, 11, 13, 13, 14, 16, 17, 18] },
            ],
            "y_axis": { "label": "GB" },
        },
        {
            "kind": "chart", "x": 60, "y": 490, "width": 1160, "height": 280,
            "label": "points over numbers", "plot": "line",
            "series": [
                { "name": "sin x", "points": curve(f64::sin) },
                { "name": "cos x", "points": curve(f64::cos) },
            ],
            "x_axis": { "label": "x" },
        },
    ])
}

/// Two clusters of points with axes fitted around them.
fn case_24_scatter_plot() -> Value {
    let cluster = |seed: f64, (x, y): (f64, f64)| -> Vec<[f64; 2]> {
        (0..80)
            .map(|i| {
                let (a, b) = (noise(seed + i as f64), noise(seed + i as f64 + 0.5));
                [round(x + 1.6 * (a - 0.5)), round(y + 1.2 * (a + b - 1.0))]
            })
            .collect()
    };
    json!([
        {
            "kind": "chart", "x": 190, "y": 130, "width": 900, "height": 600,
            "label": "height and weight", "plot": "scatter",
            "series": [
                { "name": "group A", "points": cluster(1.0, (165.0, 62.0)) },
                { "name": "group B", "points": cluster(9.0, (178.0, 75.0)) },
            ],
            "x_axis": { "label": "Height (cm)" },
            "y_axis": { "label": "Weight (kg)" },
        },
    ])
}

/// Shares of a whole, with percentages on the slices.
fn case_25_pie_and_donut() -> Value {
    let browsers = json!(["Chrome", "Firefox", "Safari", "Edge", "Other"]);
    json!([
        {
            "kind": "chart", "x": 60, "y": 140, "width": 560, "height": 560,
            "label": "pie", "plot": "pie",
            "categories": browsers,
            "series": [{ "values": [64, 9, 18, 5, 4] }],
        },
        {
            "kind": "chart", "x": 660, "y": 140, "width": 560, "height": 560,
            "label": "donut", "plot": "donut",
            "categories": ["Rent", "Food", "Travel", "Savings", "Fun", "Health", "Books", "Gifts"],
            "series": [{ "values": [35, 18, 10, 15, 8, 6, 4, 4] }],
        },
    ])
}

/// Activity by weekday and hour: rows named, columns numbered by the x axis span.
fn case_26_heatmap() -> Value {
    let grid: Vec<Vec<f64>> = (0..7)
        .map(|day| {
            (0..24)
                .map(|hour| {
                    let work = if day < 5 { 1.0 } else { 0.4 };
                    let busy = (-((hour as f64 - 14.0) / 4.0).powi(2)).exp();
                    (100.0 * work * busy + 10.0 * noise(f64::from(day * 24 + hour))).round()
                })
                .collect()
        })
        .collect();
    json!([
        {
            "kind": "chart", "x": 40, "y": 130, "width": 1200, "height": 420,
            "label": "commits by hour", "plot": "heatmap",
            "rows": ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
            "series": [{ "grid": grid }],
            "x_axis": { "label": "Hour of day", "min": 0, "max": 23 },
        },
    ])
}

/// z = sin(x) · cos(y) sampled on a 30 × 30 grid, shaded by height. Drag it to orbit.
fn case_27_surface() -> Value {
    let at = |index: usize| -3.0 + 6.0 * index as f64 / 29.0;
    let grid: Vec<Vec<f64>> = (0..30)
        .map(|row| {
            (0..30)
                .map(|column| round(at(column).sin() * at(row).cos()))
                .collect()
        })
        .collect();
    json!([
        {
            "kind": "chart", "x": 240, "y": 110, "width": 800, "height": 660,
            "label": "sin x · cos y", "plot": "surface",
            "series": [{ "grid": grid }],
            "x_axis": { "label": "x", "min": -3, "max": 3 },
            "z_axis": { "label": "y", "min": -3, "max": 3 },
            "y_axis": { "label": "z" },
        },
    ])
}

/// Sales by region and month as 3D bars, and two clouds of 3D points.
fn case_28_3d_bars_and_points() -> Value {
    let sales: Vec<Vec<f64>> = (0..4)
        .map(|region| {
            (0..6)
                .map(|month| {
                    (20.0 + 8.0 * month as f64 + 30.0 * noise(f64::from(region * 6 + month)))
                        .round()
                })
                .collect()
        })
        .collect();
    let cloud = |seed: f64, center: [f64; 3]| -> Vec<[f64; 3]> {
        (0..60)
            .map(|i| {
                std::array::from_fn(|axis| {
                    round(center[axis] + 2.0 * noise(seed + i as f64 + axis as f64 * 0.3) - 1.0)
                })
            })
            .collect()
    };
    json!([
        {
            "kind": "chart", "x": 40, "y": 130, "width": 590, "height": 600,
            "label": "sales by region", "plot": "bar_3d",
            "categories": ["Jan", "Feb", "Mar", "Apr", "May", "Jun"],
            "rows": ["North", "East", "South", "West"],
            "series": [{ "grid": sales }],
            "y_axis": { "label": "units" },
        },
        {
            "kind": "chart", "x": 650, "y": 130, "width": 590, "height": 600,
            "label": "two clouds", "plot": "scatter_3d",
            "series": [
                { "name": "first", "points": cloud(3.0, [0.0, 0.0, 0.0]) },
                { "name": "second", "points": cloud(40.0, [2.5, 2.0, 1.5]) },
            ],
            "x_axis": { "label": "x" },
            "y_axis": { "label": "y" },
            "z_axis": { "label": "z" },
        },
    ])
}

/// Keys replace the data: bars and slices move from one year to the next and back while the
/// axes hold still.
fn case_29_changing_data() -> Value {
    let year = |values: [f64; 5]| json!([{ "name": "share", "values": values }]);
    let years = [
        [30.0, 22.0, 18.0, 16.0, 14.0],
        [26.0, 30.0, 14.0, 20.0, 10.0],
        [18.0, 34.0, 12.0, 28.0, 8.0],
    ];
    let keys: Vec<Value> = (1..=4)
        .map(|step| {
            let series = year(years[[1, 2, 1, 0][step - 1]]);
            json!({ "at": 1.5 * step as f64, "series": series, "ease": "in_out" })
        })
        .collect();
    let teams = json!(["Red", "Blue", "Gold", "Green", "Gray"]);
    json!({
        "repeat": "loop",
        "shapes": [
            {
                "kind": "chart", "x": 60, "y": 140, "width": 640, "height": 560,
                "label": "votes by team", "plot": "bar",
                "categories": teams,
                "series": year(years[0]),
                "keys": keys,
                "y_axis": { "label": "%" },
            },
            {
                "kind": "chart", "x": 740, "y": 140, "width": 480, "height": 560,
                "label": "the same as a donut", "plot": "donut",
                "categories": teams,
                "series": year(years[0]),
                "keys": keys,
            },
        ],
    })
}

/// Six weeks of daily prices: each day opens at the last close; green days close higher.
fn case_30_candlestick() -> Value {
    let days: Vec<String> = (1..=42)
        .map(|day| match day {
            ..=31 => format!("Mar {day}"),
            _ => format!("Apr {}", day - 31),
        })
        .collect();
    let mut close = 100.0;
    let candles: Vec<[f64; 4]> = (0..42)
        .map(|day| {
            let seed = f64::from(day);
            let open = close;
            close = round(open + 6.0 * (noise(seed) - 0.45));
            let high = round(open.max(close) + 2.0 * noise(seed + 0.3));
            let low = round(open.min(close) - 2.0 * noise(seed + 0.7));
            [open, high, low, close]
        })
        .collect();
    json!([
        {
            "kind": "chart", "x": 60, "y": 130, "width": 1160, "height": 600,
            "label": "XAI daily price", "plot": "candlestick",
            "categories": days,
            "series": [{ "points": candles }],
            "y_axis": { "label": "USD" },
        },
    ])
}

/// A month's money: where it comes from, the budget it pools in, and where it goes.
fn case_31_sankey() -> Value {
    let flows = [
        ("Salary", "Income", 5000),
        ("Freelance", "Income", 1200),
        ("Dividends", "Income", 300),
        ("Income", "Housing", 1800),
        ("Income", "Savings", 1500),
        ("Income", "Taxes", 1350),
        ("Income", "Food", 900),
        ("Income", "Fun", 500),
        ("Income", "Transport", 450),
        ("Housing", "Rent", 1500),
        ("Housing", "Utilities", 300),
        ("Savings", "Index funds", 1000),
        ("Savings", "Emergency fund", 500),
    ];
    let links: Vec<Value> = flows
        .into_iter()
        .map(|(from, to, value)| json!({ "from": from, "to": to, "value": value }))
        .collect();
    json!([
        {
            "kind": "chart", "x": 60, "y": 130, "width": 1160, "height": 620,
            "label": "monthly budget ($)", "plot": "sankey",
            "links": links,
        },
    ])
}

/// A repeatable number from 0 to 1 for `seed`, so the example data stays the same.
fn noise(seed: f64) -> f64 {
    ((seed * 12.9898).sin() * 43_758.545).rem_euclid(1.0)
}

/// `value` to three decimals, which keeps the printed JSON short.
fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn main() -> Result<()> {
    let this = std::env::current_exe()?;
    match std::env::var_os("GROK_SCREEN_OVERLAY") {
        // `screen_draw` started this binary as its overlay process.
        Some(path) if Path::new(&path) == this => {
            overlay::main();
            return Ok(());
        }
        Some(_) => {}
        // SAFETY: no other threads exist yet.
        None => unsafe { std::env::set_var("GROK_SCREEN_OVERLAY", &this) },
    }
    tokio::runtime::Runtime::new()?.block_on(cases())
}

async fn cases() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ctx = test_ctx(Resources::new().into_shared());
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["draw"] => draw(&ctx, 1..=CASES.len()).await,
        ["draw", case] => {
            let number: usize = case.parse().context("N must be a case number")?;
            ensure!(
                (1..=CASES.len()).contains(&number),
                "N must be 1 to {}",
                CASES.len()
            );
            draw(&ctx, number..=number).await
        }
        ["read", directory] => read(&ctx, Path::new(directory)).await,
        _ => bail!("Usage: screen draw [N] | screen read DIRECTORY"),
    }
}

/// Show the numbered cases one at a time, then clear the screen.
async fn draw(ctx: &ToolCallContext, numbers: RangeInclusive<usize>) -> Result<()> {
    for (number, (title, case)) in (1..).zip(CASES) {
        if !numbers.contains(&number) {
            continue;
        }
        let case = case();
        let heading = format!("Case {number} of {}: {title}", CASES.len());
        println!("\n{heading}\n{}", serde_json::to_string_pretty(&case)?);
        let caption = json!({ "kind": "text", "x": 55, "y": 60, "text": heading });
        let mut input = if case.is_array() {
            json!({ "shapes": case })
        } else {
            case
        };
        if let Some(shapes) = input["shapes"].as_array_mut() {
            shapes.insert(0, caption);
        }
        run(ctx, serde_json::from_value(input)?).await?;
        println!("Press Enter to continue.");
        std::io::stdin().read_line(&mut String::new())?;
    }
    run(ctx, ScreenDrawInput::default()).await
}

async fn run(ctx: &ToolCallContext, input: ScreenDrawInput) -> Result<()> {
    println!("{}", ScreenDrawTool.run(ctx.clone(), input).await?);
    Ok(())
}

async fn read(ctx: &ToolCallContext, directory: &Path) -> Result<()> {
    std::fs::create_dir_all(directory).context("Cannot create the image directory")?;
    let output = ScreenReadTool.run(ctx.clone(), ScreenReadInput {}).await?;
    for (index, window) in output.windows.iter().enumerate() {
        let extension = if window.image.mime_type == "image/jpeg" {
            "jpg"
        } else {
            "png"
        };
        let image = base64::engine::general_purpose::STANDARD.decode(&window.image.data)?;
        let path = directory.join(format!("window-{}.{extension}", index + 1));
        std::fs::write(&path, image).with_context(|| format!("Cannot save {}", path.display()))?;
    }
    println!("{}", output.prompt_text());
    Ok(())
}
