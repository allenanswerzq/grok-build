//! Check the screen tools on this desktop.
//!
//! - `screen draw` shows every drawing case in turn; press Enter for the next one.
//! - `screen draw N` shows only case N.
//! - `screen read DIRECTORY` reads the visible windows and saves one image per window.
//!
//! Each case is the exact `shapes` list a model would send to `screen_draw`. It is printed
//! in the terminal while it is on screen; only the caption at the top is added. The cases
//! fit a monitor of 1280 × 800 logical units.
//!
//! ```sh
//! just build
//! just build xai-grok-tools --example screen
//! ./target/debug/examples/screen draw
//! ```

use std::ops::RangeInclusive;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use xai_grok_tools::implementations::grok_build::screen_draw::{ScreenDrawInput, ScreenDrawTool};
use xai_grok_tools::implementations::grok_build::screen_read::{ScreenReadInput, ScreenReadTool};
use xai_grok_tools::types::resources::Resources;
use xai_grok_tools::types::tool_metadata::test_ctx;
use xai_tool_runtime::{Tool, ToolCallContext};

const CASES: [(&str, fn() -> Value); 11] = [
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
];

/// Every theme color. A box's label sits just above it.
fn case_1_box_colors() -> Value {
    json!([
        { "kind": "box", "x": 55, "y": 200, "width": 210, "height": 160, "label": "accent", "color": "accent" },
        { "kind": "box", "x": 295, "y": 200, "width": 210, "height": 160, "label": "red", "color": "red" },
        { "kind": "box", "x": 535, "y": 200, "width": 210, "height": 160, "label": "yellow", "color": "yellow" },
        { "kind": "box", "x": 775, "y": 200, "width": 210, "height": 160, "label": "green", "color": "green" },
        { "kind": "box", "x": 1015, "y": 200, "width": 210, "height": 160, "label": "blue", "color": "blue" },
    ])
}

/// Fill is an opacity from 0 to 1; the default is a faint 0.08. The border is optional.
fn case_2_box_fill_and_border() -> Value {
    json!([
        { "kind": "box", "x": 55, "y": 200, "width": 210, "height": 160, "label": "default" },
        { "kind": "box", "x": 295, "y": 200, "width": 210, "height": 160, "label": "fill 0.4", "color": "red", "fill": 0.4 },
        { "kind": "box", "x": 535, "y": 200, "width": 210, "height": 160, "label": "fill 1", "color": "yellow", "fill": 1 },
        { "kind": "box", "x": 775, "y": 200, "width": 210, "height": 160, "label": "fill 0", "color": "green", "fill": 0 },
        { "kind": "box", "x": 1015, "y": 200, "width": 210, "height": 160, "label": "no border", "color": "blue", "fill": 0.4, "border": false },
    ])
}

/// A line runs from (x, y) to (to_x, to_y). The top row shows each arrows choice, the
/// middle row each pattern, and the star arrows pointing out in eight directions.
fn case_3_lines_and_arrows() -> Value {
    json!([
        { "kind": "text", "x": 55, "y": 170, "text": "arrows: none", "fill": 0, "border": false },
        { "kind": "line", "x": 55, "y": 230, "to_x": 325, "to_y": 230 },
        { "kind": "text", "x": 355, "y": 170, "text": "arrows: start", "color": "red", "fill": 0, "border": false },
        { "kind": "line", "x": 355, "y": 230, "to_x": 625, "to_y": 230, "color": "red", "arrows": "start" },
        { "kind": "text", "x": 655, "y": 170, "text": "arrows: end", "color": "yellow", "fill": 0, "border": false },
        { "kind": "line", "x": 655, "y": 230, "to_x": 925, "to_y": 230, "color": "yellow", "arrows": "end" },
        { "kind": "text", "x": 955, "y": 170, "text": "arrows: both", "color": "green", "fill": 0, "border": false },
        { "kind": "line", "x": 955, "y": 230, "to_x": 1225, "to_y": 230, "color": "green", "arrows": "both" },
        { "kind": "text", "x": 55, "y": 290, "text": "pattern: solid", "fill": 0, "border": false },
        { "kind": "line", "x": 55, "y": 350, "to_x": 325, "to_y": 350 },
        { "kind": "text", "x": 355, "y": 290, "text": "pattern: dashed", "color": "red", "fill": 0, "border": false },
        { "kind": "line", "x": 355, "y": 350, "to_x": 625, "to_y": 350, "color": "red", "pattern": "dashed" },
        { "kind": "text", "x": 655, "y": 290, "text": "pattern: dotted", "color": "yellow", "fill": 0, "border": false },
        { "kind": "line", "x": 655, "y": 350, "to_x": 925, "to_y": 350, "color": "yellow", "pattern": "dotted" },
        { "kind": "text", "x": 955, "y": 290, "text": "dashed, both arrows", "color": "blue", "fill": 0, "border": false },
        { "kind": "line", "x": 955, "y": 350, "to_x": 1225, "to_y": 350, "color": "blue", "pattern": "dashed", "arrows": "both" },
        { "kind": "line", "x": 670, "y": 600, "to_x": 790, "to_y": 600, "arrows": "end" },
        { "kind": "line", "x": 661, "y": 621, "to_x": 746, "to_y": 706, "color": "red", "arrows": "end" },
        { "kind": "line", "x": 640, "y": 630, "to_x": 640, "to_y": 750, "color": "yellow", "arrows": "end" },
        { "kind": "line", "x": 619, "y": 621, "to_x": 534, "to_y": 706, "color": "green", "arrows": "end", "pattern": "dotted" },
        { "kind": "line", "x": 610, "y": 600, "to_x": 490, "to_y": 600, "color": "blue", "arrows": "end", "pattern": "dashed" },
        { "kind": "line", "x": 619, "y": 579, "to_x": 534, "to_y": 494, "arrows": "end", "pattern": "dotted" },
        { "kind": "line", "x": 640, "y": 570, "to_x": 640, "to_y": 450, "color": "red", "arrows": "end", "pattern": "dashed" },
        { "kind": "line", "x": 661, "y": 579, "to_x": 746, "to_y": 494, "color": "yellow", "arrows": "both" },
    ])
}

