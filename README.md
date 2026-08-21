# Prisma

Zone-based LED pixel control software. A real-time pixel-mapping and effects engine for
LED installations, built around one architectural idea: **effects, dimmers and control are
fully independent per zone**, not bound to a single global output.

## The idea

Most pixel-mapping tools hold one global state and map layers onto fixture groups, but the
effects across those groups stay coupled. Running genuinely independent looks per zone — say,
sector A holding a static colour at 40% while sector B runs a chase — means pre-programming
every combination in advance.

Prisma treats zones as first-class. Each zone runs its own effect stack with its own
parameters, dimmer and colour, composited into a shared pixel buffer before output.
Triggering a whole scene is still one action; so is changing a single zone without touching
anything else.

## Status

Under active development. The engine, output and core UI are working:

- GPU render pipeline with per-zone effects, blend modes and overlap resolution
- Art-Net output on a fixed-interval thread, with a software virtual sink for testing
- Live 2D preview streamed from the engine to the UI
- Visual patch editor: fixture placement, zone assignment, DMX addressing
- Main operation window: per-zone scene grids, live faders, effect parameters

The live operation window (independent per-zone content, scene grids, global bar) is the
current focus. sACN/DDP output, MIDI, media sources and scripting are planned.

Design target is verified up to ~43,000 pixels (256 DMX universes), with the real reference
rig at ~18,000 pixels. Latency from input to visible output stays under a 50 ms budget.

## Stack

- **Engine** — Rust (no GC pauses, memory-safe concurrency)
- **Rendering** — wgpu (Vulkan / Metal / DX12) with WGSL shaders
- **Desktop shell** — Tauri (Rust engine + web UI in one binary)
- **UI** — React + TypeScript
- **Protocols** — Art-Net / sACN / DDP, hand-rolled over UDP for timing control

## Building and running

Requires a recent Rust toolchain and Node.js. On Linux you also need the WebKitGTK
development package (`webkit2gtk-4.1`).

Run the app in development:

```
npm --prefix ui install
cargo run --manifest-path app/Cargo.toml
```

Build installers (Linux):

```
cd app
npx @tauri-apps/cli build --bundles appimage,deb
```

The installers land in `app/target/release/bundle/`. For Windows builds, see
[docs/BUILD-WINDOWS.md](docs/BUILD-WINDOWS.md) — Tauri does not cross-compile, so Windows
installers are built on Windows.

## Repository layout

```
crates/
  prisma-core      engine model: clock, zones, compositor, parameter tree, show model
  prisma-render    wgpu pipeline, shaders, effects
  prisma-output    Art-Net / sACN / DDP and the virtual sink
  prisma-preview   master buffer to preview frames, plus the IPC transport
  prisma-patch     patch model and serialization
  prisma-midi      MIDI layer and device profiles
  prisma-script    Lua runtime
app/               Tauri shell
ui/                React frontend
shaders/           WGSL effect library
```

The workspace is split into focused crates. `prisma-core` has no dependency on the renderer,
so the engine model stays testable headlessly.

## License

Proprietary. All rights reserved.
