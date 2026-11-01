#![cfg(feature = "live-tests")]

//! Opt-in test against managed Chrome on 127.0.0.1:9222. Uses and closes its own
//! synthetic private browser context; does not modify user profiles or existing pages.

use anyhow::{Context, Result, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use xai_grok_tools::implementations::grok_build::screen::chrome::ChromeApplication;

// Creating/disposing contexts can change Chrome focus. Keep fixtures from
// invalidating each other's otherwise-static capture checks.
static CHROME_FIXTURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn command(websocket: &str, method: &str, params: Value) -> Result<Value> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        command_inner(websocket, method, params),
    )
    .await?
}

async fn command_inner(websocket: &str, method: &str, params: Value) -> Result<Value> {
    let (mut socket, _) = tokio_tungstenite::connect_async(websocket).await?;
    command_on_socket(&mut socket, method, params).await
}

async fn command_on_socket(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    method: &str,
    params: Value,
) -> Result<Value> {
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
            if value["id"] != 1 {
                continue;
            }
            ensure!(value.get("error").is_none(), "CDP error: {value}");
            return Ok(value["result"].clone());
        }
    }
    anyhow::bail!("CDP disconnected")
}

struct Fixture {
    browser: String,
    context: String,
    target: String,
    websocket: String,
}

impl Fixture {
    async fn new() -> Result<Self> {
        #[expect(
            clippy::disallowed_methods,
            reason = "Fixture connects only to loopback HTTP"
        )]
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        let version: Value = client
            .get("http://127.0.0.1:9222/json/version")
            .send()
            .await
            .context("Managed Chrome must be running on 127.0.0.1:9222")?
            .error_for_status()?
            .json()
            .await?;
        let browser = version["webSocketDebuggerUrl"]
            .as_str()
            .context("Missing browser WebSocket")?
            .to_owned();
        let mut page_url = url::Url::parse(&browser)?;
        let response = command(&browser, "Target.createBrowserContext", json!({})).await?;
        let context = response["browserContextId"]
            .as_str()
            .context("Missing private context")?
            .to_owned();
        let result = async {
            let response = command(
                &browser,
                "Target.createTarget",
                json!({"url":"about:blank","browserContextId":context,"background":true}),
            )
            .await?;
            let target = response["targetId"]
                .as_str()
                .context("Missing fixture target")?
                .to_owned();
            page_url.set_path(&format!("/devtools/page/{target}"));
            Ok::<_, anyhow::Error>((target, page_url.to_string()))
        }
        .await;
        match result {
            Ok((target, websocket)) => Ok(Self {
                browser,
                context,
                target,
                websocket,
            }),
            Err(error) => {
                command(
                    &browser,
                    "Target.disposeBrowserContext",
                    json!({"browserContextId":context}),
                )
                .await?;
                Err(error)
            }
        }
    }

    async fn close(&self) -> Result<()> {
        command(
            &self.browser,
            "Target.disposeBrowserContext",
            json!({"browserContextId":self.context}),
        )
        .await?;
        Ok(())
    }
}

async fn evaluate(websocket: &str, expression: &str) -> Result<Value> {
    let result = command(
        websocket,
        "Runtime.evaluate",
        json!({"expression":expression,"returnByValue":true,"awaitPromise":true}),
    )
    .await?;
    ensure!(
        result.get("exceptionDetails").is_none(),
        "Evaluation failed: {result}"
    );
    Ok(result["result"]["value"].clone())
}