/// Boxes without labels and lines, from thin to thick. The default follows the window
/// border; lines default to 1.5 times it.
fn case_4_line_widths() -> Value {
    json!([
        { "kind": "box", "x": 55, "y": 200, "width": 270, "height": 160, "line_width": 1 },
        { "kind": "box", "x": 355, "y": 200, "width": 270, "height": 160, "color": "red", "line_width": 3 },
        { "kind": "box", "x": 655, "y": 200, "width": 270, "height": 160, "color": "green", "line_width": 6 },
        { "kind": "box", "x": 955, "y": 200, "width": 270, "height": 160, "color": "blue", "line_width": 10 },
        { "kind": "text", "x": 55, "y": 420, "text": "line_width 1", "fill": 0, "border": false },
        { "kind": "text", "x": 355, "y": 420, "text": "line_width 3", "color": "red", "fill": 0, "border": false },
        { "kind": "text", "x": 655, "y": 420, "text": "line_width 6", "color": "green", "fill": 0, "border": false },
        { "kind": "text", "x": 955, "y": 420, "text": "line_width 10", "color": "blue", "fill": 0, "border": false },
        { "kind": "line", "x": 55, "y": 520, "to_x": 325, "to_y": 520, "line_width": 1, "arrows": "end" },
        { "kind": "line", "x": 355, "y": 520, "to_x": 625, "to_y": 520, "color": "red", "line_width": 3, "arrows": "end" },
        { "kind": "line", "x": 655, "y": 520, "to_x": 925, "to_y": 520, "color": "green", "line_width": 6, "arrows": "end" },
        { "kind": "line", "x": 955, "y": 520, "to_x": 1225, "to_y": 520, "color": "blue", "line_width": 10, "arrows": "end" },
    ])
}

/// Text starts at its top-left corner and may span lines. Its color outlines the panel.
fn case_5_text_colors_and_lines() -> Value {
    json!([
        { "kind": "text", "x": 55, "y": 200, "text": "Default text uses the accent color" },
        { "kind": "text", "x": 55, "y": 260, "text": "Red text", "color": "red" },
        { "kind": "text", "x": 55, "y": 320, "text": "Yellow text", "color": "yellow" },
        { "kind": "text", "x": 55, "y": 380, "text": "Green text", "color": "green" },
        { "kind": "text", "x": 55, "y": 440, "text": "Blue text", "color": "blue" },
        { "kind": "text", "x": 640, "y": 200, "text": "Text may span lines.\nEvery line starts at the same x,\nso short notes stay tidy." },
        { "kind": "text", "x": 640, "y": 340, "text": "Long text wraps at 30 times its size, so this sentence breaks onto a second line." },
        { "kind": "text", "x": 640, "y": 440, "text": "Symbols: → ← ↑ ↓ ✓ ✗ ± ° ×", "color": "yellow" },
    ])
}

