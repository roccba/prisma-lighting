# Prisma desktop shell (Tauri)

The Tauri shell that hosts the engine and, from M7 on, the patch-editor window.
It is the **only** place the engine is assembled outside a test: it owns a
[`prisma_engine::EngineHandle`] for the life of the process and tears it down in
order on exit.

## Why this is not a workspace member

Tauri v2 on Linux needs `webkit2gtk-4.1` and `gtk3` system libraries (plus a
frontend build). Headless CI here has neither, so this crate is listed under the
root workspace's `exclude` and is **not** built by `cargo build --workspace`,
`cargo fmt --all`, or `cargo clippy --workspace`. The engine assembly it depends
on (`prisma-engine`) *is* in the workspace and *is* fully tested headlessly
(`crates/prisma-engine/tests/boot_shutdown.rs`); this crate is the thin,
manually-verified shell on top of it.

## Building it (on a machine with the desktop toolchain)

System dependencies (verified on Fedora KDE, `fc44`):

```sh
sudo dnf install webkit2gtk4.1-devel gtk3-devel librsvg2-devel openssl-devel \
                 libappindicator-gtk3-devel curl wget file \
                 dbus-devel pkgconf-pkg-config \
                 glib2-devel cairo-devel pango-devel gdk-pixbuf2-devel atk-devel
sudo dnf group install "c-development"
```

If `libappindicator-gtk3-devel` is not found on your Fedora, use the Ayatana
replacement: `libayatana-appindicator-gtk3-devel`.

Debian/Ubuntu equivalents: `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev
libglib2.0-dev libcairo2-dev libpango1.0-dev libgdk-pixbuf-2.0-dev libatk1.0-dev
libdbus-1-dev pkg-config build-essential`.

Then, from the repo root:

```sh
cargo run --manifest-path app/Cargo.toml
```

The first build compiles ~480 crates (tauri, wry, webkit-sys) and takes a few
minutes. No Node or `cargo tauri` needed — the frontend is a static `ui/index.html`
and `build.rs` runs `tauri-build`. A 1280×800 "Prisma" window opens with the engine
running behind it; closing it prints the clean-shutdown line to the terminal.

`app/icons/icon.png` is required by Tauri at compile time and is committed (a
provisional placeholder — replace with real branding later).

## What S1 wires here

- **Engine ownership.** `setup` starts `Engine::start(...)` and stores the
  `EngineHandle` in Tauri managed state.
- **Ordered shutdown on exit.** `RunEvent::ExitRequested` calls
  `EngineHandle::shutdown()` *before* the process exits — the app-side half of the
  S1 shutdown criterion. The engine's `Drop` backstop covers any exit path that
  does not go through `ExitRequested` (a panic), so the process can never leak a
  running loop or a live GPU device.

## What is still to come

- **S2:** the IPC command surface (`patch_load`, `patch_validate`, `patch_apply_op`,
  …) over a Rust-side `PatchDraft`, and the editor window opened from the menu.
- **S3+:** the React canvas in `ui/`.
- **S7:** Highlight — the editor writing into the live output while the show is
  frozen; the seam is already the Parameter Tree + output stage owned by the engine.
