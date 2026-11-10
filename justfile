set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

export RUSTC_WRAPPER := "kache"
jobs := "6"
cli := "xai-grok-pager-bin"

# Recipes take a Cargo package, then any extra Cargo flags:
#   just test xai-grok-tools --lib -- screen_draw
#   just check xai-grok-tools --all-targets

# List the development commands.
default:
    @just --list

# Build a package; the default is the CLI.
build package=cli *flags:
    cargo build -p "$1" --locked -j {{ jobs }} "${@:2}"

# Build the CLI and launch the TUI with any TUI arguments.
run *args: build
    ./target/debug/xai-grok-pager "$@"

# Type-check a package without building binaries.
check package=cli *flags:
    cargo check -p "$1" --locked -j {{ jobs }} "${@:2}"

# Run a package's tests. Put test filters and harness flags after --.
test package=cli *flags:
    cargo test -p "$1" --locked -j {{ jobs }} "${@:2}"

# Run live tests (default: tests/*_live.rs) against the built CLI on an unlocked desktop.
test-live package="xai-grok-tools" *flags: build
    flags=("${@:2}"); [ ${#flags[@]} -gt 0 ] || flags=(--test '*_live'); \
    GROK_BINARY="{{ justfile_directory() }}/target/debug/xai-grok-pager" \
    RUST_TEST_THREADS=1 RUST_TEST_NOCAPTURE=1 \
    cargo test -p "$1" --features live-tests --locked -j {{ jobs }} "${flags[@]}"

# Lint a package with warnings treated as errors.
lint package=cli *flags:
    cargo clippy -p "$1" --locked -j {{ jobs }} "${@:2}" -- -D warnings

# Generate a package's API documentation without its dependencies.
doc package=cli *flags:
    cargo doc -p "$1" --no-deps --locked -j {{ jobs }} "${@:2}"

# Format all Rust sources; pass --check to only verify.
fmt *flags:
    cargo fmt --all "$@"

# Download dependencies after a manifest change.
fetch:
    cargo fetch

# Build the whole Cargo workspace only when that scope is required.
build-workspace:
    cargo build --workspace --locked -j {{ jobs }}

# Show cache statistics for this checkout's latest build session.
cache:
    kache stats --last-build --root "{{ justfile_directory() }}" --full

# Explain misses in this checkout's latest build session.
cache-explain:
    kache explain --root "{{ justfile_directory() }}"
