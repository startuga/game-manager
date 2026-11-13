# core (crate `gm-core`)

Game discovery, launching, configuration, graphics detection and mods. **No UI code**, so the UI
can be swapped. Each module's `//!` doc says how it works and why; this file maps the modules and
holds the rules and facts that span them.

## Modules
- **Stores**: `steam.rs` (+ `vdf.rs`, Valve KeyValues text and binary), `epic.rs`, `gog.rs`,
  `ea.rs`, `ubisoft.rs` (Ubisoft Connect; its own small protobuf and YAML readers), `xbox.rs`;
  `custom.rs` (games added by hand, `NewGame` from a picked or dropped file), `shortcut.rs`
  (`.lnk` via `ShellLink`).
- **Game data**:
  - `lib.rs` (`Game`, `Launch`, `Arch`, `scan_all`, `complete_game`).
  - `config.rs` (shipped `games.json`, the user's `game-settings.json`: edits, hidden games,
    the `pinned` list in pin order, into which older favorites are moved, and `library`, how the
    lists are sorted and filtered (`LibraryView`); `apply`).
  - `snapshot.rs` (last scan, for a fast start).
  - `pe.rs` (architecture and imports from PE headers).
- **Around games**:
  - `launcher.rs` (`launch`, `open_folder`; both on the UI thread, whose COM apartment the Shell
    needs).
  - `uninstall.rs` (how a game is uninstalled: `route` reads the registry, `run` hands over
    through `launch`; nothing is deleted here).
  - `icon.rs` (icons as they are in the games' files, at their best: each icon's single best
    image, the largest in true colour (its depth as the icon's directory lists it too), byte for
    byte from `.ico` files and programs' icon resources, as a `.png` or a one-image `.ico` so
    Windows' own decoder shows it too; cached; nothing is decoded or scaled here, no icon
    handles, no GDI) and `data_module.rs` (a program or DLL mapped as data, for its resources:
    icons, EA's Game Definition File).
  - `covers.rs` (cover art: the user's own, the stores' originals downloaded once by id or by
    name into the cache, and meanwhile the smaller copies the stores keep on this PC; every file
    kept byte for byte, never re-encoded; a `Cover` names its file, read with `covers::read` when
    a tile needs it). Each store's facts are
    in its own module (`steam::cached_cover`, `epic::cover_url`, `gog::cached_cover`, …).
  - `picture.rs` (pictures in pure Rust: JPEG, PNG and WebP through their crates, BMP and icon
    bitmaps read here, sizes from headers, icons fitted with Catmull-Rom, the capture's PNG; no
    Windows API).
  - `graphics.rs` + `pcgamingwiki.rs` (graphics APIs, wiki links).
  - `http.rs` (WinHTTP `get`/`download`).
- **Mods**:
  - `mods/mod.rs` (`mods.json`, `FileOp` plans that undo themselves, `apply`; `CopyTree` and
    `DeleteFiles` move a folder of files as one operation).
  - `mods/dxvk.rs` (Stable, Development, HDR-mod).
  - `mods/laa.rs` (Large Address Aware for 32-bit programs: status, enable, restore; the patched
    program is recorded in `mods.json` under `laa`).
  - `mods/reshade.rs` (Stable, Development, No Addons; the DLL, `ReShade.ini`, the
    `reshade-shaders` folder, `deploy`) and `mods/reshade_layer.rs` (ReShade's global Vulkan
    layer, shared with ReShade's own setup).
  - `mods/shader_packs.rs` (ReShade ▸ Shaders: the shipped `config/reshade-shaders.json`, the pack
    cache, layout and staging) and `mods/reshade_settings.rs` (`reshade.json`: the shared
    selection and add-ons, games' own ones, and profiles).
  - `mods/reshade_addons.rs` (ReShade ▸ Addons: ReShade's `Addons.ini`, the shipped
    `config/reshade-addons.json` and the user's own; the add-on cache).
  - `mods/upscalers.rs` (DLSS, Streamline, FSR 3.1+, XeSS DLLs: the shipped
    `config/upscalers.json`, the vendors' versions, the cache, `scan`, `swap`, `restore`; swaps
    are recorded in `mods.json` under `upscalers`) and `signature.rs` (`WinVerifyTrust`).
  - `seven_zip.rs` (the shipped 7-Zip).
  - `elevate.rs` (`runas` + `--file-ops` / `--reshade-ops` / `--nvidia-ops` /
    `--gpu-scheduling`).
  - `nvidia.rs` (the game's NVIDIA driver profile through NVAPI: any DWORD settings read,
    planned and saved, with DXVK's Present Method and its flags; the driver's version and its NGX
    DLLs) and `mods/dlss_override.rs` (the DLSS overrides: `config/nvidia-overrides.json`,
    recorded in `mods.json` under `dlssOverride`).
- **My PC** (`pc/`, registry and Win32, no WMI):
  - `pc/mod.rs` (`Snapshot`, each part read on its own).
  - `pc/hardware.rs` (processor, graphics cards, memory, Windows, board, drives).
  - `pc/smbios.rs` (memory modules from the SMBIOS table).
  - `pc/displays.rs` (displays, modes, HDR, colour profiles, DDC/CI brightness, and changing them).
  - `pc/switches.rs` (GPU scheduling, Game Mode, windowed optimizations, Auto HDR).
- **Shared**:
  - `error.rs` (`Error`, `Result`, `ScanError`).
  - `json_file.rs` (where files live, load/save).
  - `security.rs` (the user's SID, whether elevated, granting file rights).
  - `clipboard.rs` (a file (`CF_HDROP`) or text (`CF_UNICODETEXT`) put on the clipboard, and its
    text read back, through a message-only window).
  - `version_info.rs` (a file's `ProductName`/`ProductVersion`, and its fixed file version).
  - `text.rs`.
  - `com.rs` (`ComGuard`).
- `examples/scan.rs` – headless check: `-v` (with each game's uninstall route), `--graphics`,
  `--icons <dir>`, `--nvidia`, `--pc`, `--covers` (each game's cover source, read only).

## Scanning
- `scan_all` runs these steps in order:
  1. The store scanners (`SCANNERS` in `lib.rs`, in priority order: when two stores report one
     install folder, the earlier keeps the game).
  2. `config::apply`.
  3. `default_program`.
  4. `dedupe_install_dirs` (a dropped duplicate gives its program to the kept game).
  5. Custom games, with `apply` too.
  6. `complete_game` (icon fallback, then `arch` from the PE header unless already set, and
     `installed`, the day the game's folder was created).
- A store that fails adds a `ScanError` to `ScanReport::errors`; it never hides other stores'
  games.
- **New store**: a module with `pub fn scan() -> Result<Vec<Game>>`, a `Store` variant, and an
  entry in `SCANNERS`.
- **Game ids** are `<store>:<store id>` and stable: they key the list and the user's files.
- **Program precedence**: the user's choice > the shipped `config/games.json` (recognised by the
  game's binaries, never store ids) > the scanner's. A chosen bitness replaces `Game::arch`.

## Files
- `json_file::user_file` → `%APPDATA%\Game Manager`: the user's `game-settings.json`,
  `custom-games.json`, `mods.json` and `reshade.json`, their own shaders and add-ons in
  `reshade\custom`, and their own covers in `covers` (`<game id>.jpg`, `:` as `_`).
  - They are loaded before saving, so an unreadable file is never overwritten, and unknown fields
    are kept.
- `json_file::local_file` → `%LOCALAPPDATA%\Game Manager`: caches, safe to delete.
- `build.rs` ships every `config/*.json` and `7-Zip\` next to `app.exe`; `config::shipped_file`
  finds them there or one folder up (examples, tests).
- **Format numbers**: bump the one whose layout changes.
  - `snapshot::VERSION`: the serde form of `Game`.
  - `icon::CACHE_FORMAT`.
  - `covers::CACHE_FORMAT` (what is cached for a cover).
  - `dxvk::CACHE_FORMAT`.
  - `reshade::CACHE_FORMAT`.

## Rules
- **Errors**: return `gm_core::Result<T>` with a `gm_core::Error` variant, never a `String`
  error. A new failure gets its own variant when a caller may want to tell it apart.
- **`unsafe`**: every block holds one operation and gets a `// SAFETY:` comment (Clippy enforces
  both). A handle that must be freed is owned by a type whose `Drop` frees it:
  `data_module::DataModule`, `ItemIdList`, `ComGuard`, WinHTTP's `Handle`, `elevate::OwnedHandle`.
- **No game-specific data in code**: knowledge of particular games goes into `config/games.json`.
  Store file formats and registry locations stay in the scanners.
- **Network goes through `http.rs`**, with a User-Agent that has contact details.
  - PCGamingWiki: one request a second.
  - GitHub's API: 60 anonymous requests an hour.
- **windows-rs bindings**: Win32 features are named after C headers (`core/Cargo.toml`). The
  bindings come from windows-rs master and change shape between commits (constants' integer
  types, `Option` vs raw pointers): fix call sites to the new signatures.
  - A function the crate has no binding for (dxva2's monitor API) is declared in its module with
    `#[link(name = "…")] unsafe extern "system"`, not through another windows-rs crate.
  - A function newer than Windows 10 1703 (mscms's per-display colour profiles) is loaded with
    `GetProcAddress`: an import would stop `app.exe` from starting on older Windows.

## Facts learned the hard way
- **Graphics APIs**: PCGamingWiki first, because files alone mislead. On 37 games:
  - GTA V imports only `d3d9.dll` but runs Direct3D 11.
  - Every Unreal game imports `d3d9.dll`/`opengl32.dll`.
  - Plug-ins import Direct3D 11/12.
  - Unity loads its API at run time.

  The answer is a suggestion; the user picks one API in Game settings, so detection keeps no
  cross-checks.
- **My PC** (each fact is in its module's `//!` doc too):
  - `ProductName` still says "Windows 10" on Windows 11: build 22000 and later are named 11.
  - NVIDIA's driver number is the last five digits of Windows' driver version (32.0.16.2112 is
    621.12).
  - The display class key keeps drivers of cards no longer plugged in, and virtual adapters
    (Parsec) have no PCI `VEN_`: a card counts when `EnumDisplayDevicesW` lists it.
  - SMBIOS memory sizes with bit 15 set are in KB; 32 GB and larger modules use the extended size.
  - `HwSchMode` is 2 for GPU scheduling on, 1 off; with no value it isn't offered.
  - `DirectXUserGlobalSettings` flags can hold more bits (2097): only the lowest is on or off.
  - `scan --pc` read everything here in about 50 ms; DDC/CI brightness is slower, and a TV
    over HDMI has none.
- **Stores**:
  - The EA app's data is encrypted: there is no EA-app launch link.
  - Xbox packaged games start only through `shell:AppsFolder\<family>!<app>`.
  - A GOG primary task can be a launcher (Witcher 3's `REDprelauncher.exe`).
  - **Ubisoft Connect** (`ubisoft.rs`):
    - The registry has only each game's folder (`Ubisoft\Launcher\Installs\<install id>`,
      forward slashes) and an Uninstall entry `Uplay Install <id>` with its name and icon.
      Entries outlive their folders (games bought on Epic and removed there).
    - Current launchers encrypt their game list
      (`%LOCALAPPDATA%\Ubisoft Game Launcher\cache\configuration\configurations.vdat`). The
      plain one an older launcher left in `<launcher>\cache\configuration\configurations` is
      no longer updated: it is read when present (programs, extra launch actions, the
      240 × 280 picture in `cache\assets`), never required.
    - That cache is keyed by the install id (field 2), not the Ubisoft id (field 1): Far Cry 3
      is 46 and 101. Entries with install id 0 can't be installed on their own.
    - Without it the game's program is the largest `.exe` outside `installer_resources`
      (which holds a 66 MB `UplayInstaller.exe`). It agreed with the cache on all 4 games.
    - `scan -v` on 4 games: every name, icon, program and bitness was right; the 2 stale
      entries were skipped.
- **Uninstalling** (`uninstall.rs`):
  - Steam: `steam://uninstall/<appid>` shows Steam's own confirmation.
  - Ubisoft Connect: `uplay://uninstall/<install id>`, the link its Uninstall entries pass to
    `upc.exe`. Not run in a check; the user tries it.
  - GOG and the EA app write a Windows Uninstall entry whose `InstallLocation` is the game's
    folder. GOG's is the game's `unins000.exe`; the EA app's is
    `Common Files\EAInstaller\<game>\Cleanup.exe uninstall_game -autologging`, unquoted, so the
    command is split after `.exe`.
  - Epic keeps no Uninstall entry: the app opens the launcher's library
    (`com.epicgames.launcher://library`). Xbox games go to `ms-settings:appsfeatures`.
  - On 37 games, `scan -v` found every Steam, GOG and EA route.
- **Cover art** (`covers.rs`):
  - **Steam's client** keeps 300 × 450 covers in `appcache\librarycache\<appid>`, as
    `library_600x900.jpg` whatever their size, or as `library_capsule.jpg`, often in a folder
    named after a hash. The 600 × 900 one: `IStoreBrowseService/GetItems` with
    `include_assets` gives `asset_url_format` and `library_capsule_2x`, under
    `shared.fastly.steamstatic.com/store_item_assets/`.
  - **GOG Galaxy** keeps 342 × 482 WebP covers in
    `%ProgramData%\GOG.com\Galaxy\webcache\<user>\gog\<id>\<hash>_glx_vertical_cover.webp`
    (image-webp reads them; WIC needed Windows' WebP extension). The original:
    `api.gog.com/v2/games/<id>` → `_links.boxArtImage`, a JPEG. It is 342 × 482 too: GOG's image
    server makes every form (`.jpg`, `.png`, `.webp`, `_glx_vertical_cover.webp`) from one
    342 × 482 source, so a GOG tile is enlarged above 150% scaling.
  - **The Epic launcher** keeps its catalog, not the pictures: the legacy launcher in
    `Data\Catalog\catcache.bin` (base64 JSON), the current one in `cache\prod\catalog-cache.json`.
    Items list a `DieselGameBoxTall` address on `cdn1.epicgames.com`: the original is a
    1200 × 1600 PNG of about 1.7 MB (its server would resize it on `?h=600&w=450&resize=1`, as a
    JPEG). Epic's GraphQL store lookup needs a browser's User-Agent: not used.
  - **Xbox**: `displaycatalog.mp.microsoft.com/v7.0/products?bigIds=<StoreId>` lists a 2:3
    `Poster`: Forza Horizon 5's original is a 1440 × 2160 JPEG of 617 KB (`?w=400&h=600` would
    resize it).
  - **Originals at the display's scaling**: a 200 × 300 DIP tile is 450 × 675 px at 225%. The old
    400 × 600 thumbnails were enlarged there; the originals are shrunk by Direct2D instead.
  - **Ubisoft Connect** has no cover by id. Its old cache's picture is 240 × 280, so the Steam
    store is asked by name first and the picture stays only when that finds nothing. On 4
    games the store had 2 under the same name (not "Beyond Good & Evil - 20th Anniversary
    Edition", nor "Tom Clancy's Splinter Cell Chaos Theory" for the entry's shorter name).
  - **By name**: the Steam store's `storesearch` is loose ("Alan Wake 2" finds a Beat Saber
    song), so only a result with the same letters counts. It found all 3 EA games and Shadow of
    War on 37; Alan Wake 2 isn't on Steam.
  - On 37 games: all 16 Steam and 3 GOG covers were on the PC, all 9 Epic addresses were in the
    catalog cache, and the first start had 48 covers cached after about 20 s.
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
- **Large Address Aware**:
  - **The flag**: bit `0x0020` of the COFF Characteristics (PE offset + 22); only x86 PE32 programs
    are patched.
  - **The checksum** (optional header + 64) is the folded 16-bit word sum without the field, plus
    the file's length (`pe::checksum`). It reproduces the stored checksum of Windows' own programs
    exactly. It is kept valid when a program has one; 0 stays 0.
  - **Steam's DRM wrapper** (SteamStub) adds a `.bind` section (Tomb Raider: Legend's `trl.exe`).
    A changed program may not start, so the app warns but still patches.
  - **Updates**: a game update replaces the patched program, and the old backup is then stale and
    removed.
- **ReShade**:
  - **Builds**: every setup (reshade.me's `ReShade_Setup_<v>_Addon.exe` and `ReShade_Setup_<v>.exe`,
    and the CI's "ReShade Setup" artifact) appends a ZIP of `ReShade32/64.dll` and their layer
    `.json` files. The ZIP's offsets count from its own start, not the file's: it starts at the
    central directory minus the offset the directory states (`appended_zip`).
  - **Add-ons**: the CI's `Release` configuration is `RESHADE_ADDON=2` (full, like `_Addon`); the
    official no-add-on build is `Release Signed`, `=1`. CI artifacts expire after 90 days.
  - **As its setup does**: `d3d9.dll` (D3D9), `dxgi.dll` (D3D10–12), `opengl32.dll`; Direct3D 8
    needs d3d8to9 or DXVK; a DLL whose ProductName is "DXVK" means the Vulkan layer. Default
    effects are the `Enabled=1`/`Required=1` packages of `EffectPackages.ini`. In a protected
    folder, `ReShade.ini`, `ReShadePreset.ini` and `ReShade.log` are created and given to the
    user (Modify), or ReShade cannot save there.
  - **The Vulkan layer**: `%ProgramData%\ReShade` (`ReShade32/64.dll` + `.json`), registered under
    `HKLM\SOFTWARE\Khronos\Vulkan\ImplicitLayers` (32-bit under `WOW6432Node`); the folder grants
    ALL APPLICATION PACKAGES read & execute. `ReShadeApps.ini` (UTF-8 with BOM) has one
    `Apps=` line of paths split by commas, so a path with a comma can never match; it is left
    byte for byte except for the game's own entry. ReShade's setup removes the layer when the
    list ends empty, and so does Game Manager.
  - **Shader packs**: GitHub's and GitLab's branch archives answer HEAD with an ETag, which tells whether a
    pack changed. Some releases carry no files (PumboAutoHDR): use the branch archive.
    `cargo test -p gm-core live_packs -- --ignored --nocapture` downloads every pack and reports
    its effects.
  - **Add-ons**: some entries of `Addons.ini` link a plain `.addon` as `DownloadUrl64`; it is
    kept as `.addon64`. Its links can be broken (ShaderToggler's 32-bit one): one bitness that
    downloads is enough. `live_addons` downloads every add-on. A No Addons build gets none.
- **Upscalers**:
  - **Sources**: only the vendors' own signed DLLs, from their GitHub repositories at each tag.
    - NVIDIA/DLSS `lib/Windows_x86_64/rel` (SR since 2.2.1, RR 310.1.0, FG 310.4.0).
    - intel/xess `bin` (SR since 1.2.0, as 1.0 and 1.1 need more DLLs; DX11, FG, XeLL 2.0.1).
    - FidelityFX-SDK `PrebuiltSignedDLL` (1.1.x) and `Kits/FidelityFX/signedbin` (2.x).
    - Streamline only inside its release archives since 2.7.32, `bin\x64\` (production;
      `bin\x64\development\` is not). Older tags hold Git LFS pointers (real files on
      `media.githubusercontent.com`).
  - **Streamline 1 games can't load Streamline 2**: the catalog's `sameMajor` offers only
    versions of the game's own major version, and every listed version is 2.x.
  - **No community archive**: DLSS Swapper's is GPL-3.0, and its author pays for its hosting.
  - **Versions**: the SDK's tag is not always the DLL's version. DLSS SDK 2.2.1 ships DLL
    2.2.16, FidelityFX SDK 1.1.4 DLL 1.0.1, XeSS SDK 3.0.2 DLL 2.0.2. Version strings can read
    "2,14,16,0", so the fixed file version is used.
  - `cargo test -p gm-core live_upscalers -- --ignored --nocapture` downloads the oldest
    version of every kind (about 450 MB, Streamline's archive included) and checks each DLL.
- **NVIDIA DLSS overrides** (`mods/dlss_override.rs`):
  - **Settings**: from nvidiaProfileInspector's `CustomSettingNames.xml`, group "05 - Upscaling
    and Frame Generation", copied into `config/nvidia-overrides.json` with each one's minimum
    driver. NVIDIA adds presets with drivers: add them there.
  - **Rights**: with driver 621.12 all of them were written without administrator rights.
  - **The driver's DLLs**: `%ProgramData%\NVIDIA\NGX\models\<model>\versions\<n>`, with n =
    major << 16 | minor << 8 | patch (20318480 = 310.9.16). The models are `dlss`, `dlssd`,
    `dlssg` and `sl_*_0`.
  - **The driver's version**: `NvAPI_SYS_GetDriverAndBranchVersion` (`0x2926AAAD`), 62112 for
    621.12.
  - **What the NVIDIA app writes**: resetting its DLSS override writes every setting as Off or
    N/A, as the user's own values in the profile. Most profiles inherit them from the base
    profile instead.
  - **One profile for two records**: DXVK's Present Method and the overrides share the `.exe`'s
    profile. Each keeps its own originals. A profile either record says Game Manager created
    counts as created for the other too (the `ours` argument of `nvidia::plan_settings`), and
    `apply` deletes it only once it is empty.
  - **Checks**:
    - `cargo test -p gm-core live_overrides_round_trip -- --ignored` writes to a made-up `.exe`'s
      new profile, alone and shared with DXVK's.
    - `scan --nvidia` lists what each profile holds.
