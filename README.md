# Game Manager

A native Windows 11 game launcher written in Rust with a WinUI 3 interface
([`windows-reactor`](https://github.com/microsoft/windows-rs)). It finds installed games from
Steam, Epic Games, GOG, EA and the Xbox app, shows their icons, and starts them. Any other game
can be added by hand with **Add game** (choose its program, then a name and optional command-line
arguments), or by dropping the game's `.exe` or its desktop shortcut on the window (a shortcut
brings its name, arguments and icon); those are saved in `%APPDATA%\Game Manager\custom-games.json`.

Each game's **Game settings** (an expander under its buttons) change the name shown, the program
Play starts (instead of the store's launch), the command-line arguments (also passed to Steam
games as launch options) and the game's own program (for its bitness, Open folder and mods). Its
**Bitness** and **Graphics API** drop-downs set the game's 32 or 64-bit build and the one API it
runs on, when detection got them wrong or found several APIs: mods such as ReShade and DXVK need
exactly these. Edits are saved in `%APPDATA%\Game Manager\game-settings.json`; **Reset to
detected** removes them.

Right-click a game in the library for **Play** and **Add to Favorites** (or Remove from
Favorites); the **Favorites** entry of the filter above the list shows only those games. Favorites
are saved in `game-settings.json` too.

The game panel also shows the game's **graphics APIs** (DirectX 8–12, OpenGL, Vulkan): from
PCGamingWiki's API table when the wiki has the game (looked up once, when you select it, and cached
in `%LOCALAPPDATA%\Game Manager\cache`), otherwise read from the game's files, which can list APIs
the game does not use. Hover over them to see where they come from. When several are shown, choose
the one the game runs on in Game settings.

The **Mods** expander installs mods into the game's folder. The first is
[DXVK](https://github.com/doitsujin/dxvk), which runs Direct3D 8–11 games on Vulkan: **Install DXVK**
downloads the latest build and copies the DLLs for the game's bitness and graphics API next to its
program (choose them in Game settings when they are unknown or several APIs were found). The
version link opens the page that build came from, **Info** explains DXVK, ⚙ chooses **Stable**
(GitHub releases) or **Development** (the latest master build, via nightly.link) and the HDR,
frame-rate limit and HUD settings written to `dxvk.conf`, and ✕ uninstalls it. Files of the game
that DXVK replaces are kept as `<name>.gm-backup` and put back on uninstall. When a game is in a
protected folder (such as `C:\Program Files`), Windows asks for administrator rights. What is
installed is saved in `%APPDATA%\Game Manager\mods.json`; downloaded builds are cached in
`%LOCALAPPDATA%\Game Manager\mods`.

**PCGamingWiki** opens the game's [PCGamingWiki](https://www.pcgamingwiki.com) page (fixes,
settings, save locations) in your browser: by store id for Steam and GOG games, otherwise a wiki
search for the game's name (rename the game in Game settings if the search misses it).

When a store's Play action starts a launcher, the game's own program comes from the shipped
`config/games.json` (known games, e.g. Cyberpunk 2077's `bin\x64\Cyberpunk2077.exe`) unless you set
one. The build copies `games.json` and the `7-Zip` folder next to `app.exe`; keep them there when
you move the app.

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
- `tools/7-Zip` – the 7-Zip command line (`7z.exe`, `7z.dll`) that unpacks downloaded mods.
  7-Zip is © Igor Pavlov, licensed under the GNU LGPL; see `tools/7-Zip/License.txt`.
- `CLAUDE.md` – conventions and notes used when developing with Claude Code.
