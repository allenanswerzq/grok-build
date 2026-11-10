//! Accessibility text for Chrome windows, read through the browser's loopback DevTools port.
//!
//! The port comes from the window process's `--remote-debugging-port` flag, so only the
//! browser that owns the window is queried. The page is the tab whose title matches the
//! window title; when several tabs share it, only the visible one is used.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::ScreenReadOutput;
use crate::implementations::grok_build::hyprland::Client;

/// Window title suffixes by window class prefix.
const BROWSERS: &[(&str, &str)] = &[
    ("google-chrome", " - Google Chrome"),
    ("chromium", " - Chromium"),
];
const PAGE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE: usize = 16 * 1024 * 1024;
/// Text budget shared by all windows of one read.
const MAX_TEXT: usize = 60_000;
const MAX_LINES: usize = 1500;
const MAX_FIELD: usize = 300;
const MAX_INDENT: usize = 40;
const STATES: &[&str] = &["focused", "disabled", "checked", "expanded", "selected"];

/// Attach accessibility text to each captured Chrome window. Failures become notes.
pub(super) async fn describe(clients: &[&Client], output: &mut ScreenReadOutput) {
    let mut budget = MAX_TEXT;
    for (index, (client, window)) in clients.iter().zip(&mut output.windows).enumerate() {
        let Some(title) = page_title(client) else {
            continue;
        };
        let text = match devtools_port(client.pid).await {
            Some(port) => tokio::time::timeout(PAGE_TIMEOUT, page_text(port, title, budget))
                .await
                .unwrap_or_else(|_| Err("timed out".to_owned())),
            None => Err("the browser has no DevTools port".to_owned()),
        };
        match text {
            Ok(text) => {
                budget = budget.saturating_sub(text.len());
                window.accessibility = Some(text);
            }
            Err(error) => output.notes.push(format!(
                "Window {}: no page accessibility text ({error})",
                index + 1
            )),
        }
    }
}

/// The active tab's title for Chrome windows, or `None` for other windows.
fn page_title(client: &Client) -> Option<&str> {
    BROWSERS
        .iter()
        .find(|(class, _)| client.class.starts_with(class))
        .map(|(_, suffix)| client.title.strip_suffix(suffix).unwrap_or(&client.title))
}

/// The DevTools port from a process command line. Chrome may rewrite its arguments into
/// one space-separated string, so both separators are accepted.
async fn devtools_port(pid: i64) -> Option<u16> {
    let cmdline = tokio::fs::read(format!("/proc/{pid}/cmdline")).await.ok()?;
    String::from_utf8_lossy(&cmdline)
        .split(['\0', ' '])
        .find_map(|arg| arg.strip_prefix("--remote-debugging-port="))?
        .parse()
        .ok()
        .filter(|port| *port != 0)
}

#[derive(Deserialize)]
struct Target {
    id: String,
    title: String,
    #[serde(rename = "type")]
    kind: String,
}

async fn page_text(port: u16, title: &str, budget: usize) -> Result<String, String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "Loopback DevTools over plain HTTP; this client never makes a TLS connection"
    )]
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let targets: Vec<Target> = http
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let candidates: Vec<_> = targets
        .iter()
        .filter(|target| target.kind == "page" && target.title == title)
        .filter(|target| target.id.chars().all(|c| c.is_ascii_alphanumeric()))
        .collect();
    let mut pages = Vec::new();
    for target in &candidates {
        let mut page = Cdp::connect(port, &target.id).await?;
        if candidates.len() == 1 || page.visible().await? {
            pages.push(page);
        }
    }
    let mut page = match pages.len() {
        1 => pages.remove(0),
        0 => return Err("no visible tab has the window's title".to_owned()),
        _ => return Err("several visible tabs share the window's title".to_owned()),
    };
    let tree: AxTree = serde_json::from_value(
        page.command("Accessibility.getFullAXTree", json!({}))
            .await?,
    )
    .map_err(|error| error.to_string())?;
    Ok(outline(&tree.nodes, budget))
}

/// One DevTools page session.
struct Cdp {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    sequence: u64,
}

