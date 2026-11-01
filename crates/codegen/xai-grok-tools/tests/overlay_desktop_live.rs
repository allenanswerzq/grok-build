#![cfg(all(feature = "live-tests", target_os = "linux"))]

//! Opt-in native desktop test. Uses only its own overlay; Chrome is not involved.
use anyhow::{Result, ensure};
use serde_json::Value;
use xai_grok_tools::implementations::grok_build::screen::{
    DrawOverlayInput, DrawOverlayTool, ModelPart, OverlayAction, OverlayScene, OverlayState,
    Primitive,
};
use xai_grok_tools::types::resources::Resources;
use xai_grok_tools::types::tool_metadata::test_ctx;
use xai_tool_runtime::Tool;

async fn own_layer_present() -> Result<bool> {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::process::Command::new("hyprctl")
            .args(["-j", "layers"])
            .output(),
    )
    .await??;
    ensure!(result.status.success(), "Cannot inspect compositor layers");
    let layers: Value = serde_json::from_slice(&result.stdout)?;
    Ok(layers
        .as_object()
        .into_iter()
        .flat_map(|outputs| outputs.values())
        .filter_map(|output| output.get("levels").and_then(Value::as_object))
        .filter_map(|levels| levels.get("3").and_then(Value::as_array))
        .flatten()
        .any(|layer| {
            if layer.get("namespace").and_then(Value::as_str) != Some("grok-overlay") {
                return false;
            }
            let Some(pid) = layer.get("pid").and_then(Value::as_u64) else {
                return false;
            };
            // Another agent session's overlay cannot satisfy this assertion.
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                return false;
            };
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(1))
                .and_then(|parent| parent.parse::<u32>().ok())
                == Some(std::process::id())
        }))
}

#[tokio::test]
async fn tool_draws_an_independent_desktop_surface() -> Result<()> {
    let resources = Resources::new().into_shared();
    let shown = DrawOverlayTool
        .run(
            test_ctx(resources.clone()),
            DrawOverlayInput {
                output: None,
                action: OverlayAction::Show {
                    scene: OverlayScene::Model3d {
                        parts: vec![ModelPart {
                            primitive: Primitive::Sphere,
                            position: [0.0; 3],
                            rotation: [0.0; 3],
                            scale: [2.0; 3],
                            color: "#ff3366".to_owned(),
                        }],
                    },
                },
            },
        )
        .await?;
    ensure!(
        matches!(shown.state, OverlayState::Visible),
        "Show did not return Visible"
    );
    ensure!(
        !shown.output.is_empty(),
        "No actual desktop output reported"
    );
    ensure!(
        own_layer_present().await?,
        "Native surface is missing from the compositor's overlay layer"
    );
    for action in [
        OverlayAction::Show {
            scene: OverlayScene::Model3d {
                parts: vec![ModelPart {
                    primitive: Primitive::Sphere,
                    position: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                    color: "#ffffff".to_owned(),
                }],
            },
        },
        OverlayAction::Clear,
    ] {
        let result = DrawOverlayTool
            .run(
                test_ctx(resources.clone()),
                DrawOverlayInput {
                    output: Some("grok-test-nonexistent-output".to_owned()),
                    action,
                },
            )
            .await;
        ensure!(result.is_err(), "Unknown output was accepted");
        ensure!(
            own_layer_present().await?,
            "Rejected request destroyed this session's existing overlay"
        );
    }
    let cleared = DrawOverlayTool
        .run(
            test_ctx(resources.clone()),
            DrawOverlayInput {
                output: None,
                action: OverlayAction::Clear,
            },
        )
        .await?;
    ensure!(
        matches!(cleared.state, OverlayState::Cleared),
        "Clear failed"
    );
    for _ in 0..20 {
        if !own_layer_present().await? {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    ensure!(false, "Clear left this session's surface mapped");
    Ok(())
}
