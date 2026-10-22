# Building scry

`scry` is a single Rust binary. It builds on macOS, Windows and Linux with
Rust 1.94+ (pinned in `rust-toolchain.toml`).

```sh
cargo build --release      # target/release/scry
cargo test
```

CI (`.github/workflows/ci.yml`) builds and tests on all three platforms on every
push; prefer building each OS natively on its own runner over cross-compiling.

## Platform notes

### macOS
No system packages needed. The embedded sign-in window uses WKWebView (via `wry`).

### Windows
No extra packages with the MSVC toolchain (the default). The embedded sign-in
window uses WebView2, which ships with Windows 11 / recent Windows 10.

### Linux
The sign-in window (`wry`/WebView2/WKWebView) is **not** built on Linux; sign-in
falls back to the system browser (paste the redirect URL, or it is read from the
clipboard). So no WebKitGTK is required. The window, GPU scaling and clipboard do
need X11/Wayland and graphics headers:

```sh
sudo apt-get install -y \
  libxkbcommon-dev libwayland-dev libxcb1-dev \
  libx11-dev libxrandr-dev libxi-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

At runtime the viewer needs a Vulkan or OpenGL driver (`wgpu`); on a headless box
run the non-viewer commands (`login`, `list`, `session` without `--viewer`).

## Cross-compiling to Windows from macOS (optional)

Used only for a quick local check; release artifacts come from CI.

```sh
brew install mingw-w64
rustup target add x86_64-pc-windows-gnu
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
  cargo build --target x86_64-pc-windows-gnu
```

The GNU target is for convenience; CI produces the MSVC build. If the C
compilers are killed on first run (`signal: 9`), clear macOS quarantine once:
`xattr -dr com.apple.quarantine /opt/homebrew/Cellar/mingw-w64`.

## Desktop app

Running `scry` with **no subcommand** opens a small launcher window (all platforms):
it signs you in, lists your Cloud PC(s) and opens the viewer on *Connect*. The launcher
is egui/wgpu (no GTK/WebKit), so it needs only the libraries the viewer already needs.
The CLI subcommands (`login`, `list`, `session …`, etc.) still work unchanged.

### macOS .app bundle

```sh
scripts/make-macos-app.sh       # -> dist/scry.app
open dist/scry.app              # or drag it into /Applications
```

The bundle embeds a generated icon (`scry icon <file.png>` renders it). A locally
built app is not quarantined; a downloaded one needs a right-click → Open once.

### Windows / Linux

On Windows the release `scry.exe` is itself the launcher when run with no arguments
(pin it to the taskbar). On Linux the launcher works too (egui/wgpu); only the embedded
sign-in window is macOS/Windows — on Linux sign-in falls back to the system browser.