impl Cdp {
    async fn connect(port: u16, id: &str) -> Result<Self, String> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_RESPONSE))
            .max_frame_size(Some(MAX_RESPONSE));
        let url = format!("ws://127.0.0.1:{port}/devtools/page/{id}");
        let (socket, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), false)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Self {
            socket,
            sequence: 0,
        })
    }

    async fn command(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.sequence += 1;
        let id = self.sequence;
        let request = json!({"id": id, "method": method, "params": params}).to_string();
        self.socket
            .send(Message::Text(request.into()))
            .await
            .map_err(|error| error.to_string())?;
        while let Some(message) = self.socket.next().await {
            let Message::Text(text) = message.map_err(|error| error.to_string())? else {
                continue;
            };
            let mut response: Value =
                serde_json::from_str(&text).map_err(|error| error.to_string())?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(format!("{method} failed: {error}"));
            }
            return Ok(response
                .get_mut("result")
                .map(Value::take)
                .unwrap_or_default());
        }
        Err("DevTools disconnected".to_owned())
    }

    async fn visible(&mut self) -> Result<bool, String> {
        let params = json!({"expression": "document.visibilityState", "returnByValue": true});
        let response = self.command("Runtime.evaluate", params).await?;
        Ok(response.pointer("/result/value").and_then(Value::as_str) == Some("visible"))
    }
}

#[derive(Deserialize)]
struct AxTree {
    nodes: Vec<AxNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AxNode {
    node_id: String,
    #[serde(default)]
    ignored: bool,
    role: Option<AxValue>,
    name: Option<AxValue>,
    value: Option<AxValue>,
    #[serde(default)]
    properties: Vec<AxProperty>,
    #[serde(default)]
    child_ids: Vec<String>,
}

#[derive(Deserialize)]
struct AxValue {
    #[serde(default)]
    value: Value,
}

#[derive(Deserialize)]
struct AxProperty {
    name: String,
    value: AxValue,
}

fn text(value: Option<&AxValue>) -> &str {
    value
        .and_then(|value| value.value.as_str())
        .unwrap_or_default()
}

fn clip(text: &str) -> String {
    text.chars().take(MAX_FIELD).collect()
}

/// Indented `role "name"` lines in document order, within `budget` bytes. Wrappers are
/// skipped, text that repeats its parent's name is dropped, and protected values are hidden.
fn outline(nodes: &[AxNode], budget: usize) -> String {
    let by_id: HashMap<&str, &AxNode> = nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect();
    let mut stack: Vec<(&AxNode, usize, &str)> = nodes
        .first()
        .map(|root| (root, 0, ""))
        .into_iter()
        .collect();
    let mut seen = HashSet::new();
    let mut out = String::new();
    let mut lines = 0;
    while let Some((node, depth, parent)) = stack.pop() {
        if !seen.insert(node.node_id.as_str()) {
            continue;
        }
        let line = line(node, parent);
        if line.is_some() && lines == MAX_LINES {
            out.push_str("[accessibility text truncated]\n");
            break;
        }
        let (child_depth, child_parent) = match line {
            Some(_) => (depth + 1, text(node.name.as_ref())),
            None => (depth, parent),
        };
        for child in node.child_ids.iter().rev() {
            if let Some(child) = by_id.get(child.as_str()) {
                stack.push((child, child_depth, child_parent));
            }
        }
        if let Some(line) = line {
            let line = format!("{}{line}\n", "  ".repeat(depth.min(MAX_INDENT)));
            if out.len() + line.len() > budget {
                out.push_str("[accessibility text truncated]\n");
                break;
            }
            out.push_str(&line);
            lines += 1;
        }
    }
    if lines == 0 && out.is_empty() {
        out.push_str("[the page exposes no accessibility text]\n");
    }
    out
}

/// One node's line, or `None` for nodes that add nothing beyond their children.
fn line(node: &AxNode, parent: &str) -> Option<String> {
    use std::fmt::Write as _;
    let role = text(node.role.as_ref());
    if node.ignored
        || matches!(
            role,
            "" | "none" | "presentation" | "InlineTextBox" | "LineBreak"
        )
    {
        return None;
    }
    let name = text(node.name.as_ref());
    if role == "StaticText" && (name.is_empty() || name == parent) {
        return None;
    }
    let protected = node
        .properties
        .iter()
        .any(|property| property.name == "protected" && property.value.value == true);
    let value = text(node.value.as_ref());
    let value = (!protected && !value.is_empty()).then_some(value);
    let mut states = String::new();
    for property in &node.properties {
        let name = property.name.as_str();
        if !STATES.contains(&name) {
            continue;
        }
        let state = match &property.value.value {
            Value::Bool(state) => state.to_string(),
            Value::String(state) => state.clone(),
            _ => continue,
        };
        match state.as_str() {
            "true" => {
                let _ = write!(states, " {name}");
            }
            "false" if !matches!(name, "checked" | "expanded") => {}
            _ => {
                let _ = write!(states, " {name}={state}");
            }
        }
    }
    if role == "generic" && name.is_empty() && value.is_none() && states.is_empty() {
        return None;
    }
    let mut line = role.to_owned();
    if !name.is_empty() {
        let _ = write!(line, " {:?}", clip(name));
    }
    if let Some(value) = value {
        let _ = write!(line, " value={:?}", clip(value));
    }
    line.push_str(&states);
    Some(line)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AxTree, outline, page_title};
    use crate::implementations::grok_build::hyprland::Client;

