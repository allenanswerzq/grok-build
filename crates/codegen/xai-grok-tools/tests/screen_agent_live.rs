#![cfg(all(feature = "live-tests", target_os = "linux"))]

//! Runs the real CLI agent against mock inference, managed Chrome, and Wayland.
//! The browser fixture and agent home are private and removed after the test.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use xai_grok_test_support::sse;
use xai_grok_test_support::{
    DEFAULT_MODEL, InferenceEndpoint, InferenceRequestMatcher, MockInferenceServer,
    ScriptedResponse, TestSandbox, run_headless_in_sandbox_borrowed_with_env,
};

async fn command(websocket: &str, method: &str, params: Value) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut socket, _) = tokio_tungstenite::connect_async(websocket).await?;
        socket
            .send(Message::Text(
                json!({"id":1,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await?;
        while let Some(message) = socket.next().await {
            if let Message::Text(text) = message? {
                let value: Value = serde_json::from_str(&text)?;
                if value["id"] == 1 {
                    ensure!(value.get("error").is_none(), "Fixture CDP error: {value}");
                    return Ok(value["result"].clone());
                }
            }
        }
        anyhow::bail!("Fixture CDP disconnected")
    })
    .await?
}

async fn exercise() -> Result<()> {
    let server = MockInferenceServer::start().await?;
    server.preset_allow_access();
    let matcher = InferenceRequestMatcher::foreground(InferenceEndpoint::ChatCompletions);
    let calls = [
        ("read", "screen_read", json!({})),
        ("read_again", "screen_read", json!({})),
        (
            "draw",
            "screen_draw",
            json!({"shapes":[{"kind":"box","x":10,"y":10,"width":100,"height":60,"label":"Here"}]}),
        ),
        ("clear", "screen_draw", json!({"shapes":[]})),
    ];
    let mut expectations = Vec::new();
    for (id, name, arguments) in calls {
        expectations.push(server.expect_response(
            id,
            matcher,
            ScriptedResponse::sse(sse::chat_completions_reasoning_then_tool_call_events(
                "Exercise the screen tool path.",
                id,
                name,
                &arguments.to_string(),
                DEFAULT_MODEL,
            )),
        ));
    }
    expectations.push(server.expect_response(
        "finish",
        matcher,
        ScriptedResponse::sse(sse::chat_completion_script_exact(
            "SCREEN_AGENT_E2E_COMPLETE",
            DEFAULT_MODEL,
        )),
    ));

    let sandbox = TestSandbox::builder().mock_url(server.url()).build();
    let binary = std::env::var_os("GROK_BINARY").context("Set GROK_BINARY to the built CLI")?;
    ensure!(
        Path::new(&binary).is_absolute(),
        "GROK_BINARY must be absolute"
    );
    let mut cli = tokio::process::Command::new(binary);
    cli.current_dir(sandbox.workspace()).args([
        "--no-auto-update",
        "--model",
        DEFAULT_MODEL,
        "--yolo",
        "--max-turns",
        "8",
        "-p",
        "Read the screen, mark a spot, clear the drawing, and report completion.",
    ]);
    // TestSandbox clears the real environment. Give only this test's child access
    // to the existing desktop sockets and the freshly built screen overlay.
    let mut environment = Vec::new();
    for name in [
        "XDG_RUNTIME_DIR",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "GROK_SCREEN_OVERLAY",
    ] {
        if let Ok(value) = std::env::var(name) {
            environment.push((name, value));
        }
    }
    let overrides: Vec<(&str, &str)> = environment
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let result = run_headless_in_sandbox_borrowed_with_env(cli, &sandbox, &overrides).await;
    ensure!(
        !result.timed_out && result.status.success(),
        "Agent failed with {:?} (timeout={}): {}\n{}",
        result.status.code(),
        result.timed_out,
        result.stdout,
        result.stderr
    );
    ensure!(
        result.stdout.contains("SCREEN_AGENT_E2E_COMPLETE"),
        "Final response missing: {}\n{}",
        result.stdout,
        result.stderr
    );
    for expectation in expectations {
        ensure!(
            expectation.is_satisfied(),
            "{} was not executed",
            expectation.name()
        );
    }
    let requests = server.request_bodies();
    let tool_messages = requests
        .iter()
        .filter_map(|request| request.get("messages").and_then(Value::as_array))
        .flatten()
        .filter(|message| message["role"] == "tool");
    let mut returned = Vec::new();
    for message in tool_messages {
        match &message["content"] {
            Value::String(text) => returned.push(text.as_str()),
            Value::Array(blocks) => {
                for block in blocks {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        returned.push(text);
                    }
                }
            }
            _ => {}
        }
    }
    let transcript = serde_json::to_string(&requests)?;
    let errors: BTreeSet<&str> = returned
        .iter()
        .copied()
        .filter(|text| text.starts_with("Tool `") && text.contains(" failed:"))
        .collect();
    ensure!(errors.is_empty(), "Agent received tool errors: {errors:?}");
    let observations: BTreeSet<&str> = returned
        .iter()
        .filter_map(|text| text.strip_prefix("Observation ")?.split_whitespace().next())
        .collect();
    ensure!(
        observations.len() == 2,
        "Expected both reads to deliver distinct observations; got {observations:?}"
    );
    if returned.iter().any(|text| text.contains("Window 1 of ")) {
        ensure!(
            transcript.contains("data:image/"),
            "Window images did not reach inference"
        );
    }
    for expected in ["Drew 1 shape on monitor ", "Cleared the drawing."] {
        ensure!(
            returned.iter().any(|text| text.starts_with(expected)),
            "screen_draw result {expected:?} did not reach inference"
        );
    }
    Ok(())
}

#[tokio::test]
async fn agent_reads_the_screen_then_draws_and_clears() -> Result<()> {
    // Optional Omarchy check: this test also runs on a plain Hyprland session.
    let lock = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("omarchy-shell")
            .args(["lock", "isLocked"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    if let Ok(Ok(lock)) = lock {
        ensure!(
            !lock.status.success() || String::from_utf8_lossy(&lock.stdout).trim() != "true",
            "Live desktop test requires an unlocked session; unlock it before running this test"
        );
    }
    let monitors = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("hyprctl")
            .args(["-j", "monitors"])
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        monitors.status.success(),
        "Live test requires a running Hyprland session"
    );
    let monitors: Vec<Value> = serde_json::from_slice(&monitors.stdout)?;
    ensure!(
        monitors.iter().any(|monitor| monitor["dpmsStatus"] == true),
        "Live desktop test requires a powered-on display; wake it and keep it awake for the test"
    );
    #[expect(
        clippy::disallowed_methods,
        reason = "Fixture connects only to loopback HTTP"
    )]
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let version: Value = client
        .get("http://127.0.0.1:9222/json/version")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let browser = version["webSocketDebuggerUrl"]
        .as_str()
        .context("Missing managed Chrome WebSocket")?;
    let created = command(browser, "Target.createBrowserContext", json!({})).await?;
    let context = created["browserContextId"]
        .as_str()
        .context("Missing context")?;
    let result = async {
        let created = command(browser, "Target.createTarget", json!({
            "url":"data:text/html,<!doctype html><title>Screen agent fixture</title><h1>Screen agent fixture</h1><button>Continue</button>",
            "browserContextId":context,"background":false
        })).await?;
        created["targetId"].as_str().context("Missing target")?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        exercise().await
    }.await;
    let cleanup = command(
        browser,
        "Target.disposeBrowserContext",
        json!({"browserContextId":context}),
    )
    .await;
    result?;
    cleanup?;
    Ok(())
}
