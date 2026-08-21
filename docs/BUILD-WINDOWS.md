# Building Prisma for Windows

How to produce a Windows installer (`.exe` / `.msi`) for Prisma from a Windows machine —
typically a VM, since **Tauri cannot cross-compile Linux → Windows** (the Windows build needs
the MSVC toolchain and the Edge **WebView2** runtime, neither of which exists on Linux).

The Linux side (AppImage / `.deb`) is built on Linux; see the repo root and `tauri.conf.json`.
This document is only the Windows half.

> **Target:** a 64-bit Windows 10 or 11 VM (x86_64). Give it plenty of disk (the Rust build
> tree + toolchain is several GB) and RAM (8 GB+ recommended for a comfortable release build).

---

## 1. What you are building

- **NSIS installer** → `Prisma_0.1.0_x64-setup.exe` (recommended; small, familiar wizard).
- **MSI installer** → `Prisma_0.1.0_x64_en-US.msi` (WiX; good for managed/Group-Policy deploys).

Both wrap the same `prisma-app.exe`. The webview on Windows is **WebView2** (Chromium-based),
not WebKitGTK — so the WebKitGTK-specific behaviour we tuned on Linux (e.g. the mini-preview
canvas flicker) will behave differently, almost certainly better. **Windows has never been
tested**, so treat the first run as a verification pass (see §6).

---

## 2. Prerequisites (install once per VM)

Everything below can be installed with **winget** (built into Windows 10 21H2+ and Windows 11).
Run these in an **Administrator PowerShell**. Manual-download alternatives are noted.

