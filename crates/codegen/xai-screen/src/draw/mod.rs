//! `xai-screen draw`: show drawings above all windows.
//!
//! Each stdin line is the `screen_draw` tool's input as the model wrote it:
//! `{"shapes": [...], "repeat": "..."}`. A line is checked against the drawing rules and the
//! focused monitor, then shown on a Wayland overlay layer. Each line gets one reply line:
//! `{"monitor": "...", "width": ..., "height": ...}` once the compositor shows the drawing, or
//! `{"error": "..."}`. Closing stdin or killing the process removes the drawing.
//!
//! - [`model`]: what a drawing is: shapes, defaults, rules, and each shape at a given time.
//! - `paint` and `gpu`: how a drawing looks.
//! - `wayland`: the overlay surface, the playback clock, and pointer input.

mod gpu;
pub mod model;
mod paint;
mod wayland;

use std::io::{BufRead, Read, Write};

use anyhow::Result;
use serde_json::json;

use crate::hyprland::{self, Monitor};
use model::{Request, Style};

const MAX_REQUEST: u64 = 1024 * 1024;

pub fn run() -> Result<()> {
    wayland::run()
}

/// One stdin read.
enum Input {
    Line(Vec<u8>),
    Failed(String),
    End,
}

/// Pass each stdin line to `send` until stdin ends or fails, or `send` returns false.
fn forward_requests(mut send: impl FnMut(Input) -> bool) {
    let mut reader = std::io::stdin().lock();
    loop {
        let input = read_request(&mut reader);
        let done = !matches!(input, Input::Line(_));
        if !send(input) || done {
            break;
        }
    }
}

fn read_request(reader: &mut impl BufRead) -> Input {
    let mut line = Vec::new();
    match reader
        .by_ref()
        .take(MAX_REQUEST + 1)
        .read_until(b'\n', &mut line)
    {
        Ok(0) => Input::End,
        Ok(_) if line.len() as u64 > MAX_REQUEST => {
            Input::Failed("Drawing request exceeds 1 MiB".to_owned())
        }
        Ok(_) if line.last() != Some(&b'\n') => {
            Input::Failed("Incomplete drawing request".to_owned())
        }
        Ok(_) => Input::Line(line),
        Err(error) => Input::Failed(format!("Cannot read drawing request: {error}")),
    }
}

/// A checked drawing, with the monitor it goes on and the look it follows.
struct Placed {
    request: Request,
    monitor: Monitor,
    style: Style,
}

/// Parse a request line and check it against the drawing rules and the focused monitor.
fn place(line: &[u8]) -> std::result::Result<Placed, String> {
    let request: Request = serde_json::from_slice(line)
        .map_err(|error| format!("Invalid drawing request: {error}"))?;
    let monitor = hyprland::active_monitor().map_err(|error| format!("{error:#}"))?;
    if hyprland::locked() {
        return Err("The screen is locked".to_owned());
    }
    let (width, height) = monitor.logical_size();
    request.check(width, height)?;
    Ok(Placed {
        request,
        monitor,
        style: hyprland::style(),
    })
}

/// Answer the waiting request: the monitor once the drawing shows, otherwise why it cannot.
fn reply(answer: std::result::Result<&Monitor, &str>) -> Result<()> {
    let line = match answer {
        Ok(monitor) => {
            let (width, height) = monitor.logical_size();
            json!({ "monitor": monitor.name, "width": width, "height": height })
        }
        Err(error) => json!({ "error": error }),
    };
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &line)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
