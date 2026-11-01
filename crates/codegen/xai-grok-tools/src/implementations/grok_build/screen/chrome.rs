//! Chrome page observations through the loopback DevTools Protocol.
//! Uses an existing CDP-enabled instance without an extension or desktop capture permission.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;
use xai_tool_runtime::{ToolError, ToolErrorKind};

use super::{AccessibilityTree, ReadScreenOutput, Screenshot, Viewport};
use crate::types::output::ImageContent;

const MAX_RESPONSE: usize = 16 * 1024 * 1024;
const MAX_NODES: usize = 1500;
const MAX_RAW_NODES: usize = 20_000;
const MAX_TEXT: usize = 60_000;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(20);

fn json_at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
    value.pointer(pointer).unwrap_or(&Value::Null)
}

fn failure(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(ToolErrorKind::Execution, format!("Chrome screen: {error}"))
}

fn local_url(url: &Url, schemes: &[&str]) -> Result<(), ToolError> {
    let local = match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        _ => false,
    };
    if !schemes.contains(&url.scheme())
        || !local
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ToolError::invalid_arguments(
            "Chrome CDP must use a loopback HTTP/WebSocket address without credentials, query, or fragment",
        ));
    }
    Ok(())
}

pub(crate) struct ChromePage {
    pub target: String,
    pub title: String,
    pub viewport: Viewport,
    pub accessibility: AccessibilityTree,
    pub coverage: Vec<String>,
}

#[derive(Clone, Copy)]
enum CaptureKind {
    Accessibility,
    Page,
}

/// Chrome page reader. Operations are serialized and capture state is checked for changes.
pub struct ChromeApplication {
    endpoint: Url,
    client: reqwest::Client,
    operation: Mutex<()>,
}

