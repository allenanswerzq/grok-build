//! Show a standalone scene: `overlay SCENE.json [OUTPUT]`. Enter clears and exits.

use anyhow::{Context, Result, ensure};
use xai_grok_tools::implementations::grok_build::screen::{
    DrawOverlayInput, DrawOverlayTool, OverlayAction, OverlayScene,
};
use xai_grok_tools::types::resources::Resources;
use xai_grok_tools::types::tool_metadata::test_ctx;
use xai_tool_runtime::Tool;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        (1..=2).contains(&args.len()),
        "Usage: overlay SCENE.json [OUTPUT]"
    );
    let scene: OverlayScene =
        serde_json::from_slice(&std::fs::read(args.first().context("Missing scene")?)?)?;
    let output = args
        .get(1)
        .map(|name| {
            name.to_str()
                .context("Output name must be UTF-8")
                .map(str::to_owned)
        })
        .transpose()?;
    let resources = Resources::new().into_shared();
    let shown = DrawOverlayTool
        .run(
            test_ctx(resources.clone()),
            DrawOverlayInput {
                output,
                action: OverlayAction::Show { scene },
            },
        )
        .await?;
    println!(
        "Overlay shown on {}. Press Enter to clear and exit.",
        shown.output
    );
    tokio::task::spawn_blocking(|| std::io::stdin().read_line(&mut String::new())).await??;
    DrawOverlayTool
        .run(
            test_ctx(resources),
            DrawOverlayInput {
                output: None,
                action: OverlayAction::Clear,
            },
        )
        .await?;
    Ok(())
}
