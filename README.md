# Game Manager

A native Windows 11 game launcher written in Rust with a WinUI 3 interface
([`windows-reactor`](https://github.com/microsoft/windows-rs)). It finds installed games from
Steam, Epic Games, GOG, EA and the Xbox app, shows their icons, and starts them. Any other game
can be added by hand with **Add game** (choose its program, then a name and optional command-line
arguments), or by dropping the game's `.exe` or its desktop shortcut on the window (a shortcut
brings its name, arguments and icon); those are saved in `%APPDATA%\Game Manager\custom-games.json`.

Each game's **Game settings** (an expander under its buttons) change the name shown, the program
Play starts (instead of the store's launch), the command-line arguments (also passed to Steam
games as launch options) and the game's own program (for its bitness, Open folder and mods).
Edits are saved in `%APPDATA%\Game Manager\game-settings.json`; **Reset to detected** removes them.

Right-click a game in the library for **Play** and **Add to Favorites** (or Remove from
Favorites); the **Favorites** entry of the filter above the list shows only those games. Favorites
are saved in `game-settings.json` too.

**PCGamingWiki** opens the game's [PCGamingWiki](https://www.pcgamingwiki.com) page (fixes,
settings, save locations) in your browser: by store id for Steam and GOG games, otherwise a wiki
search for the game's name (rename the game in Game settings if the search misses it).

When a store's Play action starts a launcher, the game's own program comes from the shipped
`config/games.json` (known games, e.g. Cyberpunk 2077's `bin\x64\Cyberpunk2077.exe`) unless you set
one. The build copies `games.json` next to `app.exe`; keep it there when you move the app.

To start fast, the app caches the last scanned library and the extracted game icons in
`%LOCALAPPDATA%\Game Manager\cache`. Deleting that folder is always safe: it is rebuilt on the next
start. Your own choices stay in `%APPDATA%\Game Manager`.

## Set up a new computer

1. **Visual Studio Build Tools** (or Visual Studio) with the **Desktop development with C++**
   workload. Rust needs its MSVC linker and the Windows SDK.
2. **Rust** via [rustup](https://rustup.rs). The project pins Rust 1.99.1 in
   `rust-toolchain.toml`; rustup installs it automatically on the first build.
   The windows-rs crates come straight from
   [microsoft/windows-rs](https://github.com/microsoft/windows-rs) master, so the first build
   also downloads that repository (about 1.4 GB). `cargo update` moves them to its latest commit.
3. **Windows App Runtime 2.x** (needed to run WinUI 3 apps). It is usually already installed
   with Windows 11 apps; otherwise install it from Microsoft.
4. **Git** and, optionally, the **GitHub CLI** (`winget install GitHub.cli`).

Then:

```powershell
gh repo clone startuga/game-manager    # or: git clone https://github.com/startuga/game-manager.git
cd game-manager
cargo run -p app
```

## Commands

| Command | What it does |
|---|---|
| `cargo run -p app` | Builds and starts the app |
| `cargo run -p gm-core --example scan -- -v` | Lists detected games and how each is launched, without the UI |
| `cargo run -p gm-core --example scan -- --icons <folder>` | Saves every game's icon as a PNG |
| `cargo test` | Runs all tests |
| `cargo clippy --all-targets` | Lints (the project keeps zero warnings) |

## Project layout

- `app/` – the WinUI 3 window (top bar, library sidebar, game panel).
- `core/` (crate `gm-core`) – store scanners, icon extraction and launching; no UI code.
- `config/games.json` – per-game knowledge used by the scanners (no game-specific data in code).
- `CLAUDE.md` – conventions and notes used when developing with Claude Code.
