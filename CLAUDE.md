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
  - `-- --icons <dir>` saves each icon as the app gets it (its best image, `.png` or `.ico`).
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
The `check-app` skill (`.claude/skills/check-app/SKILL.md`) builds, starts, drives and captures
the window, guards the user's files, and holds the details (UI Automation, posted input, the
Open dialog, the packaged session, mods with a throwaway game, icon diagnostics). Never break
these:
- **The user's window**: never close or kill it. Build into `CARGO_TARGET_DIR=target\agent` and
  close only `target\agent\debug\app.exe`.
- **The user's files**: back up `%APPDATA%\Game Manager` before a check and restore it after,
  comparing first that only the test changed it. Programs started from the Claude desktop app
  write to a packaged copy, so compare from both views (PowerShell and Git Bash).
- **The clipboard is the user's**: save its text before a Cut, Copy or Paste check and put it
  back after.
- **My PC changes the user's own PC**: note the registry values its switches write and put them
  back exactly; display modes, HDR, colour profiles, brightness and GPU scheduling are for the
  user to try.
- **Mods**: never install into the user's real games; use a throwaway custom game. "All games
  with ReShade" (Deploy in Shaders, Apply in Add-ons) reaches the user's real games too.
- **The game menu's Uninstall…** hands the game to its store for real: open its dialog and
  Cancel.
- **What the user tries**: drag and drop, the UAC prompt (ReShade's Vulkan layer), Shift+F10, and
  DXVK when its cache holds an older layout. There is no input desktop here.

## Conventions
- **Background work**: blocking work (disk, registry, network) never runs on the UI thread.
- **Game data**: no game-specific data in Rust code. Knowledge about particular games goes into
  `config/games.json` (matched by binaries, never store ids); the user's choices go into
  `game-settings.json`.
- **Errors and `unsafe`**: typed errors, one unsafe operation per block with `// SAFETY:`, and
  RAII owners for handles (details in `core/CLAUDE.md`).
- **No GDI**: the window is drawn with Direct2D and DirectWrite only, text boxes included; no
  child windows. Pictures are decoded with WIC, game icons too (read from `.ico` files and
  programs' resources, never through icon handles). The core keeps `wingdi` only for display
  settings' structures (`DEVMODEW`), which draw nothing.
