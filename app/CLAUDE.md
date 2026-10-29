# app (the window)

WinUI 3 through `windows-reactor`, a preview that changes often: when unsure of an API, read the
source of the commit in `Cargo.lock` (`~/.cargo/git/checkouts/windows-rs-*/<short rev>/crates/libs/`
`reactor`, `pickers`, samples in `crates/samples/`) instead of guessing.

## Structure
- `main.rs` – `Message`, the `GameManager` state, `render` (top bar, sidebar, main panel,
  dialogs) and `handle`. `update` runs `handle`, then `request_graphics` and
  `request_hdr_mod_configs`, so whatever selects a game or shows HDR-mod looks things up once.
- `mods_view.rs` (`impl GameManager`) – the Mods expander and its dialog.
- `edit.rs` (the Game settings draft) and `library.rs` (search, filter, selection, list key) are
  UI-free and unit-tested: put logic there, not in views.
- Blocking work runs through `ComponentContext::spawn_background` and comes back as a `Message`,
  never on the UI thread. Errors arrive as `String`s (`to_string()` where received) and show in
  the error bar.
- `main()` with `--file-ops <ops>` runs `gm_core::mods::run_encoded` and exits before any UI (the
  copy started as administrator). A failure to start shows a message box (there is no console),
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
- **Menus** attach only to `Button`/`DropDownButton`/`MenuBarItem`, and `AppContext::show_menu_at`
  needs `App::run_with` + `open_component_window`, which never exits when the window closes. So
  the right-click menu is drawn by the app:
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
  - DXVK's API is `dxvk_api`: the one chosen in Game settings, else the only one detected.
