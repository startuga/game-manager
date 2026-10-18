# Game Manager

Native Windows 11 game launcher/library written in Rust.

## Layout
- `app/` – WinUI 3 UI built with `windows-reactor` (preview, 0.100). Component/message/view model, Elm style.
  - `main.rs` – window layout: `top_bar` (title, scan status, Add game, Rescan), `sidebar` (search, store filter, game list), `main_panel` (selected game: icon, title, install path, ▶ Play + extra launch buttons, Remove for custom games, store), `add_dialog`/`remove_dialog` (`ContentDialog`s driven by `draft`/`removing` state). Selection is kept by game id.
  - `library.rs` – UI-free search/store-filter/selection rules, unit-tested.
  - `picker.rs` – Win32 `IFileOpenDialog` for a custom game's program. It runs on its own STA thread inside a background task, owned by the window from `GetActiveWindow` (taken on the UI thread at click time), so the main window is disabled while it is open.
- `core/` (crate `gm-core`) – game discovery, launching, data model. **No UI code here**, so the UI layer can be swapped.
  - `vdf.rs` – Valve KeyValues parser; `steam.rs` – Steam scanner; `epic.rs` – Epic scanner (current launcher: `%LOCALAPPDATA%\com.epicgames.eglv2\installations.jsonl` log + `cache\prod\catalog-cache.json` for titles; legacy: `%ProgramData%` `*.item` manifests); `gog.rs` – GOG scanner (`HKLM\SOFTWARE\WOW6432Node\GOG.com\Games` + each game's `goggame-<id>.info` primary play task); `ea.rs` – EA scanner (folders with `__Installer\installerdata.xml`, found via uninstall entries and `Electronic Arts`/`EA Games` `Install Dir` keys, listed only if the EA app knows them: an `Origin Games\<contentID>` key or an `EAInstaller\...\Cleanup.exe` uninstaller; names from `Origin Games\<contentID>`; program from installerdata `<runtime><launcher>` or the GDF XML resource in `GDFBinary_<locale>.dll`. The EA app's own data is encrypted, so there is no EA-app launch link); `launcher.rs` – `ShellExecuteW`.
  - `xbox.rs` – Xbox app / Microsoft Store scanner: the current user's packages from WinRT `PackageManager.FindPackagesByUserSecurityId("")`, kept when the install folder has `MicrosoftGame.config`. Play = `shell:AppsFolder\<PackageFamilyName>!<AppId>` (packaged games cannot be started by their `.exe`); "Open in Xbox app" = `msxbox://game/?productId=<StoreId>`; icon = `Square150x150Logo` (or its largest `scale-`/`targetsize-` variant).
  - `custom.rs` – games added by hand (`Store::Custom`, ids `custom:<creation ms>`): saved in `%APPDATA%\Game Manager\custom-games.json` (written via a temp file; an unreadable file is never overwritten). `scan_all` adds them after `dedupe_install_dirs`, so they are never dropped for sharing a folder with a store game. `suggested_name` reads the program's version info (`ProductName`, then `FileDescription`, else the file stem).
  - `text.rs` – shared helpers: UTF-8/UTF-16 file reading, XML declaration stripping, `child_text`.
  - `icon.rs` – game icons as PNG via native APIs (`SHDefExtractIconW` + WIC). `Game::icon` is an `IconRef` (file + index): Steam `Steam App <id>` uninstall `DisplayIcon`, GOG `goggame-<id>.ico`, EA uninstall `DisplayIcon`/`game.ico`, Epic `launchExecutable`, Xbox package logo (PNG/JPEG files are used as they are); otherwise `scan_all` falls back to the Play program. The app loads icons in a second background task after the list is shown.
  - `Game::launch` is the default Play action; `Game::alt_launches` are optional extra actions (e.g. GOG "Play via Galaxy"). `Launch::Exe` without a `working_dir` starts in the program's own folder.
  - New stores: add a module with `pub fn scan() -> Result<Vec<Game>, String>`, a `Store` variant, and an entry in `SCANNERS` in `lib.rs`. `SCANNERS` order is priority: when two stores report the same install folder, the earlier store keeps the game.
  - `examples/scan.rs` – prints detected games (headless check of the scanners).

## Toolchain
- Rust pinned to 1.99.1 in `rust-toolchain.toml`, edition 2024, MSVC target.
- WinUI apps are framework-dependent: they need the Windows App Runtime 2.x installed.

## Commands
- Build: `cargo build`
- Run UI: `cargo run -p app`
- Scanner check: `cargo run -p gm-core --example scan` (add `-- -v` to print launch commands and icon sources, `-- --icons <dir>` to save every icon as PNG)
- UI check: screen capture does not work from the agent session; inspect the running window with UI Automation (`System.Windows.Automation` in PowerShell) instead. The Open dialog appears under the main window in the UIA tree, and its Win32 controls show as plain panes: set the file name with `WM_SETTEXT` on the `Edit` with id `1148` and press Open with `BM_CLICK` on the `Button` with id `1`. Custom-game checks write to the real `%APPDATA%\Game Manager\custom-games.json`; clean it up afterwards.
- Before finishing a change: `cargo fmt`, `cargo clippy --all-targets` (no warnings), `cargo test`.

## Conventions
- Blocking work (disk scans, registry, network) runs via `ComponentContext::spawn_background` and returns a `Message`; never on the UI thread.
- A store that fails to scan adds to `ScanReport::errors`; it must not hide other stores' games.
- Every `unsafe` block gets a `// SAFETY:` comment.
- Game ids are `<store>:<store id>` and must be stable (used as list keys and, later, for saved data).
- `windows-reactor` is a preview: when unsure of an API, read its source in `~/.cargo/registry/src/*/windows-reactor-0.100.0/src` rather than guessing.
