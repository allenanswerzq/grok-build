#![cfg(all(
    feature = "live-tests",
    feature = "native-overlay",
    target_os = "linux"
))]

//! Exercises the actual host, compositor pixels, and Wayland pointer delivery.
//! All button presses target this test's own overlay. The original cursor is restored.

use anyhow::{Context, Result, ensure};
use image::RgbaImage;
use serde_json::{Value, json};
use smithay_client_toolkit::reexports::{
    client::{
        Connection, Dispatch, EventQueue, QueueHandle, delegate_noop,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{wl_output, wl_pointer, wl_registry},
    },
    protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
    },
};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::Duration,
};

use xai_tty_utils::{ProcessGroup, ProcessScope};

// Capture may stop producing frames after a lock or DPMS transition. Keep the
// deadline around both pipe reads and process exit, then kill and reap on failure.
fn command_output(program: &str, args: &[&str], deadline: Duration) -> Result<Output> {
    use tokio::io::AsyncReadExt;

    const MAX_CAPTURE: u64 = 32 * 1024 * 1024;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut command = tokio::process::Command::new(program);
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let scope = ProcessScope::new();
            let (mut child, group) = scope.spawn(command)?;
            let result: Result<Output> = tokio::time::timeout(deadline, async {
                let mut stdout = child
                    .stdout
                    .take()
                    .context("missing stdout")?
                    .take(MAX_CAPTURE + 1);
                let mut stderr = child.stderr.take().context("missing stderr")?.take(8193);
                let mut data = Vec::new();
                let mut errors = Vec::new();
                tokio::try_join!(
                    stdout.read_to_end(&mut data),
                    stderr.read_to_end(&mut errors)
                )?;
                ensure!(
                    data.len() as u64 <= MAX_CAPTURE && errors.len() <= 8192,
                    "{program} exceeded the capture limit"
                );
                let status = child.wait().await?;
                Ok(Output {
                    status,
                    stdout: data,
                    stderr: errors,
                })
            })
            .await
            .with_context(|| format!("{program} timed out after {deadline:?}"))
            .and_then(|result| result);
            if result.is_err() {
                let _ = group.kill();
                let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            }
            result
        })
}

fn hypr(query: &str) -> Result<Value> {
    let output = command_output("hyprctl", &["-j", query], Duration::from_secs(5))?;
    ensure!(output.status.success(), "hyprctl {query} failed");
    Ok(serde_json::from_slice(&output.stdout)?)
}