impl std::fmt::Debug for ChromeApplication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChromeApplication")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl ChromeApplication {
    /// Configure a loopback Chrome CDP endpoint. Connection is deferred until tool use.
    ///
    /// # Errors
    /// Returns an error for remote endpoints or failure to construct the HTTP client.
    pub fn new(endpoint: &str) -> Result<Self, ToolError> {
        let endpoint = Url::parse(endpoint).map_err(|error| {
            ToolError::invalid_arguments(format!("Invalid Chrome CDP endpoint: {error}"))
        })?;
        local_url(&endpoint, &["http"])?;
        if endpoint.path() != "/" {
            return Err(ToolError::invalid_arguments(
                "CDP endpoint must be an HTTP origin",
            ));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "Validated loopback HTTP CDP only; this client never makes a TLS connection"
        )]
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(failure)?;
        Ok(Self {
            endpoint,
            client,
            operation: Mutex::new(()),
        })
    }

    async fn targets(&self) -> Result<Vec<Target>, ToolError> {
        let response = self
            .client
            .get(self.endpoint.join("json/list").map_err(failure)?)
            .send()
            .await
            .map_err(|error| {
                failure(format!(
                    "Cannot reach the local Chrome accessibility endpoint: {error}"
                ))
            })?
            .error_for_status()
            .map_err(failure)?;
        let mut response = response;
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failure)? {
            if data.len() + chunk.len() > MAX_RESPONSE {
                return Err(failure("Target list exceeds response limit"));
            }
            data.extend_from_slice(&chunk);
        }
        let targets: Vec<Target> = serde_json::from_slice(&data).map_err(failure)?;
        Ok(targets
            .into_iter()
            .filter(|target| target.kind == "page")
            .collect())
    }

    async fn target(&self, id: &str) -> Result<Target, ToolError> {
        self.targets()
            .await?
            .into_iter()
            .find(|target| target.id == id)
            .ok_or_else(|| failure("Chrome page closed during observation"))
    }

    /// Read a known page for provider-level observation and capture validation.
    /// Target IDs stay inside the desktop provider; the agent tool takes no arguments.
    ///
    /// # Errors
    /// Returns an error for unavailable, changing, or timed-out pages.
    pub async fn read_page(&self, target: &str) -> Result<ReadScreenOutput, ToolError> {
        tokio::time::timeout(OPERATION_TIMEOUT, async {
            let _operation = self.operation.lock().await;
            let (page, screenshot) = self.read(target, CaptureKind::Page).await?;
            Ok(ReadScreenOutput {
                observation_id: uuid::Uuid::now_v7().to_string(),
                application: "chrome".to_owned(),
                target: page.target,
                title: page.title,
                viewport: page.viewport,
                accessibility: page.accessibility,
                screenshot: screenshot.ok_or_else(|| failure("Screenshot is missing"))?,
                coverage: page.coverage,
            })
        })
        .await
        .map_err(|_| ToolError::new(ToolErrorKind::Timeout, "Chrome page read timed out"))?
    }

    pub(crate) async fn visible_pages(
        &self,
        titles: &[String],
    ) -> Result<(Vec<ChromePage>, Vec<String>), ToolError> {
        let _operation = self.operation.lock().await;
        let targets = self.targets().await?;
        let mut pages = Vec::new();
        let mut coverage = Vec::new();
        for title in titles {
            let matching: Vec<_> = targets
                .iter()
                .filter(|target| &target.title == title)
                .collect();
            let mut selected = Vec::new();
            for target in matching {
                let mut cdp = self.session(target).await?;
                if cdp
                    .evaluate("document.visibilityState === 'visible'", None)
                    .await?
                    == true
                {
                    selected.push(target);
                }
            }
            let [selected] = selected.as_slice() else {
                coverage.push(format!("Chrome accessibility unavailable for visible window {title:?}; page mapping was not unique"));
                continue;
            };
            match self.read(&selected.id, CaptureKind::Accessibility).await {
                Ok((page, _)) => pages.push(page),
                Err(error) => coverage.push(format!(
                    "Chrome accessibility unavailable for {title:?}: {error}"
                )),
            }
        }
        Ok((pages, coverage))
    }

    async fn session(&self, target: &Target) -> Result<Cdp, ToolError> {
        let url = Url::parse(&target.websocket).map_err(failure)?;
        local_url(&url, &["ws"])?;
        if url.port_or_known_default() != self.endpoint.port_or_known_default() {
            return Err(failure("CDP target advertised a different port"));
        }
        Cdp::connect(url.as_str()).await
    }

    async fn read(
        &self,
        id: &str,
        capture: CaptureKind,
    ) -> Result<(ChromePage, Option<Screenshot>), ToolError> {
        let target = self.target(id).await?;
        let mut cdp = self.session(&target).await?;
        let frame = json_at(
            &cdp.command("Page.getFrameTree", json!({})).await?,
            "/frameTree/frame",
        )
        .clone();
        let frame_id = json_at(&frame, "/id")
            .as_str()
            .ok_or_else(|| failure("Missing page frame"))?;
        let world = cdp
            .command(
                "Page.createIsolatedWorld",
                json!({"frameId":frame_id,"worldName":"grok-screen"}),
            )
            .await?;
        let context = json_at(&world, "/executionContextId")
            .as_u64()
            .ok_or_else(|| failure("Missing isolated context"))?;
        cdp.evaluate(STATE_SCRIPT, Some(context)).await?;
        let before = cdp
            .evaluate("globalThis.__grokScreen.sample()", Some(context))
            .await?;
        viewport(&before)?;
        let (ax, layout, screenshot) = async {
            let ax = cdp
                .command("Accessibility.getFullAXTree", json!({"frameId":frame_id}))
                .await?;
            let layout = cdp
                .command("DOMSnapshot.captureSnapshot", json!({"computedStyles":[]}))
                .await?;
            let screenshot = match capture {
                CaptureKind::Accessibility => None,
                CaptureKind::Page => Some(
                    cdp.command(
                        "Page.captureScreenshot",
                        json!({"format":"png","fromSurface":true,"captureBeyondViewport":false}),
                    )
                    .await?,
                ),
            };
            Ok::<_, ToolError>((ax, layout, screenshot))
        }
        .await?;
        let after = cdp
            .evaluate("globalThis.__grokScreen.sample()", Some(context))
            .await?;
        if before != after {
            return Err(failure(
                "Page changed during capture; call read_screen again",
            ));
        }
        let screenshot = screenshot
            .map(|screenshot| {
                let data = json_at(&screenshot, "/data")
                    .as_str()
                    .ok_or_else(|| failure("Screenshot is missing"))?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(failure)?;
                let decoder = image::ImageReader::with_format(
                    std::io::Cursor::new(&bytes),
                    image::ImageFormat::Png,
                );
                let (pixel_width, pixel_height) = decoder.into_dimensions().map_err(failure)?;
                if pixel_width == 0 || pixel_height == 0 {
                    return Err(failure("Empty screenshot"));
                }
                Ok(Screenshot {
                    image: ImageContent {
                        data: data.to_owned(),
                        mime_type: "image/png".to_owned(),
                        annotations: None,
                        uri: None,
                        meta: None,
                    },
                    pixel_width,
                    pixel_height,
                })
            })
            .transpose()?;
        let page = ChromePage {
            target: target.id,
            title: json_at(&before, "/title").as_str().unwrap_or(&target.title).to_owned(),
            viewport: viewport(&before)?,
            accessibility: accessibility(&ax, &layout, &before)?,
            coverage: vec![
                "Chrome page viewport only; browser controls and native dialogs are excluded".to_owned(),
                "Accessibility is application-provided; canvas and cross-origin frame semantics may be absent. Main-document bounds use a checked DOM reference; unmatched bounds and child-frame coordinates are omitted".to_owned(),
                "AX and layout are sampled separately. DOM and viewport changes are checked; animation, video, and compositor-only changes cannot be made atomic".to_owned(),
            ],
        };
        Ok((page, screenshot))
    }
}

