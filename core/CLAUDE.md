# core (crate `gm-core`)

Game discovery, launching, configuration, graphics detection and mods. **No UI code**, so the UI
can be swapped. Each module's `//!` doc says how it works and why; this file maps the modules and
holds the rules and facts that span them.

## Modules
- **Stores**: `steam.rs` (+ `vdf.rs`, Valve KeyValues text and binary), `epic.rs`, `gog.rs`,
  `ea.rs`, `xbox.rs`; `custom.rs` (games added by hand, `NewGame` from a picked or dropped file),
  `shortcut.rs` (`.lnk` via `ShellLink`).
- **Game data**:
  - `lib.rs` (`Game`, `Launch`, `Arch`, `scan_all`, `complete_game`).
  - `config.rs` (shipped `games.json`, the user's `game-settings.json`, `apply`).
  - `snapshot.rs` (last scan, for a fast start).
  - `pe.rs` (architecture and imports from PE headers).
- **Around games**:
  - `launcher.rs` (`launch`, `open_folder`; both on the UI thread, whose COM apartment the Shell
    needs).
  - `icon.rs` (icons as PNG via the Shell + WIC, cached).
  - `graphics.rs` + `pcgamingwiki.rs` (graphics APIs, wiki links).
  - `http.rs` (WinHTTP `get`/`download`).
- **Mods**:
  - `mods/mod.rs` (`mods.json`, `FileOp` plans that undo themselves, `apply`).
  - `mods/dxvk.rs` (Stable, Development, HDR-mod).
  - `seven_zip.rs` (the shipped 7-Zip).
  - `elevate.rs` (`runas` + `--file-ops` / `--nvidia-ops`).
  - `nvidia.rs` (the game's NVIDIA driver profile: Present Method and its flags, through NVAPI).
- **Shared**:
  - `error.rs` (`Error`, `Result`, `ScanError`).
  - `json_file.rs` (where files live, load/save).
  - `text.rs`.
  - `com.rs` (`ComGuard`).
- `examples/scan.rs` – headless check: `-v`, `--graphics`, `--icons <dir>`.

## Scanning
- `scan_all` runs these steps in order:
  1. The store scanners (`SCANNERS` in `lib.rs`, in priority order: when two stores report one
     install folder, the earlier keeps the game).
  2. `config::apply`.
  3. `default_program`.
  4. `dedupe_install_dirs` (a dropped duplicate gives its program to the kept game).
  5. Custom games, with `apply` too.
  6. `complete_game` (icon fallback, then `arch` from the PE header unless already set).
- A store that fails adds a `ScanError` to `ScanReport::errors`; it never hides other stores'
  games.
- **New store**: a module with `pub fn scan() -> Result<Vec<Game>>`, a `Store` variant, and an
  entry in `SCANNERS`.
- **Game ids** are `<store>:<store id>` and stable: they key the list and the user's files.
- **Program precedence**: the user's choice > the shipped `config/games.json` (recognised by the
  game's binaries, never store ids) > the scanner's. A chosen bitness replaces `Game::arch`.

## Files
- `json_file::user_file` → `%APPDATA%\Game Manager`: the user's `game-settings.json`,
  `custom-games.json` and `mods.json`.
  - They are loaded before saving, so an unreadable file is never overwritten, and unknown fields
    are kept.
- `json_file::local_file` → `%LOCALAPPDATA%\Game Manager`: caches, safe to delete.
- `build.rs` ships `games.json` and `7-Zip\` next to `app.exe`; `config::shipped_file` finds them
  there or one folder up (examples, tests).
- **Format numbers**: bump the one whose layout changes.
  - `snapshot::VERSION`: the serde form of `Game`.
  - `icon::CACHE_FORMAT`.
  - `dxvk::CACHE_FORMAT`.

## Rules
- **Errors**: return `gm_core::Result<T>` with a `gm_core::Error` variant, never a `String`
  error. A new failure gets its own variant when a caller may want to tell it apart.
- **`unsafe`**: every block holds one operation and gets a `// SAFETY:` comment (Clippy enforces
  both). A handle that must be freed is owned by a type whose `Drop` frees it: `OwnedIcon`,
  `DataModule`, `ItemIdList`, `ComGuard`, WinHTTP's `Handle`, `elevate::OwnedHandle`.
- **No game-specific data in code**: knowledge of particular games goes into `config/games.json`.
  Store file formats and registry locations stay in the scanners.
- **Network goes through `http.rs`**, with a User-Agent that has contact details.
  - PCGamingWiki: one request a second.
  - GitHub's API: 60 anonymous requests an hour.
- **windows-rs bindings**: Win32 features are named after C headers (`core/Cargo.toml`). The
  bindings come from windows-rs master and change shape between commits (constants' integer
  types, `Option` vs raw pointers): fix call sites to the new signatures.

## Facts learned the hard way
- **Graphics APIs**: PCGamingWiki first, because files alone mislead. On 37 games:
  - GTA V imports only `d3d9.dll` but runs Direct3D 11.
  - Every Unreal game imports `d3d9.dll`/`opengl32.dll`.
  - Plug-ins import Direct3D 11/12.
  - Unity loads its API at run time.

  The answer is a suggestion; the user picks one API in Game settings, so detection keeps no
  cross-checks.
- **Stores**:
  - The EA app's data is encrypted: there is no EA-app launch link.
  - Xbox packaged games start only through `shell:AppsFolder\<family>!<app>`.
  - A GOG primary task can be a launcher (Witcher 3's `REDprelauncher.exe`).
- **Protected folders**: Epic/EA under `C:\Program Files` need administrator rights; Steam's and
  some Epic folders grant Users full control. Mod file changes there run in
  `app.exe --file-ops <ops>`, started with `runas`. The ops go on the command line, so the
  elevated copy reads no file another program could change.
- **NVAPI**: `nvapi64.dll` (System32 only) exports just `nvapi_QueryInterface`; functions are
  fetched by the ids in NVIDIA's `nvapi_interface.h`.
  - **Struct packing**: `NVDRS_SETTING_V1` is inside `#pragma pack(4)` in `nvapi.h`. Its value
    unions are 4 100 bytes, so the struct is 12 320 bytes and its version `0x13020`. Assuming
    8-byte alignment gave 0x13028 and the driver answered −9 (incompatible struct version).
  - **Settings**: Present Method `0x20D690F8` and its flags `0x20324987`, from
    nvidiaProfileInspector's `CustomSettingNames.xml`.
  - **Reads and writes**: reading needs no rights, and neither does setting Present Method.
    Writing the flags (`OGL_DX_PRESENT_DEBUG`) answers −137 without administrator rights, so
    plans fall back to `app.exe --nvidia-ops <plan>` (the UAC prompt).
    - A refused session is never saved: nothing is left.
    - `cargo test -p gm-core live_profile_round_trip -- --ignored` checks this on an NVIDIA PC.
    - `app.exe --nvidia-ops <percent-encoded plan>` can be run directly.
  - Many games' predefined profiles already hold NVIDIA's "Auto".