    fn tree(nodes: serde_json::Value) -> AxTree {
        serde_json::from_value(json!({ "nodes": nodes })).unwrap()
    }

    fn page() -> AxTree {
        tree(json!([
            {"nodeId": "1", "role": {"value": "RootWebArea"}, "name": {"value": "Sign in"}, "childIds": ["2", "3"]},
            {"nodeId": "2", "role": {"value": "generic"}, "childIds": ["4", "5", "8"]},
            {"nodeId": "3", "ignored": true, "role": {"value": "banner"}},
            {"nodeId": "4", "role": {"value": "link"}, "name": {"value": "Help\nnow"}, "childIds": ["6"]},
            {"nodeId": "5", "role": {"value": "textbox"}, "name": {"value": "Password"},
             "value": {"value": "hunter2"},
             "properties": [{"name": "protected", "value": {"value": true}},
                            {"name": "focused", "value": {"value": true}}]},
            {"nodeId": "6", "role": {"value": "StaticText"}, "name": {"value": "Help\nnow"}, "childIds": ["7"]},
            {"nodeId": "7", "role": {"value": "InlineTextBox"}, "name": {"value": "Help"}},
            {"nodeId": "8", "role": {"value": "checkbox"}, "name": {"value": "Remember me"},
             "properties": [{"name": "checked", "value": {"value": "false"}},
                            {"name": "disabled", "value": {"value": false}}],
             "childIds": ["1"]}
        ]))
    }

    #[test]
    fn outline_keeps_semantics_and_hides_passwords() {
        assert_eq!(
            outline(&page().nodes, usize::MAX),
            concat!(
                "RootWebArea \"Sign in\"\n",
                "  link \"Help\\nnow\"\n",
                "  textbox \"Password\" focused\n",
                "  checkbox \"Remember me\" checked=false\n",
            )
        );
    }

    #[test]
    fn outline_stays_within_its_budget() {
        let text = outline(&page().nodes, 30);
        assert!(text.starts_with("RootWebArea \"Sign in\"\n"));
        assert!(text.ends_with("[accessibility text truncated]\n"));
        assert_eq!(
            outline(&tree(json!([])).nodes, usize::MAX),
            "[the page exposes no accessibility text]\n"
        );
    }

    #[test]
    fn only_browser_windows_have_a_page_title() {
        let window = |class: &str, title: &str| -> Client {
            serde_json::from_value(json!({
                "stableId": "1", "pid": 1, "mapped": true, "hidden": false, "pinned": false,
                "fullscreen": 0, "workspace": {"id": 1}, "monitor": 0, "at": [0.0, 0.0],
                "size": [1.0, 1.0], "class": class, "title": title
            }))
            .unwrap()
        };
        let chrome = window("google-chrome", "Docs - Google Chrome");
        assert_eq!(page_title(&chrome), Some("Docs"));
        assert_eq!(
            page_title(&window("dev.zed.Zed", "Docs - Google Chrome")),
            None
        );
    }
}