#[derive(Debug, serde::Deserialize)]
struct Target {
    id: String,
    title: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "webSocketDebuggerUrl", default)]
    websocket: String,
}

struct Cdp {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    sequence: u64,
}

impl Cdp {
    async fn connect(url: &str) -> Result<Self, ToolError> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_RESPONSE))
            .max_frame_size(Some(MAX_RESPONSE));
        let (socket, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), false)
            .await
            .map_err(failure)?;
        Ok(Self {
            socket,
            sequence: 0,
        })
    }
    async fn command(&mut self, method: &str, params: Value) -> Result<Value, ToolError> {
        self.sequence += 1;
        let id = self.sequence;
        self.socket
            .send(Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(failure)?;
        while let Some(message) = self.socket.next().await {
            match message.map_err(failure)? {
                Message::Text(text) => {
                    let response: Value = serde_json::from_str(&text).map_err(failure)?;
                    if json_at(&response, "/id").as_u64() != Some(id) {
                        continue;
                    }
                    if let Some(error) = response.get("error") {
                        return Err(failure(format!("{method}: {error}")));
                    }
                    return response
                        .get("result")
                        .cloned()
                        .ok_or_else(|| failure("Malformed CDP response"));
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        Err(failure("CDP disconnected"))
    }
    async fn evaluate(
        &mut self,
        expression: &str,
        context: Option<u64>,
    ) -> Result<Value, ToolError> {
        let mut params = json!({"expression":expression,"returnByValue":true,"awaitPromise":true});
        if let Some(context) = context {
            params
                .as_object_mut()
                .expect("Runtime parameters are an object")
                .insert("contextId".to_owned(), json!(context));
        }
        let response = self.command("Runtime.evaluate", params).await?;
        if response.get("exceptionDetails").is_some() {
            return Err(failure(
                "Page evaluation failed (navigation, unsupported page, or renderer failure)",
            ));
        }
        Ok(json_at(&response, "/result/value").clone())
    }
}

fn viewport(state: &Value) -> Result<Viewport, ToolError> {
    let width = json_at(state, "/width")
        .as_f64()
        .ok_or_else(|| failure("Missing viewport width"))?;
    let height = json_at(state, "/height")
        .as_f64()
        .ok_or_else(|| failure("Missing viewport height"))?;
    if width <= 0.0
        || height <= 0.0
        || width > 16_384.0
        || height > 16_384.0
        || json_at(state, "/visualScale").as_f64() != Some(1.0)
    {
        return Err(failure("Unsupported viewport size or pinch zoom"));
    }
    for field in ["deviceScale", "scrollX", "scrollY"] {
        let value = json_at(state, &format!("/{field}"))
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| failure(format!("Missing or invalid {field}")))?;
        if field == "deviceScale" && value <= 0.0 {
            return Err(failure("Invalid device scale"));
        }
    }
    Ok(Viewport { width, height })
}

#[derive(Clone, Copy)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Bounds {
    fn from_json(value: &Value) -> Option<Self> {
        let [x, y, width, height] = value.as_array()?.as_slice() else {
            return None;
        };
        let rect = Self {
            x: x.as_f64()?,
            y: y.as_f64()?,
            width: width.as_f64()?,
            height: height.as_f64()?,
        };
        [rect.x, rect.y, rect.width, rect.height]
            .iter()
            .all(|value| value.is_finite())
            .then_some(rect)
    }

    fn to_viewport(self, scale: f64, scroll_x: f64, scroll_y: f64) -> Self {
        Self {
            x: self.x / scale - scroll_x,
            y: self.y / scale - scroll_y,
            width: self.width / scale,
            height: self.height / scale,
        }
    }

    fn intersects(self, viewport: &Viewport) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && self.x < viewport.width
            && self.y < viewport.height
            && self.x + self.width > 0.0
            && self.y + self.height > 0.0
    }
}