async fn exercise(target: &str, websocket: &str) -> Result<()> {
    evaluate(websocket, r#"document.open(); document.write('<!doctype html><title>Grok screen fixture</title><style>html{scrollbar-width:none}body{margin:20px;font:20px sans-serif;background:white!important;color:#202020!important;color-scheme:light}main{height:2000px}</style><main><h1>Screen fixture</h1><button>Continue</button><input aria-label="Name"></main>'); document.close(); true"#).await?;
    evaluate(websocket, "new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve(true))))").await?;
    // Allow Chrome's tab/theme compositor initialization to finish before requiring
    // exact equality between two screenshots of this static fixture.
    evaluate(
        websocket,
        "new Promise(resolve => setTimeout(() => resolve(true),500))",
    )
    .await?;
    let reader = ChromeApplication::new("http://127.0.0.1:9222")?;
    let output = reader
        .read_page(target)
        .await
        .context("Initial static capture")?;
    ensure!(
        output.accessibility.text.contains("Continue"),
        "Button is missing from AX tree"
    );
    ensure!(
        output.accessibility.text.contains("Name"),
        "Input is missing from AX tree"
    );
    ensure!(
        output.screenshot.pixel_width > 0 && output.screenshot.pixel_height > 0,
        "Screenshot is empty"
    );
    let second = reader
        .read_page(target)
        .await
        .context("Second static capture")?;
    ensure!(
        second.target == output.target,
        "Read changed target unexpectedly"
    );
    ensure!(
        second.screenshot.image.data == output.screenshot.image.data,
        "Static page captures differ"
    );

    evaluate(websocket, r#"document.open(); document.write('<!doctype html><title>Scaled fixture</title><style>html{scrollbar-width:none}body{margin:0;height:2000px}button{position:absolute;left:100px;top:340px;width:160px;height:50px}</style><button>Scale control</button>'); document.close(); true"#).await?;
    // Emulation is scoped to its CDP connection; keep it alive through each read.
    let (mut emulation, _) = tokio_tungstenite::connect_async(websocket).await?;
    let mut zoom = None;
    for scale in [1.0, 2.0, 2.5] {
        command_on_socket(
            &mut emulation,
            "Emulation.setDeviceMetricsOverride",
            json!({"width":800,"height":600,"deviceScaleFactor":scale,"mobile":false}),
        )
        .await?;
        for scroll in [0, 200] {
            let expected = evaluate(websocket, &format!("(async () => {{ scrollTo(0,{scroll}); await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); await new Promise(resolve => setTimeout(resolve,200)); const r = document.querySelector('button').getBoundingClientRect(); return {{x:r.x,y:r.y,width:r.width,height:r.height,scale:devicePixelRatio}}; }})()")).await?;
            // Chrome may retain browser zoom even in a private context. Compare
            // actual CSS coordinates and verify emulation preserves that zoom.
            let actual_scale = expected["scale"].as_f64().context("Missing device scale")?;
            let zoom = *zoom.get_or_insert(actual_scale / scale);
            ensure!(
                (actual_scale / scale - zoom).abs() < 0.001,
                "Fixture scale changed unexpectedly: {expected}"
            );
            let output = reader
                .read_page(target)
                .await
                .with_context(|| format!("Scaled capture at scale {scale}, scroll {scroll}"))?;
            let expected_bounds = format!(
                "bounds=({:.1},{:.1},{},{})",
                expected["x"].as_f64().unwrap(),
                expected["y"].as_f64().unwrap(),
                expected["width"].as_f64().unwrap(),
                expected["height"].as_f64().unwrap()
            );
            let line = output
                .accessibility
                .text
                .lines()
                .find(|line| line.contains("button \"Scale control\""))
                .with_context(|| format!("Scaled button is absent at scale {scale}, scroll {scroll}, expected={expected}, viewport={:?}: {}", output.viewport, output.accessibility.text))?;
            ensure!(
                line.contains(&expected_bounds),
                "Incorrect CSS bounds at scale {scale}, scroll {scroll}: {line}; expected {expected_bounds}"
            );
        }
    }

    Ok(())
}

#[tokio::test]
async fn chrome_reads_accessibility_and_screenshots() -> Result<()> {
    let _fixture_guard = CHROME_FIXTURE.lock().await;
    let fixture = Fixture::new().await?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        exercise(&fixture.target, &fixture.websocket),
    )
    .await;
    // Run cleanup before propagating assertions, errors, or timeout.
    let cleanup = fixture.close().await;
    result??;
    cleanup?;
    Ok(())
}

async fn exercise_root_replacement(fixture: &Fixture) -> Result<()> {
    evaluate(
        &fixture.websocket,
        "document.body.innerHTML = '<button>Before</button>'; true",
    )
    .await?;
    let frame = command(&fixture.websocket, "Page.getFrameTree", json!({})).await?;
    let world = command(
        &fixture.websocket,
        "Page.createIsolatedWorld",
        json!({"frameId":frame["frameTree"]["frame"]["id"],"worldName":"grok-screen"}),
    )
    .await?;
    let context = world["executionContextId"].clone();
    // Existing tabs may already have the first watcher. The reader must upgrade it.
    command(
        &fixture.websocket,
        "Runtime.evaluate",
        json!({"contextId":context,"expression":"globalThis.__grokScreen = {revision:0}; true"}),
    )
    .await?;
    let reader = ChromeApplication::new("http://127.0.0.1:9222")?;
    reader.read_page(&fixture.target).await?;
    let sample = || {
        command(
            &fixture.websocket,
            "Runtime.evaluate",
            json!({"contextId":context,"expression":"globalThis.__grokScreen.sample()","returnByValue":true}),
        )
    };
    let before = sample().await?["result"]["value"].clone();
    evaluate(&fixture.websocket, "(() => { const root = document.documentElement.cloneNode(true); root.querySelector('button').textContent = 'After'; document.replaceChild(root,document.documentElement); return new Promise(resolve => setTimeout(() => resolve(true),100)); })()").await?;
    let after = sample().await?["result"]["value"].clone();
    ensure!(
        before["documentId"] == after["documentId"],
        "Fixture unexpectedly navigated"
    );
    ensure!(
        after["revision"].as_u64() > before["revision"].as_u64(),
        "Root replacement escaped the capture watcher: {before} -> {after}"
    );
    evaluate(&fixture.websocket, "document.querySelector('button').textContent = 'After again'; new Promise(resolve => setTimeout(() => resolve(true),100))").await?;
    let next = sample().await?["result"]["value"].clone();
    ensure!(
        next["revision"].as_u64() > after["revision"].as_u64(),
        "Watcher did not follow the new subtree"
    );
    let output = reader.read_page(&fixture.target).await?;
    ensure!(
        output.accessibility.text.contains("After again"),
        "Replacement tree was not observed"
    );
    Ok(())
}

#[tokio::test]
async fn chrome_capture_watcher_tracks_document_root_replacement() -> Result<()> {
    let _fixture_guard = CHROME_FIXTURE.lock().await;
    let fixture = Fixture::new().await?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        exercise_root_replacement(&fixture),
    )
    .await;
    let cleanup = fixture.close().await;
    result??;
    cleanup?;
    Ok(())
}
