# Local development

Use the root `justfile` for standard development commands. Its recipes keep Cargo
package selection, profiles, job count, and kache settings consistent.

- `just build`, `just check`, `just test`, `just lint`, `just doc`: run Cargo on one
  package, the CLI by default. Name another package and add Cargo flags after it, for
  example `just test xai-grok-tools --lib -- screen_draw`.
- `just run`: build the CLI and launch the TUI. Arguments go to the TUI.
- `just test-live`: build the CLI and run `tests/*_live.rs` one at a time against it.
  This needs managed Chrome, an unlocked desktop, and a powered-on display. Flags
  replace the default selection, for example `just test-live xai-grok-tools --test screen_agent_live`.
- `just fmt`: format all Rust sources; `just fmt --check` only verifies.
- `just cache`, `just cache-explain`: inspect this checkout's cache activity.
- `just build-workspace`: use when full-workspace compilation is explicitly needed.

Do not alternate CLI-only and workspace-wide builds for ordinary app rebuilds;
Cargo feature unification changes dependency artifacts and reduces cache reuse.
Keep debug/release profiles and feature choices consistent. Target specific work by
passing a package and Cargo flags to the general recipes; do not add narrow recipes.

The root `Cargo.toml` is generated and read-only. Preserve unrelated local changes.
