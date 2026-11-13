---
name: check-app
description: Run Game Manager's window from an agent session and check a change in it. Build into target\agent, start it, drive it through UI Automation and posted input, capture what it shows (its real pixels or its own drawing), and guard the user's window, settings and clipboard. Also icon diagnostics (a program's icon resources, an .ico file's images, what Windows' decoder shows). Use it to verify a UI or behaviour change in the running app, or to investigate game icons.
---

# Checking Game Manager from an agent session

The scripts are in `scripts/`. `gm.ps1` is a library for Windows PowerShell 5.1: dot-source it
at the start of each PowerShell call. Outputs (captures, backups) go to `target\agent\check`.

## Never break these
- **The user's window**: never close or kill it. Build into `target\agent` and close only
  `target\agent\debug\app.exe`. `gm.ps1` starts and closes nothing else, and a project hook
  blocks commands that would stop other `app.exe` processes.
- **The user's files**: before a check, back up `%APPDATA%\Game Manager` in both views
  (`Backup-GmSettings` and `settings-real.sh backup`). After it, compare both
  (`Compare-GmSettings` and `settings-real.sh compare`). Only restore (`Restore-GmSettings`) after
  checking that the check alone made the changes.
- **The clipboard is the user's**: `Save-GmClipboard` before a Cut, Copy or Paste check, and
  `Restore-GmClipboard` after.
- **My PC changes the user's own PC**: its switches write the real registry
  (`HKCU\Software\Microsoft\GameBar`, `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`). Note
  their values first and put them back exactly (a missing `AutoGameModeEnabled` is deleted
  again). Resolution, refresh rate, HDR, colour profile, brightness and GPU scheduling (UAC)
  change the user's screen or need a restart: ask the user to try them.
- **Mods**: never install into the user's real games (see "Mods with a throwaway game").
- **The game menu's Uninstall…** hands the game to its store for real: open its dialog and
  Cancel. `scan -v` prints each game's route without running it.

## Build, start, stop
```bash
CARGO_TARGET_DIR=target/agent cargo build -p app
```
```powershell
Set-Location (git rev-parse --show-toplevel)
. .\.claude\skills\check-app\scripts\gm.ps1
Backup-GmSettings
Start-GmApp                      # closes an earlier agent build, maximizes, waits 6 s for the library
Select-GmGame 'Crysis Remastered'
Save-GmScreen 'page' -Crop 380, 50, 200, 100
Stop-GmApp
Compare-GmSettings
```
A later PowerShell call picks up the running build with `Connect-GmApp`. `cargo test` doesn't
rebuild `app.exe`: build again before checking a change, or you'll be checking a stale binary.

## gm.ps1
| Function | Does |
| :- | :- |
| `Start-GmApp [-LoadSeconds 6] [-Normal]`, `Connect-GmApp`, `Stop-GmApp` | Start (with `GAME_MANAGER_CAPTURE` set), find again, or close the agent build. |
| `Get-GmScale` | Pixels per DIP of the window's monitor (2.25 on the user's display). |
| `Show-GmTree [-Type ListItem]` | Every element: type, name, automation id, screen rectangle, disabled. |
| `Find-GmElement -Name/-Like -Type -Index` | One element; `-Index` picks among repeated names. |
| `Invoke-`, `Select-`, `Switch-`, `Expand-GmElement`; `Get-`/`Set-GmValue` | UI Automation's Invoke, SelectionItem, Toggle, ExpandCollapse and Value. |
| `Select-GmGame 'Name'` | Selects a sidebar row, which opens the game's page. |
| `Get-GmCenter $el` | An element's centre in screen pixels. |
| `Send-GmClick x y [-Right] [-Double]`, `Send-GmDrag` | Posted mouse messages at screen pixels. |
| `Send-GmText 'text'`, `Send-GmKey 0x1B [-Repeat n]`, `Send-GmMenuKey` | `WM_CHAR`, `WM_KEYDOWN`/`WM_KEYUP`, and the Menu key (`WM_CONTEXTMENU`, lParam −1). |
| `Save-GmDrawing 'name'` | The app draws itself at 1 px per DIP (`WM_APP + 9`). |
| `Save-GmScreen 'name' [-Crop x, y, w, h]` | The window's real pixels at the display's scaling (`PrintWindow`, `PW_RENDERFULLCONTENT`); the crop is in DIPs from the window's top-left. |
| `Backup-`, `Compare-`, `Restore-GmSettings` | The user's settings (JSON files and own covers), packaged view. |
| `Save-`, `Restore-GmClipboard` | The clipboard's text. |

## Seeing the window
- **Look at the PNGs with the Read tool.**
- **`Save-GmDrawing`** shows layout, text, colours and text boxes as the renderer draws them, at
  one pixel per DIP.
- **`Save-GmScreen`** shows what Windows puts on screen at the real scaling. Use it for sharpness,
  blur or pixel-exact checks (icons, covers), and crop to the part that matters.
