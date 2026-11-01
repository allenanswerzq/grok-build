use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::json;
use xai_tool_runtime::{ContentBlock, Tool, ToolOutput as _};

use super::{
    AccessibilityTree, Annotation, DrawOverlayInput, DrawOverlayOutput, DrawOverlayTool, ModelPart,
    OverlayAction, OverlayBackend, OverlayRenderer, OverlayScene, OverlayState, Primitive,
    ReadScreenInput, ReadScreenOutput, ReadScreenTool, ScreenBackend, ScreenReader, Screenshot,
    Viewport,
};
use crate::types::output::{ImageContent, ToolOutput, ToolRunResult};
use crate::types::resources::Resources;
use crate::types::tool_metadata::test_ctx;

fn observation() -> ReadScreenOutput {
    ReadScreenOutput {
        observation_id: "observation-1".to_owned(),
        application: "test".to_owned(),
        target: "window-1".to_owned(),
        title: "Test window".to_owned(),
        viewport: Viewport {
            width: 100.0,
            height: 80.0,
        },
        accessibility: AccessibilityTree {
            text: "button Continue".to_owned(),
            node_count: 1,
            truncated: false,
        },
        screenshot: Screenshot {
            pixel_width: 200,
            pixel_height: 160,
            image: ImageContent {
                data: "vision-payload".to_owned(),
                mime_type: "image/png".to_owned(),
                annotations: None,
                uri: None,
                meta: None,
            },
        },
        coverage: vec!["test observation".to_owned()],
    }
}

struct FakeApplication(Arc<AtomicUsize>);

#[async_trait]
impl ScreenReader for FakeApplication {
    async fn read_screen(&self) -> Result<ReadScreenOutput, xai_tool_runtime::ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(observation())
    }
}

struct FakeRenderer(Arc<AtomicUsize>);
#[async_trait]
impl OverlayRenderer for FakeRenderer {
    async fn draw_overlay(
        &self,
        input: &DrawOverlayInput,
    ) -> Result<DrawOverlayOutput, xai_tool_runtime::ToolError> {
        assert!(matches!(input.action, OverlayAction::Clear));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(DrawOverlayOutput {
            output: "test-output".to_owned(),
            state: OverlayState::Cleared,
        })
    }
}

#[tokio::test]
async fn screen_reading_and_desktop_drawing_route_independently_without_chrome() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut resources = Resources::new();
    resources.insert(ScreenBackend::new(FakeApplication(calls.clone())));
    resources.insert(OverlayBackend::new(FakeRenderer(calls.clone())));
    let shared = resources.into_shared();
    let read = ReadScreenTool
        .run(test_ctx(shared.clone()), ReadScreenInput {})
        .await
        .unwrap();
    assert_eq!(read.application, "test");
    let draw = DrawOverlayTool
        .run(
            test_ctx(shared.clone()),
            DrawOverlayInput {
                output: None,
                action: OverlayAction::Clear,
            },
        )
        .await
        .unwrap();
    assert!(matches!(draw.state, OverlayState::Cleared));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn show_and_clear_wire_arguments_round_trip() {
    for value in [
        json!({"output":"eDP-1","action":"show","scene":{"kind":"annotations","items":[{"kind":"text","x":10.0,"y":20.0,"text":"<script>not code</script>","color":"#abcdef"}]}}),
        json!({"output":null,"action":"clear"}),
    ] {
        let input: DrawOverlayInput = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), value);
    }
    assert!(
        serde_json::from_value::<OverlayScene>(
            json!({"kind":"annotations","items":[],"script":"alert(1)"})
        )
        .is_err()
    );
    let read: ReadScreenInput = serde_json::from_value(json!({})).unwrap();
    assert_eq!(serde_json::to_value(read).unwrap(), json!({}));
    for value in [
        json!({"target":null}),
        json!({"application":"chrome"}),
        json!({"output":"eDP-1"}),
    ] {
        assert!(serde_json::from_value::<ReadScreenInput>(value).is_err());
    }
    let schema = serde_json::to_value(schemars::schema_for!(ReadScreenInput)).unwrap();
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        schema
            .get("properties")
            .is_none_or(|properties| properties.as_object().unwrap().is_empty())
    );
}

