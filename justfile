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

# Build the smallest grok-lean binary (nightly; rebuilds std, takes 15-20 minutes).
build-lean *flags:
    rustup run nightly-2026-10-04 cargo build -p grok-lean --locked -j {{ jobs }} \
        --profile lean --config crates/codegen/grok-lean/lean.toml "$@"
    @ls -lh target/x86_64-unknown-linux-gnu/lean/grok-lean

# Build grok-lean unoptimized, for quick testing; try the screen with --screen-example draw.
build-lean-debug *flags:
    cargo build -p grok-lean --locked -j {{ jobs }} "$@"
    @ls -lh target/debug/grok-lean

# Bump grok-lean, commit it, then push a matching tag to start the GitHub release.
release-lean version:
    #!/usr/bin/env bash
    set -euo pipefail

    release_version="${1#v}"
    release_tag="v$release_version"
    if [[ ! "$release_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]]; then
        echo "Version must look like 0.1.1 or v0.1.1." >&2
        exit 1
    fi

    if [[ "$(git branch --show-current)" != "main" ]]; then
        echo "Release from main." >&2
        exit 1
    fi
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "Commit or stash your changes before releasing." >&2
        exit 1
    fi

    git fetch origin main --tags
    if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]]; then
        echo "Local main must match origin/main before releasing." >&2
        exit 1
    fi
    if git rev-parse -q --verify "refs/tags/$release_tag" >/dev/null; then
        echo "Tag $release_tag already exists." >&2
        exit 1
    fi

    current_version="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/codegen/grok-lean/Cargo.toml | head -n1)"
    if [[ "$current_version" != "$release_version" ]]; then
        RELEASE_VERSION="$release_version" python3 - <<'PY'
    import os
    import re
    from pathlib import Path

    version = os.environ["RELEASE_VERSION"]

    manifest = Path("crates/codegen/grok-lean/Cargo.toml")
    text = manifest.read_text()
    text, count = re.subn(r'(?m)^version = "[^"]+"$', f'version = "{version}"', text, count=1)
    if count != 1:
        raise SystemExit("Could not update grok-lean version in Cargo.toml.")
    manifest.write_text(text)

    lock = Path("Cargo.lock")
    text = lock.read_text()
    text, count = re.subn(
        r'(\[\[package\]\]\nname = "grok-lean"\nversion = ")[^"]+(")',
        rf"\g<1>{version}\2",
        text,
        count=1,
    )
    if count != 1:
        raise SystemExit("Could not update grok-lean version in Cargo.lock.")
    lock.write_text(text)
    PY
        cargo metadata --locked --format-version 1 --no-deps >/dev/null
        git add crates/codegen/grok-lean/Cargo.toml Cargo.lock
        git commit -m "[build] Release grok-lean $release_tag"
        git push origin main
    fi

    git tag -a "$release_tag" -m "grok-lean $release_tag"
    git push origin "$release_tag"
    echo "Release workflow started for $release_tag."
    echo "Watch it with: gh run list --repo allenanswerzq/grok-build --workflow release.yaml --limit 1"

# Build the CLI, then launch the TUI with any TUI arguments.
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
