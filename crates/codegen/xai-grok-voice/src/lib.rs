//! Voice input for Grok Build CLI: an xAI streaming STT client and the [`run_voice_pipeline`] task that emits [`VoiceEvent`]s for the pager.
//!
//! Voice is dictation only: the mic streams to STT and the transcript lands in the prompt box.
//!
//! On macOS and Linux the mic is opened in a short-lived subprocess, so the long-lived TUI never pays the audio stack's permanent memory cost.
//! See [`audio`] and [`ARG0_ENTRY`].

#![deny(clippy::indexing_slicing)]

#[cfg(feature = "audio")]
pub mod audio;
pub mod auth;
// The clip route only runs with capture compiled in; its tests need no device.
#[cfg(any(test, feature = "audio"))]
mod clip;
pub mod config;
pub mod error;
pub mod event;
pub mod language;
pub mod pipeline;
pub mod probe;
pub mod stt;
pub mod transcriber;
#[cfg(any(test, feature = "audio"))]
mod wav;

pub use auth::{SharedVoiceAuth, StaticVoiceAuth, VoiceAuthError, VoiceAuthProvider};
pub use config::VoiceConfig;
pub use error::VoiceError;
pub use event::{TaggedVoiceEvent, VoiceEvent, VoiceRoute, VoiceSessionId};
pub use language::{
    STT_LANGUAGE_AUTO, STT_LANGUAGE_DEFAULT, STT_LANGUAGES, SttLanguage, canonicalize_stt_language,
    language_for_api, stt_language_by_code,
};
pub use pipeline::{SttRoutes, VoiceCommand, run_voice_pipeline};
#[cfg(feature = "audio")]
pub use probe::run_mic_only_probe;
pub use probe::{
    InputDeviceInfo, VoiceProbeOptions, VoiceProbeReport, format_probe_report, input_device_info,
    run_streaming_probe,
};
pub use transcriber::{
    AudioClip, ClipTranscriber, FINAL_TIMEOUT, SharedClipTranscriber, TranscribeError,
};

/// Linux shells out to a system recorder (`pw-record`/`parec`/`arecord`) so the static-musl binary links no audio
/// library. On Linux a `true` value means capture is *compiled in*; whether a recorder is actually installed is reported
/// when a session starts. Consumers gate voice on this so a no-audio build never advertises a mic it can't open.
/// The [`ClipTranscriber`] seam is exported regardless; without capture nothing ever calls it.
pub const AUDIO_SUPPORTED: bool = cfg!(feature = "audio");

/// Arg0 entry name consumers re-exec themselves with to capture microphone audio in a short-lived helper process on macOS.
/// See [`audio::capture_subprocess`](audio) for why capture is out of process.
/// Runs [`ARG0_ENTRY`], which the binary registers at the very top of `main`, before any TUI/agent/tokio init, so the child stays minimal.
pub const MIC_CAPTURE_SUBCOMMAND: &str = "--mic-capture";

/// The hidden mic-capture helper. Register it at the top of `main` with `xai_arg0::dispatch` in every binary
/// that links this crate with `audio` (the pager composition root and `voice-probe`).
pub const ARG0_ENTRY: xai_arg0::Entry = xai_arg0::Entry {
    name: MIC_CAPTURE_SUBCOMMAND,
    run: run_capture_helper,
};

fn run_capture_helper(args: Vec<std::ffi::OsString>) -> i32 {
    #[cfg(all(feature = "audio", not(target_os = "linux")))]
    {
        let args = args
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        audio::run_capture_child_cli(args)
    }
    #[cfg(not(all(feature = "audio", not(target_os = "linux"))))]
    {
        // This build's own parent backend never spawns the helper (Linux uses system recorders; no-audio builds have no capture)
        // Only a hand-typed invocation reaches here
        // `write!` instead of `println!` so a closed pipe never panics
        use std::io::Write;
        let _ = args;
        let _ = writeln!(
            std::io::stdout(),
            "ERR mic-capture helper unavailable in this build"
        );
        2
    }
}