- **`Show-GmTree`** answers most checks as text: names, values, enabled state and positions.

## Driving the window
- **UI Automation** sees every control by name (buttons, rows, tabs, drop-downs, text boxes with
  their Value, sliders). Invoke, SelectionItem, Toggle, ExpandCollapse, Value and RangeValue act
  like clicks, typing and dragging; Focus and Expand scroll the element into view first.
- **Names repeat**: "ReShade" is a tab, a view pill (both TabItems, the tab first) and a card. Pick
  elements by control type, and the nth match where names repeat.
- **Typing**: `Send-GmText` and `Send-GmKey` (Backspace, Delete, the arrows, Home, End, Enter,
  Escape). Ctrl and Shift can't be held by posted messages, because the app reads the real key
  state. Check Ctrl shortcuts through the text box menu and the unit tests
  (`text_input::edit_action`). A text box's Value reads and sets its text.
- **Right-clicks and drags** can be posted (`Send-GmClick -Right`, `Send-GmDrag`). Shift+F10
  reads the real keyboard, so the user tries it.
- **The Open dialog** (class `#32770`) is a child of the app's window in the UI Automation tree,
  not a top-level window. It shows its Win32 controls as plain panes. Take the
  `NativeWindowHandle` of the last element with AutomationId `1148` (the `Edit` inside the combo
  boxes) for `WM_SETTEXT`. Then `PostMessage` `BM_CLICK` to the AutomationId `1` element that has
  a window handle (Open; an item of the file list has the same id).

## What can't be done here
- **No input desktop**: drag and drop from Explorer and the UAC prompt can't be done (the cursor
  reads 0,0 and there is no foreground window). Ask the user to try them on a build they run.
  ReShade's Vulkan layer always needs the UAC prompt, so the user tests it.
  - With computer use turned on in the Claude app, Game Manager can get full control: real
    clicks, typing, dragging and keys, with screenshots shrunk to about 1400 px wide (pixel checks
    still use `Save-GmScreen`). UAC runs on a separate secure desktop, which it isn't expected to
    reach.
- **Packaged session**: programs started from the Claude desktop app run inside its package.
  - Their `%LOCALAPPDATA%` writes are redirected to `AppData\Local\Packages\Claude_…\LocalCache`,
    and so are `%APPDATA%` writes. PowerShell and the app see this view; Git Bash reads the real
    files. Compare the user's files from both before calling them untouched.
  - Not every write is redirected: on 2026-11-13 a cover chosen in the agent build was written to
    the user's real `%APPDATA%\Game Manager\covers` (a folder that already existed there), and
    Reset cover deleted it there. Expect a check's writes in either place.
  - Deleting a real folder there fails with os error 4395 (seen with an old DXVK cache folder).
  - The user's own runs are not redirected.
- **DXVK**: when the user's DXVK cache holds a build of an older layout, Install has to replace it
  and fails with os error 4395. ReShade and LAA can be checked; ask the user to try DXVK.

## Mods with a throwaway game
Never install into the user's real games, whose folders may hold their own DXVK or ReShade.
- Set up a temp folder with a copy of `SysWOW64\notepad.exe` as `game.exe` and a fake `d3d9.dll`.
- Add an entry to `custom-games.json` and `"graphicsApi": "DirectX9"` to `game-settings.json`.
- Drive Install, Settings, Reinstall and Uninstall, then compare the folder with how it started.
- `app.exe --file-ops <percent-encoded ops>` can be run directly (not elevated).
- **Upscalers**: give the throwaway game real old vendor DLLs (from the upscaler cache after
  `live_upscalers`) in a subfolder, then swap, swap again and restore, and compare the folder.
- **"All games with ReShade" reaches the user's real games too.** Deploy in ReShade ▸ Shaders, and
  Apply in (or leaving) ReShade ▸ Add-ons, reach every game with ReShade in `mods.json` that has
  no choice of its own. Before clicking them, check that only throwaway games have a `reshade`
  record, or test with a `mods.json` that holds only them. "Only <game>" reaches just that game.
- **Live downloads**: `cargo test -p gm-core live_builds -- --ignored` fetches each ReShade variant
  and the default effects; `live_upscalers` one version of each upscaler DLL.

## Icons
- `cargo run -p gm-core --example scan -- --icons <dir>` saves each game's icon as the app gets it
  (its best image, `.png` or a one-image `.ico`) and prints its size and depth. `-v` prints where
  each icon comes from.
- `scripts\icon-resources.ps1 -File <exe or dll> [-Index n]`: a program's icon group, each image
  as the directory lists it beside its own header. Use it when an icon's depth or size looks wrong.
- `scripts\ico-file.ps1 -File <ico>`: the same for an `.ico` file.
- `scripts\decode-view.ps1 -Dir <folder>`: what Windows' own decoders (Explorer's) show for each
  file: frames, size, pixel format, failures.
