# Game Manager

Native Windows 11 game launcher and library in Rust. It finds Steam, Epic, GOG, EA, Ubisoft and
Xbox games plus games added by hand, starts them, installs mods (ReShade, DXVK, LAA), swaps their
upscaler DLLs (DLSS, Streamline, FSR, XeSS) for other versions, and sets NVIDIA's driver-level
DLSS overrides in their driver profiles.

- `app/` – the window: Win32 and Direct2D (windows-window, windows-canvas, windows-text), no
  WinUI or XAML; see `app/CLAUDE.md`.
- `core/` (crate `gm-core`) – scanners, launching, configuration, graphics detection and mods,
  with no UI code; see `core/CLAUDE.md`.
- `config/games.json` – shipped per-game knowledge.
- `tools/7-Zip` – the shipped 7-Zip command line.
- **Doc comments**: every source file starts with a `//!` doc saying what it does and why. Read it
  before changing the file and keep it current. Details belong there, not in these CLAUDE.md
  files.

## Toolchain
- Rust 1.99.1 (`rust-toolchain.toml`), edition 2024, MSVC target.
- **windows-rs**: `windows`, `windows-registry`, `windows-core`, `windows-window`,
  `windows-canvas`, `windows-text` and `windows-threading` always come from
  https://github.com/microsoft/windows-rs master, never crates.io.
  - They are declared once in the root `[workspace.dependencies]`, and `Cargo.lock` pins the
    commit.
  - To update, run `cargo update -p windows -p windows-registry -p windows-core -p windows-window
    -p windows-canvas -p windows-text -p windows-threading`, then fix what the API changes break.
  - **Adding another windows-rs crate** moves them all to the latest master, since they share
    one git source. Put them back with `cargo update -p windows --precise <the locked commit>`.
  - The first fetch downloads the whole repository (about 1.4 GB).
- **No Windows App Runtime**: the window needs Windows 10 1703+ (DirectWrite, Direct2D 1.1) and
  the Visual C++ runtime (`VCRUNTIME140.dll`, which most PCs already have).
- **7-Zip**: `tools/7-Zip` holds `7z.exe`, `7z.dll` (26.05 x64) and `License.txt`.
  - It is GNU LGPL, and the license file must ship with the binaries.
  - `core/build.rs` copies the folder next to `app.exe`, together with `games.json`.
  - To update it, copy the same three files from a newer 7-Zip install.
- **Dependencies and lints**: every dependency's version is in the root
  `[workspace.dependencies]`.
  - `[workspace.lints]` denies `undocumented_unsafe_blocks` and `dbg_macro`, and warns on
    `multiple_unsafe_ops_per_block`, `unwrap_used`, `expect_used` and `todo`.
  - Tests may unwrap (`clippy.toml`); `build.rs` and `examples/scan.rs` allow `expect` with a
    reason.
- **Release profile**, tuned for speed: `opt-level = 3`, `lto`, `codegen-units = 1`,
  `panic = "abort"` (nothing catches an unwinding panic), `strip`. `app.exe` was 2.41 MB on
  2026-11-08 with My PC (2.28 MB before it; 1.68 MB with `opt-level = "s"`; the WinUI one
  5.93 MB). It links the C runtime dynamically (`VCRUNTIME140.dll`).

## Commands
- `cargo build`; `cargo run -p app`.
- `cargo run -p gm-core --example scan` checks the scanners without the UI.
  - `-- -v` prints launch commands, icons, programs and wiki links.
  - `-- --graphics` prints the wiki's and the files' APIs, about 2 s a game.
  - `-- --icons <dir>` saves every icon as a PNG.
  - `-- --nvidia` prints the driver's version and DLLs, then each game's NVIDIA driver profile
    with its Present Method, flags and DLSS overrides (read only).
  - `-- --pc` prints My PC instead of the games: hardware, displays with their modes, colour
    profiles and brightness, and Windows' game switches (read only).
  - `-- --covers` prints where each game's cover comes from, or would be looked for (read only).
  - To verify bitness, compare a `program` with `GetBinaryTypeW` and `dumpbin /headers`. Parse
    scan lines from the right, as names can be wider than their column.
