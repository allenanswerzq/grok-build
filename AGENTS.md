# Local development

Use the root `justfile` for standard development commands. Its recipes keep Cargo
package selection, profiles, job count, and kache settings consistent.

- `just build`: build the CLI and Linux native overlay. Use this for app rebuilds.
- `just run`: build and launch the normal TUI, with screen tools enabled by default.
- `just check <crate>`, `just test <crate>`, `just lint <crate>`: validate the affected
  crate. Omit the crate to use each recipe's documented default.
- `just test-live`: run the Chrome/native/default-agent live checks. This requires
  managed Chrome, an unlocked desktop, and a powered-on display.
- `just test-screen`, `just test-renderer`, `just test-native`: validate the screen tools and renderer.
- `just test-pty`: exercise the queued-message and ANSI output fixtures against the built CLI.
- `just doc <crate>`: generate affected-crate API documentation.
- `just cache`, `just cache-explain`: inspect this checkout's cache activity.
- `just build-workspace`: use when full-workspace compilation is explicitly needed.

Do not alternate CLI-only and workspace-wide builds for ordinary app rebuilds;
Cargo feature unification changes dependency artifacts and reduces cache reuse.
Keep debug/release profiles and feature choices consistent. Add a named recipe for
a recurring workflow instead of inventing another build invocation.

The root `Cargo.toml` is generated and read-only. Preserve unrelated local changes.