struct Host {
    scope: ProcessScope,
    _group: Arc<ProcessGroup>,
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<String>,
}
impl Host {
    fn start() -> Result<Self> {
        let scope = ProcessScope::new();
        let mut command = Command::new(env!("CARGO_BIN_EXE_grok-overlay-host"));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let (mut child, group) = scope.spawn_std(command)?;
        let stdin = child.stdin.take().context("missing stdin")?;
        let stdout = child.stdout.take().context("missing stdout")?;
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            scope,
            _group: group,
            child,
            stdin,
            replies,
        })
    }
    fn request(&mut self, value: Value) -> Result<Value> {
        writeln!(self.stdin, "{value}")?;
        self.stdin.flush()?;
        let line = self.replies.recv_timeout(Duration::from_secs(20))?;
        let reply: Value = serde_json::from_str(&line)?;
        ensure!(reply.get("error").is_none(), "Overlay failed: {reply}");
        Ok(reply)
    }
    fn mapped(&self) -> Result<bool> {
        let layers = hypr("layers")?;
        Ok(layers
            .as_object()
            .into_iter()
            .flat_map(|o| o.values())
            .filter_map(|o| o["levels"]["3"].as_array())
            .flatten()
            .any(|l| {
                l["namespace"] == "grok-overlay"
                    && l["pid"].as_u64() == Some(u64::from(self.child.id()))
            }))
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.scope.kill_all();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Outputs {
    names: Vec<(wl_output::WlOutput, String)>,
}
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Outputs {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_output::WlOutput, ()> for Outputs {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.names.push((output.clone(), name));
        }
    }
}
delegate_noop!(Outputs: ignore zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
delegate_noop!(Outputs: ignore zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);

struct Pointer {
    connection: Connection,
    queue: EventQueue<Outputs>,
    outputs: Outputs,
    pointer: zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    manager: zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    original: (i64, i64),
    width: u32,
    height: u32,
    held: Option<u32>,
}
impl Pointer {
    fn connect(name: &str, width: u32, height: u32) -> Result<Self> {
        let cursor = hypr("cursorpos")?;
        let original = (
            cursor["x"].as_i64().context("cursor x")?,
            cursor["y"].as_i64().context("cursor y")?,
        );
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<Outputs>(&connection)?;
        let qh = queue.handle();
        let manager: zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 =
            globals.bind(&qh, 2..=2, ())?;
        let mut outputs = Outputs::default();
        let bound: Vec<wl_output::WlOutput> = globals.contents().with_list(|list| {
            list.iter()
                .filter(|global| global.interface == "wl_output")
                .map(|global| {
                    globals
                        .registry()
                        .bind(global.name, 4.min(global.version), &qh, ())
                })
                .collect()
        });
        queue.roundtrip(&mut outputs)?;
        let output = outputs
            .names
            .iter()
            .find(|(_, n)| n == name)
            .context("No matching Wayland output")?;
        let pointer = manager.create_virtual_pointer_with_output(None, Some(&output.0), &qh, ());
        drop(bound);
        Ok(Self {
            connection,
            queue,
            outputs,
            pointer,
            manager,
            original,
            width,
            height,
            held: None,
        })
    }
    fn flush(&mut self) -> Result<()> {
        self.pointer.frame();
        self.connection.flush()?;
        self.queue.roundtrip(&mut self.outputs)?;
        // Wait for input delivery and the overlay's subsequent frame.
        std::thread::sleep(Duration::from_millis(150));
        Ok(())
    }
    fn move_to(&mut self, x: u32, y: u32) -> Result<()> {
        self.pointer
            .motion_absolute(0, x, y, self.width, self.height);
        self.flush()
    }
    fn button(&mut self, button: u32, state: wl_pointer::ButtonState) -> Result<()> {
        self.pointer.button(0, button, state);
        self.held = (state == wl_pointer::ButtonState::Pressed).then_some(button);
        self.flush()
    }
    fn click(&mut self, x: u32, y: u32) -> Result<()> {
        self.move_to(x, y)?;
        self.button(0x110, wl_pointer::ButtonState::Pressed)?;
        self.button(0x110, wl_pointer::ButtonState::Released)
    }
    fn drag(&mut self, button: u32, from: (u32, u32), to: (u32, u32)) -> Result<()> {
        self.move_to(from.0, from.1)?;
        self.button(button, wl_pointer::ButtonState::Pressed)?;
        for step in 1..=4 {
            self.move_to(
                (from.0 as i64 + (to.0 as i64 - from.0 as i64) * step / 4) as u32,
                (from.1 as i64 + (to.1 as i64 - from.1 as i64) * step / 4) as u32,
            )?;
        }
        self.button(button, wl_pointer::ButtonState::Released)
    }
}
impl Drop for Pointer {
    fn drop(&mut self) {
        if let Some(button) = self.held {
            self.pointer
                .button(0, button, wl_pointer::ButtonState::Released);
            self.pointer.frame();
        }
        self.pointer.destroy();
        self.manager.destroy();
        let _ = self.connection.flush();
        let _ = command_output(
            "hyprctl",
            &[
                "dispatch",
                "movecursor",
                &self.original.0.to_string(),
                &self.original.1.to_string(),
            ],
            Duration::from_secs(2),
        );
    }
}

fn screenshot(output: &str) -> Result<RgbaImage> {
    let result = command_output("grim", &["-o", output, "-"], Duration::from_secs(5))?;
    ensure!(
        result.status.success(),
        "Cannot capture native overlay: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(image::load_from_memory(&result.stdout)?.into_rgba8())
}
fn center(image: &RgbaImage) -> Vec<u8> {
    image::imageops::crop_imm(
        image,
        image.width() / 2 - 25,
        image.height() / 2 - 25,
        50,
        50,
    )
    .to_image()
    .into_raw()
}

fn model_area(image: &RgbaImage) -> Vec<u8> {
    image::imageops::crop_imm(
        image,
        image.width() / 4,
        image.height() / 4,
        image.width() / 2,
        image.height() / 2,
    )
    .to_image()
    .into_raw()
}

#[test]
fn real_surface_handles_annotations_orbit_zoom_pan_reset_and_close() -> Result<()> {
    if let Ok(lock) = command_output(
        "omarchy-shell",
        &["lock", "isLocked"],
        Duration::from_secs(5),
    ) {
        ensure!(
            !lock.status.success() || String::from_utf8_lossy(&lock.stdout).trim() != "true",
            "Live desktop test requires an unlocked session"
        );
    }
    let monitors = hypr("monitors")?;
    let monitor = monitors
        .as_array()
        .context("No monitors")?
        .iter()
        .find(|m| m["focused"].as_bool() == Some(true))
        .context("No focused monitor")?;
    ensure!(
        monitor["dpmsStatus"].as_bool() == Some(true),
        "Display must be on"
    );
    let name = monitor["name"].as_str().context("No monitor name")?;
    let scale = monitor["scale"].as_f64().context("No monitor scale")?;
    let width = (monitor["width"].as_f64().context("No monitor width")? / scale).round() as u32;
    let height = (monitor["height"].as_f64().context("No monitor height")? / scale).round() as u32;
    ensure!(
        width >= 640 && height >= 400,
        "Test requires a 640×400 logical viewport"
    );
    let mut host = Host::start()?;
    host.request(
        json!({"action":"show","output":name,"scene":{"kind":"annotations","items":[
        {"kind":"rectangle","x":40,"y":80,"width":200,"height":100,"color":"#ff3366"},
        {"kind":"text","x":50,"y":60,"text":"Rust native overlay — 中文","color":"#ffffff"}]}}),
    )?;
    ensure!(host.mapped()?, "Annotation layer was not mapped");
    // A compositor may animate a new layer after its first frame callback.
    // Observe the actual screen until the annotation has finished fading in.
    let mut visible = false;
    for _ in 0..15 {
        let pixels = screenshot(name)?;
        let stroke = pixels.get_pixel((40.0 * scale) as u32, (110.0 * scale) as u32);
        if stroke[0] > 220 && stroke[1] < 100 {
            visible = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure!(
        visible,
        "Actual annotation pixels are absent after the compositor transition"
    );
    let mut model = json!({"action":"show","output":name,"scene":{"kind":"model3d","parts":[
        {"primitive":"box","position":[0,0,0],"rotation":[0,0,0],"scale":[2,2,2],"color":"#88aaff"}]}});
    host.request(model.clone())?;
    let mut pointer = Pointer::connect(name, width, height)?;
    pointer.move_to(width / 2, height / 2)?;
    let initial = center(&screenshot(name)?);
    pointer.drag(
        0x110,
        (width / 2, height / 2),
        (width / 2 + 100, height / 2 + 30),
    )?;
    let orbit = screenshot(name)?;
    ensure!(
        initial != center(&orbit),
        "Real pointer orbit did not change native pixels"
    );
    pointer.move_to(width / 2, height / 2)?;
    pointer.pointer.axis_source(wl_pointer::AxisSource::Wheel);
    pointer
        .pointer
        .axis_discrete(0, wl_pointer::Axis::VerticalScroll, -15.0, -1);
    pointer.flush()?;
    let zoom = screenshot(name)?;
    ensure!(
        model_area(&zoom) != model_area(&orbit),
        "Real pointer zoom did not change native pixels"
    );
    pointer.drag(
        0x111,
        (width / 2, height / 2),
        (width / 2 + 100, height / 2),
    )?;
    let pan = screenshot(name)?;
    ensure!(
        model_area(&zoom) != model_area(&pan),
        "Real pointer pan did not change native pixels"
    );
    pointer.click(width - 138, 25)?;
    let mut reset = false;
    for _ in 0..20 {
        if initial == center(&screenshot(name)?) {
            reset = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure!(reset, "Reset did not recover original native pixels");
    pointer.click(width - 50, 25)?;
    ensure!(!host.mapped()?, "Close left the native layer mapped");
    model["output"] = json!("");
    host.request(model)?;
    ensure!(
        host.mapped()?,
        "Show after Close did not recreate the layer"
    );
    host.request(json!({"action":"clear"}))?;
    ensure!(!host.mapped()?, "Clear left the native layer mapped");
    Ok(())
}

#[test]
fn desktop_command_collects_both_pipes_and_exit_status() -> Result<()> {
    let output = command_output(
        "sh",
        &["-c", "printf 'pixels'; printf 'diagnostic' >&2; exit 7"],
        Duration::from_secs(2),
    )?;
    assert_eq!(output.stdout, b"pixels");
    assert_eq!(output.stderr, b"diagnostic");
    assert_eq!(output.status.code(), Some(7));
    Ok(())
}

#[test]
fn desktop_command_times_out_and_reaps_an_unresponsive_capture() -> Result<()> {
    // Exercise both blocked pipe reads and a process that closes its pipes but stays alive.
    for script in [
        "printf '%s' \"$$\" > \"$1\"; exec sleep 60",
        "printf '%s' \"$$\" > \"$1\"; exec >/dev/null 2>&1; exec sleep 60",
    ] {
        let pid_file = tempfile::NamedTempFile::new()?;
        let error = command_output(
            "sh",
            &[
                "-c",
                script,
                "capture-fixture",
                pid_file.path().to_str().context("fixture path")?,
            ],
            Duration::from_millis(500),
        )
        .expect_err("Unresponsive capture must time out");
        assert!(error.to_string().contains("timed out"), "{error:#}");
        let pid: u32 = std::fs::read_to_string(pid_file.path())?.parse()?;
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "Timed-out capture {pid} was not reaped"
        );
    }
    Ok(())
}
