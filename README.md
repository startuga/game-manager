# Game Manager

A native Windows 11 game launcher written in Rust. Its window is plain Win32 drawn with Direct2D and
DirectWrite ([windows-rs](https://github.com/microsoft/windows-rs)'s `windows-window`,
`windows-canvas` and `windows-text`): no WinUI, no XAML and no Windows App Runtime.

It finds installed games from Steam, Epic Games, GOG, EA, Ubisoft Connect and the Xbox app, shows
their icons, and starts them. Any other game can be added by hand with **Add a game** (choose its
program, then a name and optional command-line arguments), or by dropping the game's `.exe` or its
desktop shortcut on the window (a shortcut brings its name, arguments and icon); those are saved
in `%APPDATA%\Game Manager\custom-games.json`.

## The window

- **The sidebar**: search with the **Sort and filters** button beside it, **My PC** and
  **Library**, then the pinned games under **Pinned**, then the others by store (click a store
  to fold it), and **Add a game** at the bottom. Drag its edge to make it wider or narrower.
- **Sort and filters** (the button beside the search) opens a flyout that the sidebar and
  Library both follow. It stays open while you pick; a click outside or **Escape** closes it.
  - **Sort**: by name A–Z or Z–A, or the most recently installed first (the date of the
    game's folder). **Group by store** off shows one list; pinned games stay on top.
  - **Smart filters**: tick one store or several, and **Hidden games** to see the games you
    hid from the list.
  - **Smart Collections** fill themselves: **Pinned**, **Modded** (anything Game Manager set
    up: ReShade, DXVK, Large Address Aware, a swapped upscaler, NVIDIA DLSS overrides), **With
    ReShade**, **With DXVK**, **32-bit** and **Recently installed** (the last 30 days). One is
    on at most: picking one clears the filters, and ticking a filter turns it off.
  - **Clear all** takes the filters and the collection off. The button shows a dot while one
    is on, and Library's caption names it. Your choices are kept in `game-settings.json` for
    the next start.
- **Library**, where the window opens: every game as a tile with its cover and name, the pinned
  ones first, then by store, following the search and Sort and filters. Click a tile for the
  game's page.
  - **Covers** come from what the stores already keep on your PC (Steam's library cache, GOG
    Galaxy's), else from the store's own server by the game's id (Steam, the Epic Games
    Launcher's catalog, GOG, Microsoft's store for Xbox games), else from the Steam store under
    the same name (EA and Ubisoft games and games added by hand). A Ubisoft game the Steam
    store doesn't have under its name keeps the small picture an older Ubisoft Connect saved,
    when there is one. A game with none shows its icon.
  - Covers are kept in `%LOCALAPPDATA%\Game Manager\cache\covers`, so the next start asks the
    network for nothing. A game with no cover anywhere is asked about again a week later.
  - **Choose cover…** in a game's menu takes any picture of yours (cropped to the tile's 2:3
    around its middle), kept in `%APPDATA%\Game Manager\covers`; **Reset cover** goes back to
    the store's.
- **A game's menu**: right-click a game in the sidebar or on Library, or press the **Menu** key or
  **Shift+F10** on it. The page you are on stays put. **↑↓** move, **Enter** picks, **Escape**
  closes.
  - **Pin to quick access** (or **Unpin**): pinned games sit on top of the sidebar and of
    Library, in the order you pinned them. Favorites from earlier versions became pins.
  - **Play**, and the store's other ways to start it ("Skip launcher", a Ubisoft game's
    "Settings").
  - **Open folder**, **PCGamingWiki**.
  - **Rename…** (leave it empty for the store's name), **Choose cover…** (and **Reset cover**),
    **Hide from the list** (Hidden games in Sort and filters shows it again, with **Show in the
    list**). The
    names, pins and hidden games are saved in `game-settings.json`.
  - **Uninstall…** hands the game to Steam or Ubisoft Connect, to its own uninstaller (GOG, the
    EA app, others with a Windows Uninstall entry), or to the Epic Games Launcher, GOG Galaxy or
    Windows' Installed apps to uninstall it there. They ask before anything is removed. Game
    Manager deletes nothing itself. Mods it set up outside the game's folder (ReShade's Vulkan
    layer entry, NVIDIA profile settings) stay. **Rescan** afterwards takes the game off the list.
  - **Remove from Game Manager**, for games added by hand (their files stay).