fn snapshot_scale(document: &Value, state: &Value) -> Option<f64> {
    let types = json_at(document, "/nodes/nodeType").as_array()?;
    let parents = json_at(document, "/nodes/parentIndex").as_array()?;
    let root = types.iter().enumerate().find_map(|(index, kind)| {
        (kind == 1 && parents.get(index).and_then(Value::as_i64) == Some(0)).then_some(index)
    })?;
    let index = json_at(document, "/layout/nodeIndex")
        .as_array()?
        .iter()
        .position(|index| index.as_u64() == Some(root as u64))?;
    let raw = Bounds::from_json(json_at(document, "/layout/bounds").get(index)?)?;
    let css = Bounds::from_json(json_at(state, "/rootBounds"))?;
    // Unlike innerWidth/clientWidth, getBoundingClientRect retains fractional
    // sizes. The same root in the snapshot supplies a measured coordinate scale,
    // even when CDP emulation changes devicePixelRatio independently of layout.
    let scale = if css.width > 0.0 {
        raw.width / css.width
    } else if css.height > 0.0 {
        raw.height / css.height
    } else {
        return None;
    };
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let scroll_x = json_at(state, "/scrollX").as_f64()?;
    let scroll_y = json_at(state, "/scrollY").as_f64()?;
    let mapped = raw.to_viewport(scale, scroll_x, scroll_y);
    // Verify both size and origin; if this reference cannot establish the
    // coordinate convention, omit bounds rather than guess where controls are.
    [
        mapped.x - css.x,
        mapped.y - css.y,
        mapped.width - css.width,
        mapped.height - css.height,
    ]
    .iter()
    .all(|difference| difference.is_finite() && difference.abs() <= 0.1)
    .then_some(scale)
}

fn semantic(node: &Value) -> bool {
    if json_at(node, "/ignored") == true {
        return false;
    }
    match json_at(node, "/role/value").as_str() {
        Some("InlineTextBox") => false,
        Some("generic") => {
            !json_at(node, "/name/value")
                .as_str()
                .unwrap_or_default()
                .is_empty()
                || node.get("value").is_some()
                || json_at(node, "/properties")
                    .as_array()
                    .is_some_and(|properties| {
                        properties.iter().any(|property| {
                            matches!(
                                json_at(property, "/name").as_str(),
                                Some(
                                    "focused"
                                        | "focusable"
                                        | "disabled"
                                        | "checked"
                                        | "expanded"
                                        | "selected"
                                )
                            ) && json_at(property, "/value/value") != false
                        })
                    })
        }
        _ => true,
    }
}

