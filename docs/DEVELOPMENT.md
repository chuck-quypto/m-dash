# Development and packaging

Release targets are Windows x64 and Apple Silicon macOS. Linux, Intel/universal
Mac builds, Android, and mobile relay delivery are not qualified release targets.

## Prerequisites and checks

Use Node.js 24+, npm (`package-lock.json`), stable Rust, and
[Tauri's prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS.
Windows needs MSVC C++ build tools and WebView2; Mac needs Xcode command-line tools.
Provider CLIs are separate runtime dependencies, never bundled. Unit tests and
synthetic previews need no accounts.

Run the checks below before opening a pull request; CI runs the same set on
Windows, macOS and Ubuntu. Official release artifacts are built as described in
[RELEASING.md](../RELEASING.md). A local package is a development and
installed-behavior check, not an official release.

```sh
npm ci
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib
cargo test --manifest-path sync-core/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml --check
node --test scripts/windows-rc.test.mjs scripts/mac-rc.test.mjs scripts/release-notes.test.mjs
node scripts/check-release-docs.mjs
```

Run `npm run tauri -- dev` for the native app. For synthetic data, run `npm run dev`
and open `http://127.0.0.1:1420/preview/`. The preview does not read accounts or save
native settings. Opening `/` in an ordinary browser correctly shows the installed
desktop application notice because it has no native collector.

`node scripts/check-startup.mjs` checks startup/recovery against a running Vite
server using Playwright installed separately. Set `MDASH_PLAYWRIGHT_MODULE` to its
module URL if outside this project; `MDASH_PREVIEW_URL` overrides the server origin.
Browser evidence is separate from native installed acceptance.

## Windows package

For a local development package:

```powershell
npm run tauri -- build --target x86_64-pc-windows-msvc --bundles nsis -- --locked
```

The executable is under `src-tauri/target/x86_64-pc-windows-msvc/release/`; setup
is under its `bundle/nsis/` directory. Installation is per user. Setup may download
WebView2 if absent. Open setup in normal File Explorer; a packaged development host
can redirect app-data paths. Quit the installed app before upgrading.

## Mac package

Build on a Mac; a Windows source check is not Mac release qualification.

```sh
rustup target add aarch64-apple-darwin
npm run tauri -- build --config src-tauri/tauri.macos.conf.json --target aarch64-apple-darwin --bundles app,dmg
```

Outputs are under `src-tauri/target/aarch64-apple-darwin/release/bundle/macos/` and
`bundle/dmg/`. The configured macOS 12 build minimum is not proof of testing every
OS version. Mac uses system WebKit.

Official binaries use the maintainer's Developer ID, hardened runtime, Apple
notarization and stapled tickets, and are built only by the
[Release workflow](../RELEASING.md). Never commit signing credentials. Verify the final app with `codesign --verify --deep --strict`,
the app/DMG with `xcrun stapler validate`, and both with Gatekeeper assessment.
Unsigned developer builds are not official signed downloads.

For the current installed development build, owner walkthrough and remaining scope, see
[Mac acceptance](development/MAC_ACCEPTANCE.md).

## Ubuntu development package

The Linux port targets Ubuntu 26.04 x86_64 for local native verification.
It uses GTK/WebKitGTK, libsecret's desktop Secret Service and Zenity's native
masked dialog. Install the Tauri Linux build prerequisites plus `libsecret-1-dev`,
`libdbus-1-dev`, `zenity`, `libglib2.0-bin` and `xdg-utils`; the app itself needs
neither Rust nor Node. The `.deb` declares its native dialog, keyring, D-Bus and
browser-opener runtime utilities. Use Node 24+ and
the locked graph; the Linux notices currently require Rust 1.94.0.

```sh
node scripts/generate-notices.mjs x86_64-unknown-linux-gnu
node --test scripts/linux-packaging.test.mjs
npm run tauri -- build --config src-tauri/tauri.linux.conf.json --bundles deb -- --locked
```

For local work, add an untracked config overlay with a distinct development version
such as `0.4.0-dev.ubuntu.1`; this is not an official RC. Inspect the `.deb`, quit
and back up the resolved XDG application-data directory before installing or
reinstalling. Keep test evidence in ignored `.local/`. Run
`python3 scripts/check-linux-no-tray.py /absolute/path/to/m-dash` in the real desktop
session to check close without a tray using disposable settings and D-Bus.
The fixture selects and cleans up only its own process group: GNOME's separate
X11 frame client can have the same window title as the dashboard.
After minimizing on GNOME, `python3 scripts/check-linux-compact.py` checks the
actual 528×88 strip geometry through the installed desktop window-control bridge.
With the installed dashboard open, `python3 scripts/check-linux-restore.py`
checks strip, tray, second-launch and close-to-tray restoration. It requires GI
Atspi/Gio, Pillow, xdotool, wmctrl, the GNOME bridge, connected monitors and another
open desktop application. It
asserts activation plus the actual rendered brand header; mapped state alone
does not prove restoration. Linux restores with GTK on the UI thread because Tao
can skip focus while its cached minimized/visible state is stale. X11 presentation
uses a fresh server timestamp: tray and second-launch requests have no GTK input
event, and a cached timestamp can fail GNOME's activation check.
Run `node --test scripts/linux-rc.test.mjs` for the Linux RC preflight/staging
checks. The [Linux RC workflow](../.github/workflows/linux-rc.yml) is for maintainer
release candidates, not routine development.
Linux startup defaults to X11/XWayland when `DISPLAY` exists and `GDK_BACKEND`
is unset, before GTK/worker initialization. The launcher must be checked with
`GDK_BACKEND` removed from its environment: a shell that already sets it to `x11`
masks the native Wayland minimized-strip failure. Explicit backend
overrides are preserved. Native Wayland strip parity and a separate Xorg login
remain unqualified. Other distributions, architectures and Linux package formats
remain outside this work.

## Notices and payload

Follow [licenses/README.md](../licenses/README.md) for target-specific generation
and source obligations. Generate notices on each packaging host with its actual
locked dependencies and toolchain. Keep upstream license text unchanged.

Bundle LICENSE, NOTICE, BRAND-LICENSE.txt, the user guide, privacy statement, and
matching third-party notices. Inspect the actual installer/mounted DMG. Never
include databases, real-account captures, signing material, or `.local/`.
The [README header source](assets/readme-header.html) uses only local brand assets;
render it with a browser after the fonts load and capture the header element to PNG.

## Experimental mobile code

`mobile/`, `relay/`, and `sync-core/` remain available for development. This does not
promise an Android release or hosted relay. See [mobile/README.md](../mobile/README.md)
and the [development protocol](development/SYNC_PROTOCOL.md). A fresh desktop
install contains no configured relay endpoint or enrollment credential.
Mobile connections require separate setup and explicit consent.