- **Before finishing a change**: `cargo fmt`, `cargo clippy --all-targets` (no warnings),
  `cargo test`.

## Checking the app from an agent session
- **No screen capture**: inspect the running window with UI Automation
  (`System.Windows.Automation` in PowerShell), and see it as a PNG: start it with
  `GAME_MANAGER_CAPTURE=<path>` and post `WM_APP + 9` to its window (`app/CLAUDE.md`).
  - The Open dialog (class `#32770`) is a child of the app's window in the UI Automation tree,
    not a top-level window. It shows its Win32 controls as plain panes. Take the
    `NativeWindowHandle` of the last element with AutomationId `1148` (the `Edit` inside the
    combo boxes) for `WM_SETTEXT`, then `PostMessage` `BM_CLICK` to the AutomationId `1` element
    that has a window handle (Open; an item of the file list has the same id).
- **No input desktop**: drag and drop and the UAC prompt can't be done (the cursor reads 0,0 and
  there is no foreground window). Ask the user to try them on a build they run. Right-clicks and
  keys can be posted to the Direct2D window (`WM_RBUTTONDOWN`/`WM_RBUTTONUP`, `WM_KEYDOWN`).
  ReShade's Vulkan layer always needs the UAC prompt, so the user tests it.
- **Packaged session**: programs started from the Claude desktop app run inside its package.
  - Their `%LOCALAPPDATA%` writes are redirected to `AppData\Local\Packages\Claude_…\LocalCache`,
    and so are `%APPDATA%` writes (seen in PowerShell and the app, not in Git Bash, which reads
    the real files): compare the user's files from both before calling them untouched.
  - Deleting a real folder there fails with os error 4395 (seen with an old DXVK cache folder).
  - The user's own runs are not redirected.
- **Live downloads**: `cargo test -p gm-core live_builds -- --ignored` fetches each ReShade
  variant and the default effects; `live_upscalers` one version of each upscaler DLL.
- **The user's files**: back up `%APPDATA%\Game Manager\*.json` before a check and restore them
  after, comparing first that only the test changed them.
- **My PC changes the user's own PC**: its switches write their registry (note the values and put
  them back exactly), and display modes, HDR, colour profiles, brightness and GPU scheduling are
  for the user to try (`app/CLAUDE.md`).
- **The user's window**: never close or kill it. If `target\debug\app.exe` is locked, build into
  `CARGO_TARGET_DIR=target\agent`.
- **Mods**: never install into the user's real games, whose folders may hold their own DXVK or
  ReShade. Use a throwaway custom game instead:
  - Set up a temp folder with a copy of `SysWOW64\notepad.exe` as `game.exe` and a fake
    `d3d9.dll`.
  - Add an entry to `custom-games.json` and `"graphicsApi": "DirectX9"` to `game-settings.json`.
  - Drive Install, Settings, Reinstall and Uninstall, then compare the folder with how it started.
  - `app.exe --file-ops <percent-encoded ops>` can be run directly (not elevated).
  - **Upscalers**: give the throwaway game real old vendor DLLs (from the upscaler cache after
    `live_upscalers`) in a subfolder, then swap, swap again and restore, and compare the folder.
  - **Deploy in ReShade ▸ Shaders, and Apply in (or leaving) ReShade ▸ Add-ons, with "All
    games with ReShade" reach every game with ReShade in `mods.json`** that has no choice of its
    own, the user's real ones too. Before clicking them, check that only throwaway games have a `reshade` record (or test
    with a `mods.json` that holds only them). "Only <game>" reaches just that game.

## Conventions
- **Background work**: blocking work (disk, registry, network) never runs on the UI thread.
- **Game data**: no game-specific data in Rust code. Knowledge about particular games goes into
  `config/games.json` (matched by binaries, never store ids); the user's choices go into
  `game-settings.json`.
- **Errors and `unsafe`**: typed errors, one unsafe operation per block with `// SAFETY:`, and
  RAII owners for handles (details in `core/CLAUDE.md`).
- **No GDI**: the window is drawn with Direct2D and DirectWrite only, text boxes included; no
  child windows. The core keeps `wingdi` only for display settings' structures (`DEVMODEW`), which
  draw nothing.