/// Size is in logical units; the default is 14. The panel takes a fill and border like a
/// box, and text without a border shows its words in its color.
fn case_6_text_size_fill_and_border() -> Value {
    json!([
        { "kind": "text", "x": 55, "y": 200, "text": "size 10", "size": 10 },
        { "kind": "text", "x": 55, "y": 250, "text": "size 14, the default" },
        { "kind": "text", "x": 55, "y": 310, "text": "size 24", "size": 24 },
        { "kind": "text", "x": 55, "y": 390, "text": "size 40", "size": 40 },
        { "kind": "text", "x": 640, "y": 200, "text": "fill 0.4: windows show through", "color": "red", "fill": 0.4 },
        { "kind": "text", "x": 640, "y": 280, "text": "fill 1: a solid panel", "color": "green", "fill": 1 },
        { "kind": "text", "x": 640, "y": 360, "text": "no border: the words take the color", "color": "yellow", "border": false },
        { "kind": "text", "x": 640, "y": 440, "text": "fill 0, no border: bare words", "color": "blue", "fill": 0, "border": false },
    ])
}

/// Each kind of scene object, lit and fitted to its frame.
fn case_7_scene_objects() -> Value {
    json!([
        { "kind": "scene", "x": 55, "y": 200, "width": 210, "height": 260, "label": "cuboid", "objects": [
            { "kind": "cuboid", "center": [0, 0, 0], "size": [2, 1, 1.4] },
        ] },
        { "kind": "scene", "x": 295, "y": 200, "width": 210, "height": 260, "label": "sphere", "objects": [
            { "kind": "sphere", "center": [0, 0, 0], "radius": 1, "color": "red" },
        ] },
        { "kind": "scene", "x": 535, "y": 200, "width": 210, "height": 260, "label": "cylinder", "objects": [
            { "kind": "cylinder", "from": [0, -1, 0], "to": [0, 1, 0], "radius": 0.5, "color": "yellow" },
        ] },
        { "kind": "scene", "x": 775, "y": 200, "width": 210, "height": 260, "label": "lines: x, y, z", "objects": [
            { "kind": "line", "from": [0, 0, 0], "to": [1, 0, 0], "color": "red" },
            { "kind": "line", "from": [0, 0, 0], "to": [0, 1, 0], "color": "green" },
            { "kind": "line", "from": [0, 0, 0], "to": [0, 0, 1], "color": "blue" },
        ] },
        { "kind": "scene", "x": 1015, "y": 200, "width": 210, "height": 260, "label": "mesh", "objects": [
            {
                "kind": "mesh",
                "color": "blue",
                "vertices": [[-1, 0, -1], [1, 0, -1], [1, 0, 1], [-1, 0, 1], [0, 1.5, 0]],
                "faces": [[3, 2, 4], [2, 1, 4], [1, 0, 4], [0, 3, 4], [0, 1, 2], [0, 2, 3]],
            },
        ] },
    ])
}

/// One scene from five cameras. The default camera is yaw 35, pitch 25. Drag any scene to
/// orbit it, and scroll over it to zoom.
fn case_8_scene_cameras() -> Value {
    json!([
        { "kind": "scene", "x": 55, "y": 200, "width": 210, "height": 280, "label": "front: 0, 0",
          "camera": { "yaw": 0, "pitch": 0 }, "objects": robot_arm() },
        { "kind": "scene", "x": 295, "y": 200, "width": 210, "height": 280, "label": "default: 35, 25",
          "objects": robot_arm() },
        { "kind": "scene", "x": 535, "y": 200, "width": 210, "height": 280, "label": "side: 90, 0",
          "camera": { "yaw": 90, "pitch": 0 }, "objects": robot_arm() },
        { "kind": "scene", "x": 775, "y": 200, "width": 210, "height": 280, "label": "top: 0, 90",
          "camera": { "yaw": 0, "pitch": 90 }, "objects": robot_arm() },
        { "kind": "scene", "x": 1015, "y": 200, "width": 210, "height": 280, "label": "below: 35, -30",
          "camera": { "yaw": 35, "pitch": -30 }, "objects": robot_arm() },
    ])
}

