//! Read the active display and save its screenshot: `screen screenshot.png`.

use anyhow::{Context, Result, ensure};
use base64::Engine as _;
use xai_grok_tools::implementations::grok_build::screen::{ReadScreenInput, ReadScreenTool};
use xai_grok_tools::types::resources::Resources;
use xai_grok_tools::types::tool_metadata::test_ctx;
use xai_tool_runtime::Tool;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(args.len() == 1, "Usage: screen SCREENSHOT.png");
    let shared = Resources::new().into_shared();
    let output = ReadScreenTool
        .run(test_ctx(shared.clone()), ReadScreenInput {})
        .await?;
    let png = base64::engine::general_purpose::STANDARD.decode(&output.screenshot.image.data)?;
    std::fs::write(args.first().context("Missing screenshot path")?, png)
        .context("Cannot save screenshot")?;
    println!("{}", output.prompt_text());
    Ok(())
}