fn accessibility(
    ax: &Value,
    layout: &Value,
    state: &Value,
) -> Result<AccessibilityTree, ToolError> {
    use std::fmt::Write as _;
    let viewport = viewport(state)?;
    let scroll_x = json_at(state, "/scrollX")
        .as_f64()
        .expect("Validated scroll");
    let scroll_y = json_at(state, "/scrollY")
        .as_f64()
        .expect("Validated scroll");
    let nodes = json_at(ax, "/nodes")
        .as_array()
        .ok_or_else(|| failure("Accessibility tree is unavailable"))?;
    let by_id: HashMap<&str, &Value> = nodes
        .iter()
        .filter_map(|node| json_at(node, "/nodeId").as_str().map(|id| (id, node)))
        .collect();
    let mut bounds = HashMap::new();
    // DOMSnapshot's first document is the main frame. Child frame coordinates need
    // frame transforms and are intentionally left absent rather than guessed.
    let document = json_at(layout, "/documents/0");
    if let Some(scale) = snapshot_scale(document, state)
        && let Some(indices) = json_at(document, "/layout/nodeIndex").as_array()
    {
        for (i, index) in indices.iter().enumerate() {
            if let Some(index) = index.as_u64()
                && let Some(backend_id) = json_at(document, "/nodes/backendNodeId")
                    .get(index as usize)
                    .and_then(Value::as_u64)
                && let Some(rect) = json_at(document, "/layout/bounds")
                    .get(i)
                    .and_then(Bounds::from_json)
            {
                bounds.insert(backend_id, rect.to_viewport(scale, scroll_x, scroll_y));
            }
        }
    }
    let rect = |node: &Value| {
        json_at(node, "/backendDOMNodeId")
            .as_u64()
            .and_then(|id| bounds.get(&id))
            .copied()
    };
    // Prioritize branches containing visible controls while preserving their
    // semantic ancestors. Unmapped content remains available after those branches.
    let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in nodes {
        if let Some(id) = json_at(node, "/nodeId").as_str()
            && let Some(children) = json_at(node, "/childIds").as_array()
        {
            for child in children.iter().filter_map(Value::as_str) {
                parents.entry(child).or_default().push(id);
            }
        }
    }
    let mut visible = HashSet::new();
    let mut pending: Vec<&str> = nodes
        .iter()
        .filter(|node| semantic(node) && rect(node).is_some_and(|rect| rect.intersects(&viewport)))
        .filter_map(|node| json_at(node, "/nodeId").as_str())
        .collect();
    while let Some(id) = pending.pop() {
        if visible.insert(id)
            && let Some(ancestors) = parents.get(id)
        {
            pending.extend(ancestors.iter().copied());
        }
    }
    let mut stack: Vec<(&Value, usize)> = nodes
        .first()
        .map(|root| vec![(root, 0)])
        .unwrap_or_default();
    let mut visited = HashSet::new();
    let mut text = String::new();
    let mut node_count = 0;
    let mut truncated = false;
    while let Some((node, depth)) = stack.pop() {
        let id = json_at(node, "/nodeId").as_str().unwrap_or_default();
        if !visited.insert(id) {
            continue;
        }
        if visited.len() > MAX_RAW_NODES {
            truncated = true;
            break;
        }
        let emit = semantic(node)
            && (rect(node).is_none_or(|rect| rect.intersects(&viewport)) || visible.contains(id));
        if emit && (node_count >= MAX_NODES || depth > 40) {
            truncated = true;
            continue;
        }
        if let Some(children) = json_at(node, "/childIds").as_array() {
            let mut children: Vec<&Value> = children
                .iter()
                .filter_map(|child| child.as_str().and_then(|id| by_id.get(id)).copied())
                .collect();
            children.sort_by_key(|child| {
                !visible.contains(json_at(child, "/nodeId").as_str().unwrap_or_default())
            });
            for child in children.into_iter().rev() {
                stack.push((child, depth + usize::from(emit)));
            }
        }
        if !emit {
            continue;
        }
        let mut line = String::new();
        let role = json_at(node, "/role/value").as_str().unwrap_or("unknown");
        let name: String = json_at(node, "/name/value")
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        let _ = write!(line, "{}[{id}] {role} {name:?}", "  ".repeat(depth));
        if let Some(Bounds {
            x,
            y,
            width,
            height,
        }) = rect(node)
        {
            let _ = write!(line, " bounds=({x:.1},{y:.1},{width},{height})");
        }
        let properties = json_at(node, "/properties").as_array();
        let protected = properties.is_some_and(|properties| {
            properties
                .iter()
                .any(|p| json_at(p, "/name") == "protected" && json_at(p, "/value/value") == true)
        });
        if !protected && let Some(value) = json_at(node, "/value/value").as_str() {
            let value: String = value.chars().take(300).collect();
            let _ = write!(line, " value={value:?}");
        }
        if let Some(properties) = properties {
            for property in properties {
                if let Some(
                    name @ ("focused" | "focusable" | "disabled" | "checked" | "expanded"
                    | "selected"),
                ) = json_at(property, "/name").as_str()
                {
                    let _ = write!(line, " {name}={}", json_at(property, "/value/value"));
                }
            }
        }
        line.push('\n');
        if text.len() + line.len() > MAX_TEXT {
            truncated = true;
            break;
        }
        text.push_str(&line);
        node_count += 1;
    }
    if node_count == 0 {
        text.push_str("No accessible controls were exposed. Use the screenshot; do not infer an empty page.\n");
    }
    Ok(AccessibilityTree {
        text,
        node_count,
        truncated,
    })
}

