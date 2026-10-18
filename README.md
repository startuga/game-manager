# Game Manager

A native Windows 11 game launcher written in Rust with a WinUI 3 interface
([`windows-reactor`](https://github.com/microsoft/windows-rs)). It finds installed games from
Steam, Epic Games, GOG, EA and the Xbox app, shows their icons, and starts them. Any other game
can be added by hand with **Add game** (choose its program, then a name and optional command-line
arguments); those are saved in `%APPDATA%\Game Manager\custom-games.json`.

## Set up a new computer

1. **Visual Studio Build Tools** (or Visual Studio) with the **Desktop development with C++**
   workload. Rust needs its MSVC linker and the Windows SDK.
2. **Rust** via [rustup](https://rustup.rs). The project pins Rust 1.99.1 in
   `rust-toolchain.toml`; rustup installs it automatically on the first build.
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
- `CLAUDE.md` – conventions and notes used when developing with Claude Code.