- **Search** (**Ctrl+F** from anywhere): case, accents, ™ and ® and punctuation don't matter, and
  the words can come in any order ("cry far 3"). It also finds a name typed without spaces
  ("halflife"), by its initials ("gta", "tw3"), and by its program's file name ("ffxv").
  - While searching, every store shows open and a line says how many games match.
  - **Enter** opens the first match; **Escape** or ✕ clears the search.
- **The title bar** has **Rescan** (⟳), which looks for games again.
- **A game's page**: its icon, name and install folder, with chips for its store, bitness and
  graphics APIs; **Play** (and the store's other launches), **Folder** and **PCGamingWiki**.
  Below them are the tabs: **Overview**, **ReShade**, **DXVK & LAA**, **Upscalers** and, with an
  NVIDIA graphics driver, **NVIDIA overrides**.

The graphics API chips (DirectX 8–12, OpenGL, Vulkan) come from PCGamingWiki's API table when the
wiki has the game (looked up once, when you select it, and cached in `%LOCALAPPDATA%\Game
Manager\cache`), otherwise from the game's files, which can list APIs the game does not use. Hover
over them to see where they come from. When several are shown, choose the one the game runs on in
Game settings. With DXVK in the game's folder (installed by Game Manager or by hand), a Direct3D
8–11 game shows **Vulkan (DXVK)**: it then runs on Vulkan.

## Overview: Game settings

**Game settings** change the name shown, the program Play starts (instead of the store's launch),
the command-line arguments (also passed to Steam games as launch options) and the game's own
program (for its bitness, Folder and mods). Its **Bitness** and **Graphics API** drop-downs set the
game's 32 or 64-bit build and the one API it runs on, when detection got them wrong or found
several APIs: mods such as ReShade and DXVK need exactly these. Edits are saved in
`%APPDATA%\Game Manager\game-settings.json`; **Reset to detected** removes them. **Details** shows
the store, the game's program and its bitness.

## ReShade

The **ReShade** tab has three views: **ReShade**, **Shaders** and **Add-ons**. The first installs
[ReShade](https://reshade.me), post-processing effects for games. **Install** copies ReShade as the
game's graphics DLL (`d3d9.dll`, `dxgi.dll` or `opengl32.dll`), writes a `ReShade.ini` when the
game has none, and adds the shaders and add-ons chosen in the other two views (below) in
`reshade-shaders` and next to its program; press Home in the game for its overlay. ⚙ chooses
**Stable** (reshade.me, with add-ons), **Development** (the latest main build, via nightly.link)
or **No Addons** (reshade.me, for online games). Vulkan games, and
games with DXVK, use ReShade's Vulkan layer instead: the same install for the whole computer that
ReShade's own setup makes in `C:\ProgramData\ReShade`, so it always asks for administrator rights.
Direct3D 8 games need DXVK first. Reinstall keeps ReShade's settings; uninstalling (✕) removes
ReShade with `ReShade.ini`, `ReShadePreset.ini`, `ReShade.log` and the `reshade-shaders` folder.

**Shaders** and **Add-ons** choose what games with ReShade get:

- **Shaders** lists 55 shader packs as Essential (Lilium's HDR shaders,
  selected until you choose), Recommended, Custom Shaders (your own) and Extra. ▶ downloads a
  pack and lists its effects, to pick single ones; search finds packs and effects. Profiles keep
  selections (Save, New, rename, delete); **Export** zips the selected shaders onto the clipboard,
  to paste into Discord or a folder, and **Import** adds a ZIP's shaders to your own (**Open
  custom folder**). **Deploy** puts the selection into the games it applies to; **Discard
  changes**, or leaving the view, drops what wasn't deployed. Packs are cached in
  `%LOCALAPPDATA%\Game Manager\mods\reshade\packs` and updated when they change.
- **Add-ons** lists ReShade's add-ons and a few more, and your own (`.addon32`/`.addon64` files in
  the custom folder), each with an On/Off switch. **Apply**, or leaving the view (another view,
  tab or game), puts the add-ons switched on next to the program of the games it applies to, for
  each one's bitness; games with the No Addons build get none.

**Apply to**, at the top of both, chooses where: **Only** the game selected (the default), which
then keeps a selection of its own, or **All games with ReShade**. Deploying to
all games leaves a game's own selection alone until **Use all games' selection** (or add-ons)
puts it back on the shared one. A game without a selection of its own starts from the shared one.

Your choices are saved in `%APPDATA%\Game Manager\reshade.json` (a game's own under `games`), your
own shaders and add-ons in `%APPDATA%\Game Manager\reshade\custom`.

## DXVK & LAA

[DXVK](https://github.com/doitsujin/dxvk) runs Direct3D 8–11 games on Vulkan: **Install**
downloads the latest build and copies the DLLs for the game's bitness and graphics API next to its
program (choose them in Game settings when they are unknown or several APIs were found). The
version link opens the page that build came from, **Info** explains DXVK, ⚙ chooses **Stable**
(GitHub releases), **Development** (the latest master build, via nightly.link) or **HDR-mod**
([EndlesslyFlowering's fork](https://github.com/EndlesslyFlowering/dxvk) for HDR, GitHub releases;
pick one of its HDR configs for the game's Direct3D version, from 0 – safest to the experimental
ones) and the HDR, frame-rate limit and HUD settings written to `dxvk.conf`, and ✕ uninstalls it.
On NVIDIA GPUs, DXVK settings also set the game's driver profile for HDR: **Vulkan/OpenGL Present
Method** (Prefer layered on DXGI Swapchain) and its **flags** (Allow promoting DXVK to
DXGI/DirectFlip, or Treat DXVK as Native), with **Reset to original** to put back what the
profile had; they stay when DXVK is uninstalled.

For 32-bit games, the tab also shows **Large Address Aware**: whether the game's program may use
4 GB of memory instead of 2 GB, and **Enable LAA** to patch it (the original is kept as
`<program>.gm-backup`, and ✕ puts it back). Programs wrapped by Steam's DRM get a warning, as a
changed program may not start.

## Upscalers

The **Upscalers** tab lists the upscaler DLLs in the game's folder and below:
- **DLSS** Super Resolution, Ray Reconstruction and Frame Generation.
- **NVIDIA Streamline**, swapped as one set, as its plugins must match. A game with Streamline 1
  can't use Streamline 2, so it is offered none.
- **FSR 3.1** (DirectX 12, Vulkan) and the **FidelityFX SDK 2** set.
- **XeSS** Super Resolution (and its DirectX 11 build), Frame Generation and Xe Low Latency.

Each shows its version. Pick another of its vendor's versions and **Swap** to upgrade or downgrade
it without a game update; ✕ puts the game's own DLL back. The versions are NVIDIA's, AMD's and
Intel's own, from their GitHub repositories, and each DLL must carry a valid signature. FSR 2 and
3.0, and DLSS 1, are built into games and can't be swapped. **Versions**, folded at the bottom of
the tab, lists every version with Download and Delete. An online game's anti-cheat may object to
changed files, and a game update puts the game's own DLL back.

## NVIDIA overrides

With an NVIDIA graphics driver, the **NVIDIA overrides** tab sets NVIDIA's driver-level DLSS
override in the game's driver profile, as the NVIDIA app does, without changing the game's files:
- **Latest DLLs**: the driver's own DLSS Super Resolution, Ray Reconstruction, Frame Generation
  and Streamline, with their versions.
- **Model presets**: Super Resolution's and Ray Reconstruction's.
- **Resolution**: the mode, a scaling ratio, every mode as DLAA, and Performance as Ultra
  Performance.
- **Frame generation**: the frame generation count, and Smooth Motion.

Each drop-down starts with what the profile held; **Save** sets the choice, and **Reset to
original** then **Save** put the profile back (a profile Game Manager created is deleted). Settings
the driver is too old for are not shown. The settings come from `config/nvidia-overrides.json`.
The NVIDIA app writes the same settings: when it (or another tool) has changed them since, the tab
says which, and Save sets them again.

## My PC

The sidebar's **My PC** page shows what the computer is, read from the registry and Windows' own
functions (no WMI), each time it opens and on **Refresh**:

- **Hardware**: each graphics card (video memory, driver version as its vendor numbers it, driver
  date), the processor (cores, threads, base clock), memory (in use, and the modules: size, type,
  speed, maker and part number), Windows (edition, version, build), the motherboard and BIOS, and
  each fixed drive's free space.
- **Displays**: each display's name, resolution, refresh rate and HDR state, with:
  - **Resolution** and **Refresh rate**: the new mode applies at once and a dialog asks to keep it;
    without an answer within 15 seconds (or with **Go back** or Escape) the previous mode comes back.
  - **HDR**: Windows' own switch (Settings ▸ Display ▸ HDR).
  - **Brightness**: set over DDC/CI, like the monitor's own buttons, for monitors that have DDC/CI
    on in their own menu. A laptop's screen keeps Windows' brightness control.
  - **Colour profile**: picks among the profiles linked to the display for its mode (SDR or HDR).
- **Graphics settings for games**, Windows' own switches:
  - **Hardware-accelerated GPU scheduling**: Windows asks for administrator rights, and it applies
    after a restart.
  - **Game Mode**, **Optimizations for windowed games** and **Auto HDR** (with an HDR display):
    games pick them up when they start.

Links open the matching pages of Windows' Settings.

## Files

Files of the game that a mod replaces are kept as `<name>.gm-backup` and put back on uninstall.
When a game is in a protected folder (such as `C:\Program Files`), Windows asks for administrator
rights. What is installed is saved in `%APPDATA%\Game Manager\mods.json`; downloaded builds are
cached in `%LOCALAPPDATA%\Game Manager\mods`.

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
3. **Git** and, optionally, the **GitHub CLI** (`winget install GitHub.cli`).

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
| `cargo build --release` | Builds the optimized app: `target\release\app.exe`, with `games.json` and `7-Zip\` next to it |
| `cargo run -p app --release` | Builds the optimized app and starts it |
| `cargo run -p gm-core --example scan -- -v` | Lists detected games and how each is launched, without the UI |
| `cargo run -p gm-core --example scan -- --icons <folder>` | Saves every game's icon as a PNG |
| `cargo test` | Runs all tests |
| `cargo clippy --all-targets` | Lints (the project keeps zero warnings) |

Release builds use link-time optimization, a single codegen unit and stripped symbols
(`[profile.release]` in `Cargo.toml`), so they take longer than debug builds but give a smaller,
faster `app.exe`. To run it from another folder, copy `app.exe`, `games.json` and the `7-Zip` folder
together. A running `app.exe` cannot be replaced: close it before rebuilding.

## Project layout

- `app/` – the window (Win32, Direct2D and DirectWrite): the sidebar, the Library grid, the game
  page and its tabs, and My PC. It draws only with Windows' own fonts (Segoe UI Variable, Segoe
  Fluent Icons).
- `core/` (crate `gm-core`) – store scanners, icon extraction and launching; no UI code.
- `config/games.json` – per-game knowledge used by the scanners (no game-specific data in code).
- `tools/7-Zip` – the 7-Zip command line (`7z.exe`, `7z.dll`) that unpacks downloaded mods.
  7-Zip is © Igor Pavlov, licensed under the GNU LGPL; see `tools/7-Zip/License.txt`.
- `CLAUDE.md` – conventions and notes used when developing with Claude Code.