const STATE_SCRIPT: &str = r#"(() => {
  if (globalThis.__grokScreen?.version === 3) {
    globalThis.__grokScreen.arm();
    return true;
  }
  globalThis.__grokScreen?.dispose?.();
  const events = new AbortController();
  const state = {
    version: 3,
    documentId: Array.from(crypto.getRandomValues(new Uint32Array(4)), n => n.toString(16)).join('-'),
    revision: 0,
    arm() {
      // document.open() can discard listeners; adding the same callback is idempotent.
      for (const event of ['scroll', 'input', 'change', 'focusin', 'focusout']) {
        document.addEventListener(event, changed, { capture: true, signal: events.signal });
      }
    },
    dispose() { observer.disconnect(); events.abort(); },
    sample() {
      const root = document.documentElement?.getBoundingClientRect();
      return { documentId: this.documentId, revision: this.revision, title: document.title,
        url: location.href, width: innerWidth, height: innerHeight, scrollX, scrollY,
        visualScale: visualViewport?.scale ?? 1, visualX: visualViewport?.offsetLeft ?? 0,
        visualY: visualViewport?.offsetTop ?? 0, deviceScale: devicePixelRatio,
        rootBounds: root ? [root.x,root.y,root.width,root.height] : null };
    }
  };
  const changed = () => state.revision++;
  const observer = new MutationObserver(changed);
  // Observe the stable Document, including replacement of its root element.
  observer.observe(document,
    { subtree: true, childList: true, attributes: true, characterData: true });
  state.arm();
  globalThis.__grokScreen = state;
  return true;
})()"#;

#[cfg(test)]
mod tests {
    use super::{accessibility, local_url, viewport};

    use serde_json::json;
    use url::Url;

    fn state(scale: f64, scroll: f64) -> serde_json::Value {
        json!({"width":800,"height":600,"visualScale":1,"deviceScale":scale,"scrollX":0,"scrollY":scroll,"rootBounds":[0,-scroll,800,2000]})
    }

    fn layout(scale: f64, controls: &[(u64, [f64; 4])]) -> serde_json::Value {
        let mut types = vec![9, 1];
        let mut parents = vec![-1, 0];
        let mut ids = vec![99, 100];
        let mut indices = vec![1];
        let mut bounds = vec![[0.0, 0.0, 800.0 * scale, 2000.0 * scale]];
        for &(id, rect) in controls {
            types.push(1);
            parents.push(1);
            ids.push(id);
            indices.push(ids.len() - 1);
            bounds.push(rect.map(|value| value * scale));
        }
        json!({"documents":[{"nodes":{"nodeType":types,"parentIndex":parents,"backendNodeId":ids},"layout":{"nodeIndex":indices,"bounds":bounds}}]})
    }

    #[test]
    fn accessibility_bounds_use_css_coordinates_at_every_scale_and_scroll() {
        let ax = json!({"nodes":[{"nodeId":"1","role":{"value":"button"},"name":{"value":"Continue"},"backendDOMNodeId":12}]});
        for scale in [1.0, 2.0, 2.5] {
            for scroll in [0.0, 200.0] {
                let layout = layout(scale, &[(12, [100.0, 340.0, 160.0, 50.0])]);
                let tree = accessibility(&ax, &layout, &state(scale, scroll)).unwrap();
                assert!(
                    tree.text
                        .contains(&format!("bounds=(100.0,{:.1},160,50)", 340.0 - scroll)),
                    "{}",
                    tree.text
                );
            }
        }
        for scale in [0.0, -1.0, f64::INFINITY] {
            assert!(viewport(&state(scale, 0.0)).is_err());
        }
    }

