# Development

## Requirements

* Rust **1.92+** (stable), required by Slint 1.18. Install via `rustup`.
* A C toolchain (`base-devel` on Arch) for build scripts.
* System libraries used by Slint's default winit backend:

  | Library | Arch package | Used |
  |---|---|---|
  | fontconfig, freetype | `fontconfig`, `freetype2` | linked |
  | xkbcommon, Wayland client | `libxkbcommon`, `wayland` | loaded at runtime |
  | X11 (+ cursor, randr, xi) | `libx11`, `libxcursor`, `libxrandr`, `libxi` | loaded at runtime under X11/XWayland |
  | OpenGL / EGL | `libglvnd`, `mesa` (or your GPU driver) | loaded at runtime |

```bash
sudo pacman -S --needed base-devel rustup fontconfig freetype2 libxkbcommon \
    wayland libx11 libxcursor libxrandr libxi libglvnd mesa
rustup default stable
```

No Node.js or other frontend tooling is involved. `.slint` files are compiled
to Rust by `build.rs` through `slint-build` during `cargo build`.

## Everyday commands

```bash
cargo run                                   # debug build, simulation mode
RUSTCLICK_CONFIG_DIR=/tmp/rc cargo run      # isolated configuration
RUST_LOG=rustclick=debug cargo run          # verbose logs (stderr)

cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo run --example screenshots -- target/shots 1120 760
```

There is a single crate with no Cargo features, so `--all-features` is
equivalent to the default.

### Rendering backends

Slint picks a backend at runtime. If the GPU path misbehaves (e.g. in a VM),
use the software renderer: `SLINT_BACKEND=winit-software cargo run`. To
force X11 under Wayland, use `WINIT_UNIX_BACKEND=x11` or unset `WAYLAND_DISPLAY`.

### Tests

| Location | What |
|---|---|
| `src/**` `#[cfg(test)]` | Unit tests: normalization, device filtering, mappings, target state machine, router, config validation, mock providers |
| `tests/routing.rs` | Routing through the service: selected target only, disabled devices, duplicates, mappings, multiple devices, bounded queue, simulated→native refusal |
| `tests/target_lifecycle.rs` | Auto-selection, ambiguity prompts, window closure, replacement, restart matching, stale snapshots |
| `tests/configuration.rs` | File persistence, invalid and future configs, persistence across restarts |
| `tests/ui_state.rs` | Errors and unsupported capabilities reflected in the view model |
| `tests/ui_controller.rs` | Real Slint UI on the headless testing backend: callbacks → service → UI |

`i-slint-backend-testing` is pinned to the same version as `slint`; update
`slint`, `slint-build` and `i-slint-backend-testing` together.

## Project layout

```text
build.rs                 compiles ui/main.slint (fluent style)
ui/                      Slint UI: main window, pages, dialog, widgets, AppState global
src/main.rs              binary: load config, start service thread, run UI
src/lib.rs               library root (everything testable)
src/controller.rs        UI glue: callbacks → Command, ViewModel → properties
src/app.rs               AppService, Command, ViewModel
src/core/                platform-independent logic (see architecture.md)
src/input/               InputProvider trait; mock and native providers
src/targets/             TargetProvider trait; mock and native providers
src/platform/            per-OS capability reports
src/provider.rs          ProviderMessage, ProviderSink, ProviderError
src/storage.rs           config location, atomic save, invalid-file handling
src/logging.rs           tracing subscriber with runtime level changes
examples/screenshots.rs  headless PNG rendering of every page
```

## Release builds and packaging

```bash
cargo build --release      # target/release/rustclick (thin LTO, stripped)
```

The release binary needs only the runtime libraries listed above. On
Windows, release builds use the GUI subsystem (no console window).

* **Arch Linux:** a `PKGBUILD` would `cargo build --release --locked`, install
  the binary to `/usr/bin/rustclick`, and depend on `fontconfig freetype2
  libxkbcommon wayland libx11 libglvnd`. Add a `.desktop` file once an icon
  exists.
* **Windows:** ship `rustclick.exe` (MSVC target). An installer (WiX/MSIX) and
  code signing are needed to avoid SmartScreen warnings.
* **macOS:** an `.app` bundle (e.g. `cargo bundle`), signed with a Developer
  ID and notarized. This is mandatory once native input needs Input
  Monitoring and Accessibility, because permissions are tied to the signature.

## Cross-platform builds

Building on Arch does **not** validate Windows or macOS packages.

* The platform modules (`src/platform/windows.rs`, `macos.rs`) contain no OS
  API calls yet, so they compile anywhere their target's std is available.
  CI builds and tests on Windows and macOS runners for that reason.
* Cross-checking Windows from Arch:
  `rustup target add x86_64-pc-windows-gnu` (MinGW: `mingw-w64-gcc`), then
  `cargo check --target x86_64-pc-windows-gnu`. Produce release artifacts
  with the MSVC toolchain on Windows (or `cargo-xwin`).
* macOS cannot be legally cross-compiled without the Apple SDK. Build, sign
  and notarize on macOS (CI runner).
* Native behavior must be validated on each real OS before claiming support
  (see [native-input.md](native-input.md)).

## Conventions

* Keep platform-specific code out of `core` and out of `.slint` files.
* New providers implement `InputProvider` / `TargetProvider` and report
  capabilities honestly. Never return `Delivered` or `Supported` for
  something that has not been verified.
* UI changes: run the screenshots example at default size and at the minimum
  window size (900×600) and look at the result. It writes each page in the
  light and dark color schemes (`*-dark.png`).