### 2.1 Visual Studio 2022 Build Tools (MSVC + Windows SDK) — required for linking

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools --override `
  "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

This gives the **MSVC C++ compiler/linker** and the **Windows 10/11 SDK**. Without the
"Desktop development with C++" workload the Rust link step fails with `link.exe not found` or
missing SDK errors. (Manual: download "Build Tools for Visual Studio 2022" from Microsoft and
tick *Desktop development with C++*.)

> A full Visual Studio 2022 (Community) install also works — the Build Tools are just the
> minimal subset.

### 2.2 Rust with the MSVC toolchain

```powershell
winget install --id Rustlang.Rustup
# open a NEW terminal so PATH updates, then:
rustup default stable-x86_64-pc-windows-msvc
rustc -V   # confirm it prints a version
```

**Must be the `-msvc` toolchain**, not `-gnu`. Tauri/WebView2 assume MSVC.

### 2.3 Node.js LTS (builds the React UI)

```powershell
winget install --id OpenJS.NodeJS.LTS
# new terminal, then:
node -v ; npm -v
```

### 2.4 Git (to fetch the source; skip if you copy the folder in)

```powershell
winget install --id Git.Git
```

### 2.5 WebView2 runtime (to *run* the app, not to build it)

Windows 11 already ships it. On Windows 10 it is usually present via Edge; if not:

```powershell
winget install --id Microsoft.EdgeWebView2Runtime
```

> **WiX (for the `.msi`) and NSIS (for the `.exe`) do NOT need manual installation** — the
> Tauri CLI downloads them on first use, the same way `linuxdeploy` is fetched on Linux. The
> VM just needs internet access for that first build.

---

## 3. Get the source into the VM

**Option A — git clone** (if the repo is pushed to a remote):

```powershell
cd $HOME
git clone <your-repo-url> "Prisma Lighting Control"
cd "Prisma Lighting Control"
```

**Option B — shared folder / copy**: copy the whole project folder into the VM. You can
exclude the big build/output dirs to save time — they are regenerated:
`target/`, `app/target/`, `ui/dist/`, `ui/node_modules/`.

Either way you need the **entire repo** (the `app/` crate references `../crates/*` and `../ui`
by relative path), and the icon set under `app/icons/` (including `icon.ico`, needed by the
Windows bundler).

> **If you use Option A (git clone):** the icon set and the `tauri.conf.json` bundle config
> must be **committed and pushed first** — they were generated on the Linux side and are not in
> a fresh clone otherwise. If you use Option B (copy the folder), they are already there.
> Only the five desktop icons are used (`32x32.png`, `128x128.png`, `128x128@2x.png`,
> `icon.icns`, `icon.ico`); the `Square*Logo.png`, `android/` and `ios/` files that
> `tauri icon` also emits are for the Windows Store / mobile and can be ignored or removed.

---

## 4. Build

From the repo root, in a normal (non-admin) PowerShell:

```powershell
# 4.1 Install the UI's JS dependencies (once, and after ui/package.json changes)
npm --prefix ui install

# 4.2 Build the Windows installers.
#     Run the Tauri CLI from the app/ directory (that is where tauri.conf.json lives).
#     `--bundles nsis,msi` overrides the Linux-only targets in tauri.conf.json for this run.
cd app
npx --yes "@tauri-apps/cli@^2" build --bundles nsis,msi
cd ..
```

What happens:

1. `beforeBuildCommand` runs `npm --prefix ui run build` → produces `ui/dist` (the frontend).
2. `cargo build --release` compiles the engine + Tauri shell → `app/target/release/prisma-app.exe`.
   The first build is the slow one (compiles wgpu, Tauri and all crates); later builds are
   incremental.
3. Tauri downloads WiX/NSIS (first time only) and wraps the exe into the installers.

> If you only want the `.exe`: `--bundles nsis`. Only the `.msi`: `--bundles msi`.

---

## 5. Where the installers land

```
app\target\release\bundle\nsis\Prisma_0.1.0_x64-setup.exe
app\target\release\bundle\msi\Prisma_0.1.0_x64_en-US.msi
```

Copy those out of the VM to distribute/test. Installing either one puts Prisma in the Start
menu and Programs; the raw `app\target\release\prisma-app.exe` also runs standalone (handy for
a quick smoke test before packaging).

---

## 6. What to verify on the first Windows run

Windows has never run Prisma — check the things most likely to differ from Linux:

- **Boot splash** appears first (the animated pixel-P + "PRISMA LIGHTING CONTROL"), then
  hands over to the main window once the engine + UI are ready. The main window starts hidden
  and is revealed by the splash; if the splash never showed the main window, there is a 20 s
  safety net in `setup` that reveals it anyway (so a splash bug can't strand the boot).
- **App launches** and the main window opens (WebView2 present). If you get a blank white
  window, WebView2 runtime is missing → install it (§2.5).
- **Engine + live preview** work: faders move light, the zone-row mini-previews render, the
  scene grid fires effects. The mini-preview flicker we fixed on WebKitGTK should simply not
  occur on WebView2 — confirm it's clean.
- **The patch editor opens as a second window** and hot edits apply on close.
- **Local preview socket / firewall:** the preview WebSocket binds to `127.0.0.1` (loopback),
  which Windows Firewall does *not* prompt for. The **Art-Net output**, however, sends UDP to
  the network — the first time real output runs, Windows may show a firewall prompt; allow it
  on the correct network profile.
- **GPU / wgpu:** on a VM the GPU is often software/virtualized. wgpu should fall back, but the
  render leg may be slower than on real hardware — fine for a functional check, not for timing.

---

## 7. Code signing (optional, deferred)

Unsigned installers run, but Windows **SmartScreen** shows a "unknown publisher" warning and
some environments block them. For in-house use (Phase 1) this is acceptable. For distribution
(Phase 2) you need an **Authenticode code-signing certificate** (OV or EV). Tauri signs during
`build` when configured under `bundle.windows.certificateThumbprint` (+ `signCommand`/timestamp
URL) or via the `WINDOWS_CERTIFICATE` env vars. Leave this until there is a certificate to use;
it is a config addition, not a code change. (This is tracked with the licensing/protection
open decision in `CLAUDE.md` §9.)

---

## 8. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `link.exe not found` / missing SDK | VS Build Tools "Desktop development with C++" workload not installed (§2.1). |
| `error: toolchain 'stable-...-gnu'` / weird link errors | Wrong Rust toolchain — run `rustup default stable-x86_64-pc-windows-msvc` (§2.2). |
| `beforeBuildCommand` fails, `ui/package.json` not found | Run the CLI from `app/`, and run `npm --prefix ui install` first from the repo root (§4). |
| Blank white app window on launch | WebView2 runtime missing — install it (§2.5). |
| WiX/NSIS download fails | No internet on first build, or a proxy blocks GitHub — allow it; the tools are fetched once and cached. |
| `.msi` build fails but `.exe` works | WiX is stricter about the version/identifier; keep `version` as plain `x.y.z` and `identifier` reverse-DNS (already set in `tauri.conf.json`). Build just `--bundles nsis` if you don't need the MSI. |

---

## 9. Notes for later (CI)

The reproducible way to produce Windows builds without maintaining a VM is **GitHub Actions**
with a `windows-latest` runner (and `ubuntu-latest` for the Linux artifacts) — a matrix job
that runs the same `tauri build` per OS. When we set that up, this manual VM flow becomes the
fallback / local-debug path rather than the primary one.
