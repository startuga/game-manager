# Game Manager

Native Windows 11 game launcher and library in Rust. It finds Steam, Epic, GOG, EA and Xbox
games plus games added by hand, starts them, and installs mods (DXVK).

- `app/` – the WinUI 3 window (windows-reactor); see `app/CLAUDE.md`.
- `core/` (crate `gm-core`) – scanners, launching, configuration, graphics detection and mods,
  with no UI code; see `core/CLAUDE.md`.
- `config/games.json` – shipped per-game knowledge.
- `tools/7-Zip` – the shipped 7-Zip command line.
- **Doc comments**: every source file starts with a `//!` doc saying what it does and why. Read it
  before changing the file and keep it current. Details belong there, not in these CLAUDE.md
  files.

## Toolchain
- Rust 1.99.1 (`rust-toolchain.toml`), edition 2024, MSVC target.
- **windows-rs**: `windows`, `windows-registry`, `windows-reactor` and `windows-pickers` always
  come from https://github.com/microsoft/windows-rs master, never crates.io.
  - They are declared once in the root `[workspace.dependencies]`, and `Cargo.lock` pins the
    commit.
  - To update, run `cargo update -p windows -p windows-pickers -p windows-reactor -p
    windows-registry`, then fix what its API changes break.
  - The first fetch downloads the whole repository (about 1.4 GB).
- **Windows App Runtime**: WinUI apps need version 2.x installed.
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
- **Release profile**: `lto`, `codegen-units = 1`, `strip`.

## Commands
- `cargo build`; `cargo run -p app`.
- `cargo run -p gm-core --example scan` checks the scanners without the UI.
  - `-- -v` prints launch commands, icons, programs and wiki links.
  - `-- --graphics` prints the wiki's and the files' APIs, about 2 s a game.
  - `-- --icons <dir>` saves every icon as a PNG.
  - To verify bitness, compare a `program` with `GetBinaryTypeW` and `dumpbin /headers`. Parse
    scan lines from the right, as names can be wider than their column.
- **Before finishing a change**: `cargo fmt`, `cargo clippy --all-targets` (no warnings),
  `cargo test`.

## Checking the app from an agent session
- **No screen capture**: inspect the running window with UI Automation
  (`System.Windows.Automation` in PowerShell).
  - The Open dialog (class `#32770`) shows its Win32 controls as plain panes. Use its
    `NativeWindowHandle`, then `GetDlgItem(dlg, 1148)` → `ComboBox` → `Edit` with `WM_SETTEXT`,
    then `PostMessage` `BM_CLICK` to `GetDlgItem(dlg, 1)`.
- **No input desktop**: drag and drop, right-clicks and the UAC prompt can't be done (the cursor
  reads 0,0 and there is no foreground window). Ask the user to try them on a build they run.
- **The user's files**: back up `%APPDATA%\Game Manager\*.json` before a check and restore them
  after, comparing first that only the test changed them.
- **The user's window**: never close or kill it. If `target\debug\app.exe` is locked, build into
  `CARGO_TARGET_DIR=target\agent`.
- **Mods**: never install into the user's real games, whose folders may hold their own DXVK or
  ReShade. Use a throwaway custom game instead:
  - Set up a temp folder with a copy of `SysWOW64\notepad.exe` as `game.exe` and a fake
    `d3d9.dll`.
  - Add an entry to `custom-games.json` and `"graphicsApi": "DirectX9"` to `game-settings.json`.
  - Drive Install, Settings, Reinstall and Uninstall, then compare the folder with how it started.
  - `app.exe --file-ops <percent-encoded ops>` can be run directly (not elevated).

## Conventions
- **Background work**: blocking work (disk, registry, network) never runs on the UI thread.
- **Game data**: no game-specific data in Rust code. Knowledge about particular games goes into
  `config/games.json` (matched by binaries, never store ids); the user's choices go into
  `game-settings.json`.
- **Errors and `unsafe`**: typed errors, one unsafe operation per block with `// SAFETY:`, and
  RAII owners for handles (details in `core/CLAUDE.md`).