/// The backdrop is the theme background at the fill opacity; the default is 0.92. Line
/// width sets the frame and the line objects. A scene needs no label.
fn case_9_scene_fill_border_and_line_width() -> Value {
    json!([
        { "kind": "scene", "x": 55, "y": 200, "width": 270, "height": 320, "label": "default",
          "objects": robot_arm() },
        { "kind": "scene", "x": 355, "y": 200, "width": 270, "height": 320, "label": "fill 1",
          "fill": 1, "objects": robot_arm() },
        { "kind": "scene", "x": 655, "y": 200, "width": 270, "height": 320, "label": "fill 0.4, line_width 5",
          "fill": 0.4, "line_width": 5, "objects": robot_arm() },
        { "kind": "scene", "x": 955, "y": 200, "width": 270, "height": 320,
          "fill": 0, "border": false, "objects": robot_arm() },
    ])
}

/// Boxes, lines, and scenes draw in list order, so later ones cover earlier ones. Text
/// and labels draw last, so words stay on top even when listed first.
fn case_10_layering() -> Value {
    json!([
        { "kind": "text", "x": 420, "y": 360, "text": "Listed first, drawn last", "size": 20 },
        { "kind": "box", "x": 200, "y": 200, "width": 420, "height": 260, "label": "first", "color": "red", "fill": 1 },
        { "kind": "box", "x": 460, "y": 330, "width": 420, "height": 260, "label": "second: covers first", "color": "blue", "fill": 1 },
        { "kind": "line", "x": 150, "y": 650, "to_x": 1000, "to_y": 250, "color": "yellow", "line_width": 6, "arrows": "end" },
    ])
}

/// A scene with a callout: a box, text inside it, and a dashed arrow into the scene.
fn case_11_together() -> Value {
    json!([
        { "kind": "scene", "x": 55, "y": 150, "width": 700, "height": 580, "label": "house", "objects": [
            { "kind": "cuboid", "center": [0, 0.5, 0], "size": [2, 1, 2], "color": "blue" },
            {
                "kind": "mesh",
                "color": "red",
                "vertices": [[-1.2, 1, -1.2], [1.2, 1, -1.2], [1.2, 1, 1.2], [-1.2, 1, 1.2], [0, 2, 0]],
                "faces": [[3, 2, 4], [2, 1, 4], [1, 0, 4], [0, 3, 4], [0, 1, 2], [0, 2, 3]],
            },
            { "kind": "cuboid", "center": [0.6, 1.7, -0.4], "size": [0.25, 0.6, 0.25], "color": "yellow" },
            { "kind": "cylinder", "from": [2, 0, 0.8], "to": [2, 0.9, 0.8], "radius": 0.12, "color": "yellow" },
            { "kind": "sphere", "center": [2, 1.3, 0.8], "radius": 0.5, "color": "green" },
            { "kind": "line", "from": [-1.8, 0, 1.8], "to": [2.8, 0, 1.8] },
        ] },
        { "kind": "box", "x": 840, "y": 300, "width": 385, "height": 110, "label": "callout", "color": "yellow" },
        { "kind": "text", "x": 860, "y": 320, "text": "Boxes, text, and arrows\ncan point into a scene.", "color": "yellow" },
        { "kind": "line", "x": 840, "y": 355, "to_x": 640, "to_y": 420, "color": "yellow", "arrows": "end", "pattern": "dashed" },
    ])
}

/// A base, a mast, a joint, an angled arm, and a plumb line.
fn robot_arm() -> Value {
    json!([
        { "kind": "cuboid", "center": [0, 0, 0], "size": [2.4, 0.4, 1.4], "color": "blue" },
        { "kind": "cylinder", "from": [0, 0.2, 0], "to": [0, 1.8, 0], "radius": 0.16 },
        { "kind": "sphere", "center": [0, 1.95, 0], "radius": 0.3, "color": "red" },
        { "kind": "cylinder", "from": [0, 1.95, 0], "to": [1.3, 2.6, 0], "radius": 0.12, "color": "yellow" },
        { "kind": "line", "from": [1.3, 2.6, 0], "to": [1.3, 0.2, 0], "color": "green" },
    ])
}

#[tokio::main]
async fn main() -> Result<()> {
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
        let shapes = case();
        let heading = format!("Case {number} of {}: {title}", CASES.len());
        println!("\n{heading}\n{}", serde_json::to_string_pretty(&shapes)?);
        let caption = json!({ "kind": "text", "x": 55, "y": 60, "text": heading });
        let mut on_screen = vec![caption];
        on_screen.extend(shapes.as_array().into_iter().flatten().cloned());
        run(
            ctx,
            ScreenDrawInput {
                shapes: serde_json::from_value(on_screen.into())?,
            },
        )
        .await?;
        println!("Press Enter to continue.");
        std::io::stdin().read_line(&mut String::new())?;
    }
    run(ctx, ScreenDrawInput { shapes: Vec::new() }).await
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
