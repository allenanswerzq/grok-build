set shell := ["bash", "-euo", "pipefail", "-c"]

export RUSTC_WRAPPER := "kache"
jobs := "2"

# List the development commands.
default:
    @just --list

# Build the normal CLI and its Linux desktop overlay.
build: cli overlay

[private]
cli:
    cargo build -p xai-grok-pager-bin --locked -j {{ jobs }}

# Build the native overlay with kache and place it beside the CLI.
overlay:
    cargo build -p xai-grok-tools --bin grok-overlay-host --features native-overlay --locked -j {{ jobs }}

# Resolve/download overlay dependencies after a manifest change.
fetch:
    cargo fetch

# Build and launch the TUI with its default tools.
run: build
    ./target/debug/xai-grok-pager

# Check one crate; use the CLI composition root by default.
check crate="xai-grok-pager-bin":
    cargo check -p {{ quote(crate) }} --locked -j {{ jobs }}

# Format Rust sources.
fmt:
    cargo fmt --all

# Check Rust formatting without changing sources.
fmt-check:
    cargo fmt --all -- --check

# Run one crate's tests; desktop live tests are separately opted in.
test crate="xai-grok-agent":
    cargo test -p {{ quote(crate) }} --locked -j {{ jobs }}

# Run the screen/overlay unit tests without requiring a desktop.
test-screen:
    cargo test -p xai-grok-tools --lib --locked -j {{ jobs }} implementations::grok_build::screen

# Run native renderer/camera/input tests without requiring a desktop or GPU.
test-renderer:
    cargo test -p xai-grok-tools --bin grok-overlay-host --features native-overlay --locked -j {{ jobs }}

# Test Vulkan pixels and real Wayland interaction (requires an unlocked desktop).
test-native:
    cargo test -p xai-grok-tools --bin grok-overlay-host --test overlay_native_live --features native-overlay,live-tests --locked -j {{ jobs }} -- --nocapture --test-threads=1

# Lint the Rust Wayland host and renderer.
lint-native:
    cargo clippy -p xai-grok-tools --bin grok-overlay-host --test overlay_native_live --features native-overlay,live-tests --locked -j {{ jobs }} -- -D warnings

# Check screen-tool code, examples, and live-test compilation.
lint-screen:
    cargo clippy -p xai-grok-tools --lib --example screen --example overlay --test screen_agent_live --test screen_chrome_live --test overlay_desktop_live --features live-tests --locked -j {{ jobs }} -- -D warnings

# Call the real argument-free screen tool and save its screenshot for inspection.
inspect-screen screenshot="target/screen.png":
    cargo run -p xai-grok-tools --example screen --locked -j {{ jobs }} -- {{ quote(screenshot) }}

# Draw a scene through the real tool; Enter clears this example's session.
inspect-overlay scene output="": overlay
    cargo run -p xai-grok-tools --example overlay --locked -j {{ jobs }} -- {{ quote(scene) }} {{ if output == "" { "" } else { quote(output) } }}

# Lint one crate with warnings treated as errors.
lint crate="xai-grok-agent":
    cargo clippy -p {{ quote(crate) }} --locked -j {{ jobs }} -- -D warnings

# Generate affected-crate API documentation without documenting dependencies.
doc crate="xai-grok-tools":
    cargo doc -p {{ quote(crate) }} --no-deps --locked -j {{ jobs }}

# Exercise the changed queued-message and ANSI rendering PTY fixtures.
test-pty: build
    PAGER_BINARY="{{ justfile_directory() }}/target/debug/xai-grok-pager" cargo test -p xai-grok-pager-pty-harness --test pty_e2e_persistence --locked -j {{ jobs }} endline_park_is_markerless -- --ignored
    PAGER_BINARY="{{ justfile_directory() }}/target/debug/xai-grok-pager" cargo test -p xai-grok-pager-pty-harness --test scripted_scenarios --locked -j {{ jobs }} scripted_ansi_execute_output -- --ignored

# Build the whole Cargo workspace only when that scope is required.
build-workspace:
    cargo build --workspace --locked -j {{ jobs }}

# Exercise Chrome reading and desktop overlays with the built default agent.
test-live: build test-native
    GROK_BINARY="{{ justfile_directory() }}/target/debug/xai-grok-pager" cargo test -p xai-grok-tools --features live-tests --locked -j {{ jobs }} --test screen_agent_live --test screen_chrome_live --test overlay_desktop_live -- --nocapture --test-threads=1

# Show cache statistics for this checkout's latest build session.
cache:
    kache stats --last-build --root "{{ justfile_directory() }}" --full

# Explain misses in this checkout's latest build session.
cache-explain:
    kache explain --root "{{ justfile_directory() }}"