    #[test]
    fn snapshot_reference_handles_emulated_scale_and_fractional_sizes() {
        let ax = json!({"nodes":[{"nodeId":"control","role":{"value":"button"},"name":{"value":"Continue"},"backendDOMNodeId":12}]});
        let mut layout = layout(2.5, &[(12, [100.0, 340.0, 160.0, 50.0])]);
        let mut state = state(1.0, 200.0);
        state["rootBounds"] = json!([0, -200, 800.015625, 2000.046875]);
        layout["documents"][0]["layout"]["bounds"][0] =
            json!([0, 0, 800.015625 * 2.5, 2000.046875 * 2.5]);
        let tree = accessibility(&ax, &layout, &state).unwrap();
        assert!(
            tree.text.contains("bounds=(100.0,140.0,160,50)"),
            "{}",
            tree.text
        );
        // An inconsistent reference must not supply plausible but wrong locations.
        state["rootBounds"][1] = json!(-100);
        let tree = accessibility(&ax, &layout, &state).unwrap();
        assert!(tree.text.contains("Continue"));
        assert!(!tree.text.contains("bounds="));
    }

    #[test]
    fn accessibility_compacts_wrappers_and_prioritizes_visible_semantics() {
        let mut nodes = vec![
            json!({"nodeId":"root","role":{"value":"RootWebArea"},"childIds":["offscreen","unmapped","wrapper-0"]}),
        ];
        let glyph_ids: Vec<String> = (0..5000).map(|i| format!("glyph-{i}")).collect();
        nodes.push(json!({"nodeId":"offscreen","role":{"value":"button"},"name":{"value":"Hidden control"},"backendDOMNodeId":1,"childIds":glyph_ids}));
        for id in &glyph_ids {
            nodes.push(
                json!({"nodeId":id,"role":{"value":"InlineTextBox"},"name":{"value":"glyph"}}),
            );
        }
        nodes.push(json!({"nodeId":"unmapped","role":{"value":"textbox"},"name":{"value":"Unmapped input"}}));
        for i in 0..80 {
            let child = if i == 79 {
                "visible".to_owned()
            } else {
                format!("wrapper-{}", i + 1)
            };
            nodes.push(json!({"nodeId":format!("wrapper-{i}"),"role":{"value":"generic"},"childIds":[child]}));
        }
        nodes.push(json!({"nodeId":"visible","role":{"value":"button"},"name":{"value":"Visible control"},"backendDOMNodeId":2,"childIds":["root"]}));
        let layout = layout(
            1.0,
            &[
                (1, [10.0, 3000.0, 160.0, 50.0]),
                (2, [100.0, 140.0, 160.0, 50.0]),
            ],
        );
        let tree = accessibility(&json!({"nodes":nodes}), &layout, &state(1.0, 0.0)).unwrap();
        assert_eq!(tree.node_count, 3, "{}", tree.text);
        assert!(!tree.truncated);
        assert!(!tree.text.contains("InlineTextBox"));
        assert!(!tree.text.contains("generic"));
        assert!(!tree.text.contains("Hidden control"));
        assert!(tree.text.contains("  [visible] button"));
        assert!(
            tree.text.find("Visible control").unwrap() < tree.text.find("Unmapped input").unwrap()
        );
    }

