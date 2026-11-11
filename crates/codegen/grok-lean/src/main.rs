//! `grok-lean`: the grok TUI and agent with lean defaults.
//!
//! It runs the same pager, `MvpAgent`, sessions, and tools as `grok`, on jemalloc. Startup
//! skips the subcommands, update checks, crash reports, and plan mode, and a `GROK_CONFIG`
//! overlay selects the `grok-lean` agent and turns off services this agent does not use.

use std::time::Duration;

#[cfg(unix)]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// jemalloc's startup options. One arena instead of one per CPU cuts idle private memory about
/// fourfold; grok-lean runs few threads, so they rarely contend. `MALLOC_CONF` still overrides it.
#[cfg(unix)]
mod jemalloc_conf {
    /// jemalloc reads `const char *malloc_conf`, a thin pointer.
    #[repr(transparent)]
    struct ConfPtr(*const u8);
    // SAFETY: it points at an immutable static.
    unsafe impl Sync for ConfPtr {}
    static CONF: [u8; 10] = *b"narenas:1\0";
    #[cfg(not(target_os = "macos"))]
    #[used]
    #[unsafe(export_name = "malloc_conf")]
    static MALLOC_CONF: ConfPtr = ConfPtr(CONF.as_ptr());
    #[cfg(target_os = "macos")]
    #[used]
    #[unsafe(export_name = "_rjem_malloc_conf")]
    static MALLOC_CONF: ConfPtr = ConfPtr(CONF.as_ptr());
}

use anyhow::{Result, bail};
use xai_grok_pager::app::PagerArgs;
use xai_grok_pager::app::cli::SandboxStartup;
use xai_tty_utils::runtime::BlockingPool;

const WORKER_THREADS: usize = 2;
/// The blocking pool of every runtime in the process: the main one, the agent's, and each
/// session's. Threads start on demand and exit after 10 idle seconds. The shared policy starts
/// 16 per runtime and keeps them forever.
const BLOCKING_POOL: BlockingPool = BlockingPool {
    max_threads: 8,
    keep_alive: Duration::from_secs(10),
    prewarm: 0,
};
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Lean defaults, layered above the user's `config.toml`. A `GROK_CONFIG` the user sets is
/// merged on top, so any key can still be turned back on.
fn lean_config() -> serde_json::Value {
    let vendor_off =
        serde_json::json!({ "agents": false, "hooks": false, "mcps": false, "skills": false });
    serde_json::json!({
        "agent": { "name": "grok-lean" },
        "cli": { "use_leader": false, "auto_update": false, "session_registry": false, "show_tips": false },
        "ui": { "prompt_suggestions": false },
        "features": {
            "telemetry": false,
            "campaigns": false,
            "feedback": false,
            "codebase_indexing": false,
            "lsp_tools": false,
            "session_search": false,
            "session_recap": false,
            "turn_summary": false,
            "title_refresh": false,
            "voice_mode": false,
            "image_gen": false,
            "video_gen": false,
            "web_fetch": false,
            "ask_user_question": false,
        },
        "telemetry": { "trace_upload": false, "mixpanel_enabled": false },
        "memory": { "enabled": false },
        "memory_v2": { "enabled": false },
        "subagents": { "enabled": false },
        "managed_mcps": { "enabled": false },
        "plugins": { "enabled": [] },
        "dashboard": { "enabled": false },
        "compat": {
            "claude": vendor_off.clone(),
            "cursor": vendor_off,
            "codex": { "hooks": false, "skills": false },
        },
    })
}