#[test]
fn overlay_validation_bounds_data_and_rejects_script_colors() {
    let rectangle = Annotation::Rectangle {
        x: 10.0,
        y: 20.0,
        width: 60.0,
        height: 40.0,
        color: "#00ff88".to_owned(),
    };
    assert!(
        OverlayScene::Annotations {
            items: vec![rectangle.clone()]
        }
        .validate()
        .is_ok()
    );
    assert!(
        OverlayScene::Annotations {
            items: vec![rectangle; 257]
        }
        .validate()
        .is_err()
    );
    assert!(
        OverlayScene::Annotations {
            items: vec![Annotation::Text {
                x: f64::NAN,
                y: 0.0,
                text: "x".to_owned(),
                color: "#ffffff".to_owned()
            }]
        }
        .validate()
        .is_err()
    );
    assert!(
        OverlayScene::Annotations {
            items: vec![Annotation::Text {
                x: 1.0,
                y: 0.0,
                text: "x".to_owned(),
                color: "url(https://example.com)".to_owned()
            }]
        }
        .validate()
        .is_err()
    );
    let part = ModelPart {
        primitive: Primitive::Sphere,
        position: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0; 3],
        color: "#88aaff".to_owned(),
    };
    assert!(
        OverlayScene::Model3d {
            parts: vec![part.clone()]
        }
        .validate()
        .is_ok()
    );
    assert!(
        OverlayScene::Model3d {
            parts: vec![ModelPart {
                scale: [1.0, -1.0, 1.0],
                ..part
            }]
        }
        .validate()
        .is_err()
    );
}

#[test]
fn screenshot_is_vision_content_after_tool_runner_conversion() {
    let output = observation();
    assert!(
        matches!(&output.model_output()[1], ContentBlock::Image { data, .. } if data == "vision-payload")
    );
    let output: ToolOutput = output.into();
    let prompt_text = format!(
        "{}\nSystem reminder from the tool pipeline",
        output.to_prompt_format()
    );
    assert!(!prompt_text.contains("vision-payload"));
    let result = ToolRunResult {
        output,
        prompt_text,
        effective_tool_name: None,
    };
    let typed = result.into_typed_tool_output(ReadScreenTool.id());
    assert!(typed.model_output.iter().any(
        |block| matches!(block, ContentBlock::Image { data, .. } if data == "vision-payload")
    ));
    assert!(typed.model_output.iter().any(
        |block| matches!(block, ContentBlock::Text { text } if text.contains("button Continue") && text.ends_with("System reminder from the tool pipeline"))
    ));
}

#[test]
fn both_tools_are_registered_with_typed_inputs_and_permission_categories() {
    use crate::registry::types::ToolRegistryBuilder;
    use crate::types::{ToolInput, tool::ToolKind};
    use xai_grok_permission_rules::types::AccessKind;
    let registry = ToolRegistryBuilder::new();
    let kinds = registry.known_tool_kinds();
    assert_eq!(kinds.get("GrokBuild:read_screen"), Some(&ToolKind::Read));
    assert_eq!(kinds.get("GrokBuild:draw_overlay"), Some(&ToolKind::Other));
    let read: ToolInput = ReadScreenInput {}.into();
    assert!(matches!(AccessKind::from(&read), AccessKind::Read(None)));
    let draw: ToolInput = DrawOverlayInput {
        output: None,
        action: OverlayAction::Clear,
    }
    .into();
    assert!(matches!(AccessKind::from(&draw), AccessKind::Tool(name) if name == "draw_overlay"));
    assert!(ReadScreenTool.capabilities().is_read_only);
    assert!(!DrawOverlayTool.capabilities().is_read_only);
}
