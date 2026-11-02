# app (the window)

WinUI 3 through `windows-reactor`, a preview that changes often: when unsure of an API, read the
source of the commit in `Cargo.lock` (`~/.cargo/git/checkouts/windows-rs-*/<short rev>/crates/libs/`
`reactor`, `pickers`, samples in `crates/samples/`) instead of guessing.

## Structure
- `main.rs` – `Message`, the `GameManager` state, `render` (top bar, sidebar, main panel,
  dialogs) and `handle`. `update` runs `handle`, then `request_graphics`,
  `request_hdr_mod_configs`, `request_laa` and `request_reshade`, so whatever selects a game or
  shows HDR-mod looks things up once.
- `mods_view.rs` (`impl GameManager`) – the Enhancements and Mods expanders and their dialog.
- `reshade_panel.rs` (`impl GameManager`) – the top bar's ReShade button and its Global Shaders
  and Global Addons panels; `ShadersDraft` is UI-free and unit-tested. Deploy, and closing
  Global Addons after a change, run `reshade::deploy` for every game with ReShade.
- `edit.rs` (the Game settings draft) and `library.rs` (search, filter, selection, list key) are
  UI-free and unit-tested: put logic there, not in views.
- Blocking work runs through `ComponentContext::spawn_background` and comes back as a `Message`,
  never on the UI thread. Errors arrive as `String`s (`to_string()` where received) and show in
  the error bar.
- `main()` with `--file-ops`, `--reshade-ops` or `--nvidia-ops` runs that work and exits before
  any UI (the copy started as administrator). A failure to start shows a message box (there is no console),
  except a missing Windows App Runtime, whose install the reactor already offered.

## windows-reactor quirks (each one cost a bug)
- **Live lists**: it applies `selected_index` while it is still changing a live `ListView`'s rows,
  which selected a row that did not exist yet (`E_INVALIDARG`, the app closed) or left rows
  showing another game's name. The game list is a keyed `Grid` child whose key
  (`library::list_key`) hashes the visible ids, so another set of games builds a new `ListView`.
  There is no scroll-into-view.
- **ComboBox items** may change while an item is selected (filter counts, "As detected: …",
  HDR-mod configs): the reactor suppresses the selection event its own write raises and applies
  `selected_index` again. `on_selection_changed` can still report `None` (the list losing its
  selection), so messages carry an `Option` and ignore `None`.
- **Menus** attach only to `Button`/`DropDownButton`/`MenuBarItem`, and nothing opens one at a
  point (`AppContext::show_menu_at` went with the imperative `App` API). So the right-click menu
  is drawn by the app:
  - Each row is a `Border` (transparent, `min_width(ROW_WIDTH)` so all of it is hit-testable)
    sending `RowPointer`.
  - `row_menu_view` adds a last root-grid layer: a transparent backdrop that closes the menu on
    any press, and a sibling `Canvas` with the menu, kept inside the window via `on_window_size`.
  - Escape is an `on_key_up` on the root `Border`, present only while the menu is open.
- **Dialogs**: a view carries one `ContentDialog` (`.content_dialog()`): Add on the top bar,
  Remove on the body, the mod dialog on the main panel's `ScrollViewer`. WinUI opens one at a
  time, so whatever opens one is disabled while another is open.
- **Drop**: the root is a transparent `Border` (so empty areas are hit-testable) whose
  `drop_policy` accepts storage items only while `can_add_game`.
- **Wide panels**: `ContentDialog` is at most about 548 px wide, and the reactor cannot change
  that. ReShade's panels are a last root-grid layer instead (as the game menu is): a dimmed
  `Border` that takes every click, and the panel centred on it. Escape closes it, like the menu.
- **Messages cross threads** (`spawn_background` needs `Message: Send`): a menu's `Key` is
  passed on as a `String`.
- **Tuple children stop at 16**: group rows, or pass a `Vec<View>` when rows come and go.
- **Controls**:
  - `Button` has no foreground: the red ✕ is a `FontIcon` with `ThemeBrush::SystemCritical`.
  - Disabled controls show no tooltip, so reasons go in a hint line.
  - `.tooltip()` returns a `View`.
  - Use `GridLength::Auto.min(px)` for a column that must grow.
- `windows_pickers::OpenFilePicker::request` shows the Open dialog for the app's window, which is
  disabled while it is open.

## Game settings and Mods
- **Game settings** (`game_settings`, `edit.rs`):
  - The draft starts from the game's current values. `EditDraft::for_game` drops it once another
    game is selected or a scan changed the game.
  - Save stores only changed fields (blank = as detected) with `config::save_edits`; Reset calls
    `config::clear_edits`. Each is followed by a Rescan, so `config::apply` produces name, Play,
    program and bitness in one place.
  - In the Bitness and Graphics API drop-downs, item 0 is "As detected: …".
- **Mods**:
  - One job runs at a time (`mod_job`), and `mods.json` is replaced by what each job returns.
    After a failed job it is read again, since a job may have saved part of its work.
  - **DXVK settings' NVIDIA section** (for every variant): opening the dialog reads the game's
    driver profile in the background (`NvidiaLoaded`, matched to the selected game; hidden when
    there is no NVIDIA driver).
    - The Present Method and flags drop-downs start with "Original: …" (the recorded original,
      else what the profile holds now).
    - Reset to original sets both back; Save applies them through `dxvk::save_settings`.
    - Uninstalling DXVK leaves them.
  - **Shared columns**: mod rows share `mod_columns()`, fixed widths, so each row's cells line up
    with the others'.
  - **Enhancements** (above Mods) holds ReShade's row, with settings for its variant. Its install
    mode (DLL or Vulkan layer) depends on DXVK in the game's folder, which `request_reshade`
    reads in the background with the layer's list. DXVK's Install is disabled while ReShade is
    installed as one of DXVK's DLLs (`dxvk::reshade_dll`).
  - Mods use `mod_api`: the API chosen in Game settings, else the only one detected.
  - **The LAA row**: under DXVK, only for 32-bit games (`game.arch == X86`, the chosen bitness
    counts), with no settings.
    - Its status comes from `request_laa` (in `update`, like `request_graphics`; cleared after a
      scan, a mod job or `ModsLoaded`).
    - `mod_job` carries a `ModKind`, so only the working row shows "Working…".