    #[tokio::test]
    async fn capture_rejects_a_revision_change_between_ax_and_screenshot() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 4096);
                header.push(stream.read_u8().await.unwrap());
            }
            let targets = json!([{"id":"fixture","type":"page","title":"Fixture","webSocketDebuggerUrl":format!("ws://{address}/devtools/page/fixture")}]).to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{targets}", targets.len()).as_bytes()).await.unwrap();
            drop(stream);
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut samples = 0;
            while let Some(message) = socket.next().await {
                let Message::Text(request) = message.unwrap() else {
                    continue;
                };
                let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "Page.getFrameTree" => json!({"frameTree":{"frame":{"id":"main"}}}),
                    "Page.createIsolatedWorld" => json!({"executionContextId":1}),
                    "Runtime.evaluate"
                        if request["params"]["expression"]
                            == "globalThis.__grokScreen.sample()" =>
                    {
                        let mut snapshot = state(2.0, 0.0);
                        snapshot["documentId"] = json!("unchanged-document");
                        snapshot["revision"] = json!(samples);
                        samples += 1;
                        json!({"result":{"value":snapshot}})
                    }
                    "Runtime.evaluate" => json!({"result":{"value":true}}),
                    "Accessibility.getFullAXTree" => json!({"nodes":[]}),
                    "DOMSnapshot.captureSnapshot" => json!({"documents":[]}),
                    // Invalid PNG proves the freshness guard runs before decoding.
                    "Page.captureScreenshot" => json!({"data":"not-a-png"}),
                    other => panic!("Unexpected command {other}"),
                };
                socket
                    .send(Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                if samples == 2 {
                    break;
                }
            }
        });
        let application = super::ChromeApplication::new(&format!("http://{address}")).unwrap();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            application.read_page("fixture"),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.detail.contains("Page changed during capture"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn protocol_matches_response_ids_and_surfaces_command_errors() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for fail in [false, true] {
                let Message::Text(request) = socket.next().await.unwrap().unwrap() else {
                    panic!("Expected request");
                };
                let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                socket
                    .send(Message::Text(
                        json!({"method":"Page.frameNavigated","params":{}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                let response = if fail {
                    json!({"id":request["id"],"error":{"message":"Unsupported command"}})
                } else {
                    json!({"id":request["id"],"result":{"value":"matched"}})
                };
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let mut cdp = super::Cdp::connect(&format!("ws://{address}"))
            .await
            .unwrap();
        assert_eq!(
            cdp.command("First", json!({})).await.unwrap()["value"],
            "matched"
        );
        assert!(
            cdp.command("Second", json!({}))
                .await
                .unwrap_err()
                .detail
                .contains("Unsupported command")
        );
        server.await.unwrap();
    }

    #[test]
    fn endpoints_are_loopback_only() {
        for endpoint in [
            "http://example.com:9222",
            "http://user:secret@localhost:9222",
            "file:///tmp/a",
            "http://127.0.0.1/?token=secret",
        ] {
            assert!(local_url(&Url::parse(endpoint).unwrap(), &["http"]).is_err());
        }
        assert!(local_url(&Url::parse("http://127.0.0.1:9222").unwrap(), &["http"]).is_ok());
    }

    #[test]
    fn accessibility_preserves_hierarchy_and_masks_password_values() {
        let ax = json!({"nodes":[
            {"nodeId":"1","role":{"value":"RootWebArea"},"childIds":["2","3"]},
            {"nodeId":"2","role":{"value":"button"},"name":{"value":"Continue"},"backendDOMNodeId":12,"properties":[{"name":"focused","value":{"value":true}}]},
            {"nodeId":"3","role":{"value":"textbox"},"value":{"value":"secret"},"properties":[{"name":"protected","value":{"value":true}}]}
        ]});
        let layout = layout(1.0, &[(12, [10.0, 30.0, 80.0, 20.0])]);
        let tree = accessibility(&ax, &layout, &state(1.0, 10.0)).unwrap();
        assert_eq!(tree.node_count, 3);
        assert!(
            tree.text
                .contains("  [2] button \"Continue\" bounds=(10.0,20.0,80,20) focused=true")
        );
        assert!(!tree.text.contains("secret"));
    }

    #[test]
    fn inaccessible_page_is_explicit_and_bad_viewports_are_rejected() {
        let tree = accessibility(&json!({"nodes":[]}), &json!({}), &state(1.0, 0.0)).unwrap();
        assert!(tree.text.contains("No accessible controls"));
        assert!(viewport(&json!({"width":0,"height":800,"visualScale":1})).is_err());
        assert!(viewport(&json!({"width":1280,"height":800,"visualScale":2})).is_err());
    }
}