fn main() {
    let args = PagerArgs::parse_cli();
    if args.version {
        println!("grok-lean {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let args = lean_args(args).unwrap_or_else(|error| exit_with(&format!("grok-lean: {error:#}")));
    xai_grok_pager_minimal::install();
    #[cfg(unix)]
    xai_grok_pager::memory_release::install_release_hook(purge_retained_pages);
    xai_crash_handler::install_terminal_restore_only();
    xai_tty_utils::runtime::set_blocking_pool(BLOCKING_POOL);
    let runtime = build_runtime().unwrap_or_else(|error| {
        exit_with(&format!("grok-lean: cannot start the runtime: {error}"))
    });
    let result = runtime.block_on(run(args));
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    if let Err(error) = result {
        xai_tty_utils::restore_native_stderr();
        let report = match error.downcast_ref::<xai_grok_pager::app::StartupFailure>() {
            Some(startup) => startup.user_report(),
            None => format!("Error: {error:#}"),
        };
        exit_with(&report);
    }
}

/// Keeps the interactive flags and fixes the ones lean mode decides. Runs before any thread
/// starts, because it sets process environment.
fn lean_args(mut args: PagerArgs) -> Result<PagerArgs> {
    if args.command.is_some() {
        bail!("grok-lean has no subcommands; use grok for them");
    }
    if args.single.is_some() || args.prompt_json.is_some() || args.prompt_file.is_some() {
        bail!("grok-lean runs only the interactive TUI; use grok for headless prompts");
    }
    args.leader = false;
    args.no_leader = true;
    args.no_plan = true;
    args.no_subagents = true;
    args.no_memory = true;
    args.no_ask_user = true;
    args.no_auto_update = true;
    args.disable_web_search = true;
    args.minimal = !args.fullscreen;
    let args = args.apply_cwd()?;
    // SAFETY: no other thread exists yet.
    unsafe {
        std::env::set_var("GROK_CONFIG", merged_config()?);
        if let Some(path) = args.debug_file.as_deref() {
            std::env::set_var("GROK_DEBUG_LOG", path);
        } else if args.debug {
            std::env::set_var("GROK_DEBUG_LOG", "1");
        }
    }
    Ok(args)
}

/// [`lean_config`] with the user's own `GROK_CONFIG` object merged over it.
fn merged_config() -> Result<String> {
    let mut config = lean_config();
    if let Ok(raw) = std::env::var("GROK_CONFIG")
        && !raw.trim().is_empty()
    {
        let user: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw)
            .map_err(|error| anyhow::anyhow!("GROK_CONFIG is not a JSON object: {error}"))?;
        merge(&mut config, serde_json::Value::Object(user));
    }
    Ok(config.to_string())
}

fn merge(base: &mut serde_json::Value, over: serde_json::Value) {
    match (base, over) {
        (serde_json::Value::Object(base), serde_json::Value::Object(over)) => {
            for (key, value) in over {
                merge(base.entry(key).or_insert(serde_json::Value::Null), value);
            }
        }
        (base, over) => *base = over,
    }
}

fn build_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.worker_threads(WORKER_THREADS).enable_all();
    xai_tty_utils::runtime::build_with_blocking_pool(&mut builder)
}

async fn run(mut args: PagerArgs) -> Result<()> {
    xai_grok_extra_ca::ensure_default_crypto_provider();
    xai_grok_shell::http::set_client_name(xai_grok_workspace::permission::ClientType::GrokPager);
    args.pin_local_resume_target()?;
    let saved_profile = args.saved_resume_profile();
    let sandbox = match args.startup_sandbox_profile(saved_profile.as_deref()) {
        SandboxStartup::Apply(profile) => profile,
        SandboxStartup::Conflict { requested, saved } => bail!(
            "cannot resume this session under sandbox profile '{requested}'; it was created with '{saved}'"
        ),
    };
    xai_grok_shell::config::apply_sandbox(None, sandbox.as_deref(), args.cwd.as_deref());
    let result = xai_grok_pager::app::run(args, None).await;
    xai_grok_sandbox::flush();
    result.map(drop)
}

/// Returns freed pages in every jemalloc arena to the OS (`4096` is `MALLCTL_ARENAS_ALL`).
/// The pager calls it after large frees, such as resuming a long session.
#[cfg(unix)]
fn purge_retained_pages() {
    use std::ptr::null_mut;
    // SAFETY: a NUL-terminated mallctl name that reads and writes nothing. A failure only
    // leaves the pages retained.
    unsafe {
        tikv_jemalloc_sys::mallctl(
            c"arena.4096.purge".as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            0,
        );
    }
}

fn exit_with(message: &str) -> ! {
    xai_grok_pager::best_effort_stderr::eprint_line(message);
    std::process::exit(1);
}
