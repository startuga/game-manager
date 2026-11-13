# app (the window)

A native Win32 window drawn with Direct2D and DirectWrite: windows-rs's `windows-window` (the HWND
and message loop), `windows-canvas` (Direct2D, `system` host) and `windows-text` (DirectWrite). No
WinUI, no XAML, no Windows App Runtime. These crates are 0.101 previews that change often: when
unsure of an API, read the source of the commit in `Cargo.lock`
(`~/.cargo/git/checkouts/windows-rs-*/<short rev>/crates/libs/{window,canvas,text,threading}`, samples
in `crates/samples/`) instead of guessing.

## Structure
- `main.rs` – `Message`, the `GameManager` state, `update` (`handle`, then the `request_*`
  lookups), `go_to` (the pages), the elevated modes (`--file-ops`, `--reshade-ops`,
  `--nvidia-ops`, `--gpu-scheduling`).
- **Views**:
  - `app_view.rs` (title bar, error bar, the sidebar's resize grip, layers: the game menu and
    dialogs).
  - `game_menu.rs` (a game's menu from a right-click or the Menu key, which leaves the page as
    it is; `MenuAction`, `MenuMessage`; Pin to quick access, Hide, Choose and Reset cover, and
    the Rename and Uninstall dialogs).
  - `sidebar.rs` (search, the Sort and filters button, the pages, the view's sections, game
    rows, Add a game) and `filter_menu.rs` (that button's flyout: Sort, Smart filters, Smart
    Collections; `FilterMessage`; the view saved when it closes).
  - `library_page.rs` (the Library grid: a cover tile per game, by store; the window opens on it)
    and `covers.rs` (loading the core's covers, decoded for the tiles; Choose and Reset cover).
  - `game_page.rs` (header, launch bar, tabs, Overview; picks the page, each with its own
    scroll position).
  - `mods_view.rs` (the ReShade and DXVK & LAA tabs, the mod dialogs), `reshade_panel.rs` (the
    ReShade tab's Shaders and Add-ons views), `upscalers_view.rs` (the Upscalers tab and its
    state), `upscaler_versions.rs` (its Versions section), `dlss_override_view.rs` (the NVIDIA
    overrides tab), `my_pc.rs` and `my_pc_page.rs` (My PC: its state and messages, and its view).
- **Pages, tabs and views**: `Page` (Library, Game, My PC) is the sidebar's choice; `GameTab` the
  game page's (NVIDIA overrides only with an NVIDIA driver, read at start); `ReshadeView` (ReShade,
  Shaders, Add-ons) the ReShade tab's, as `widgets::segmented` pills.
  - Change pages through `go_to` and games through `select`. Leaving the game page, the ReShade
    tab, its view or the game runs `leave_reshade_view`: Add-ons saves and deploys a change (as
    Apply does), Shaders drops its draft (as Discard changes does).
  - `open_reshade_view` (from `update`) opens the view's panel when it shows without one, after a
    Deploy has finished; a list that fails to load sends the view back to ReShade.
  - Escape closes the menu, then the Sort and filters flyout, then a dialog, then leaves My PC,
    or Shaders and Add-ons for ReShade.
- **UI-free logic**, unit-tested: `edit.rs` (the Game settings draft), `library.rs` (search,
  the view's filters, collections and sort, the lists' sections, selection). Put logic there,
  not in views: the sidebar and the Library page both list `library::sections`.
- `ui/` – the toolkit:
  - `el.rs` (the element tree views return; builders), `layout.rs` (a small flexbox, tested),
    `input.rs` (hit-testing, hover, focus, keys, scroll, drop-downs; `UiState`; tested),
    `render.rs` (drawing; text and bitmap caches), `uia.rs` (UI Automation), `theme.rs` (design
    tokens), `widgets.rs` (controls), `image.rs` (WIC decoding, covers at their tile's pixel
    size with rounded corners, PNG capture).
- `shell/` – the window: `mod.rs` (`App`, `run`, messages → toolkit), `window.rs` (title bar,
  frame, DPI), `edit_box.rs` (text boxes), `ctx.rs` (background work, effects), `dialogs.rs`
  (Open dialog, start error), `efficiency.rs`.

## Toolkit rules
- **Elm style**: every `Message` rebuilds the view and lays it out; hover and the like only
  redraw. State a view can't know (hover, scroll offsets, focus, the open drop-down) is in
  `UiState`, keyed by element id: give an element a `key` when that state must follow it across
  frames (game rows, scroll views, text boxes, drop-downs).
- **Design tokens only**: colours, sizes and fonts come from `theme.rs`; controls from
  `widgets.rs`.
- **Layout**: `Size::Px`, `Auto` or `Fill(weight)` along a row or column; `halign`/`valign`
  across (both among `layers`). `row` centres its children vertically unless they say otherwise,
  so a column that must fill a row's height needs `fill_height()`. A `scroll` clips its content.
- **Text boxes** are real EDIT controls (`widgets::text_box`, `shell::edit_box`): IME, clipboard,
  undo and screen readers are Windows'. Their text is written only while they have no focus; a
  child window isn't clipped by a scroll view, so one shows only while its frame is fully visible.
  - **Keys**: Enter sends a box's `on_enter`; Escape empties a box made with `.clear_on_escape()`
    (the shell writes the EDIT with `set_text` and sends `on_change("")` itself, since the EDIT's
    own change notice comes while the window is busy and is dropped).
  - **Ctrl+F** focuses the box `App::search_box` names (the sidebar's `SEARCH_KEY`), its text
    selected.
  - **A click, or a UI Automation Invoke**, on a drawn control takes the keyboard from a text box,
    so its text follows the app again (the search's ✕).
- **Search** (`widgets::search_box`, `library::matches`, tested): the matching rules are in
  `library.rs`'s `//!` doc.
- **Layers** (menus, dialogs): a `modal` element stops hit-testing and scopes Tab to its layer;
  Enter, Space and the Menu key don't reach a focused element under it. Escape goes to
  `App::escape`. A dialog's body scrolls (`app_view::dialog`), so a tall one fits the window. A
  dialog whose first control is a text box gives it the keyboard as it opens (Rename, Add a
  game).
- **Menus**: a `modal` backdrop with an `on_click` is light-dismiss: a left-click closes the
  menu, and a right-click sends the `on_context` of what is under it (a new menu replaces the
  open one, as Windows' menus do), else closes it. The panel is `opaque`, so clicks on its header
  and lines leave it open. Items are `Role::MenuItem` (UI Automation MenuItems with Invoke); ↑↓
  go round the top layer's items. `Key::Menu` (the Menu key and Shift+F10, a keyboard
  `WM_CONTEXTMENU`) sends the focused element's `on_context` at its lower-left; the menu's first
  item then takes focus, and focus goes back once it closes (`UiState::follow_menu`).
- **Flyouts beside a button** (`filter_menu.rs`): the layer sits in a `layers` over just the part
  of the window the button is in (the sidebar and the page), so the panel is placed by padding
  worked out from the sidebar's width (`flyout_origin`), whatever is above it. That `layers` is always there, with or without the flyout, so
  the elements under it keep their ids. Its lines are check boxes and list items (Toggle and
  SelectionItem in UI Automation); Tab moves through them.
- **Pictures**: `el::image` draws a decoded picture scaled to fit, uploaded once per `key`. Covers
  are decoded at their tile's exact pixel size for `Ctx::scale` with their corners cut into the
  alpha (`image::decode_covers`), so they need no clipping; `App::scale_changed` makes them again.
  A tile's `hover_overlay` tints over its children (a picture hides a hover fill).
- **Dragging**: `on_drag` sends the pointer's movement while the button is held (the sidebar's
  `resize_grip`, which also shows the resize cursor).
- **Sliders** (`el::slider`): a press or drag moves the thumb in `UiState::sliding` (drawn only);
  the app gets the value on release, or at once from the arrows, Page Up/Down, Home and End, and
  from UI Automation's RangeValue.
- **Rows measure filling children at their share**: wrapped text beside a fixed label is as tall
  as the lines it needs in the width left to it (`layout::row_sizes`).
- **Motion**: hover fills fade over `input::HOVER_FADE`; spinners turn. A frame that moves reports
  `Drawn::animating` and the loop draws again; otherwise nothing draws at idle.
- **Background work** goes through `Ctx::spawn_background` (the thread pool; the message comes
  back through `WM_WAKE`), or `Ctx::spawn_stream` for work that sends several messages as it goes
  (covers). Modal dialogs (the Open dialog) and window commands are `Effect`s, run by the loop
  with no state borrowed.
- **Re-entrancy**: the shell's state is borrowed with `try_borrow_mut`; a message sent while it
  is busy gets default handling. Never send our own window a message from inside `update` and
  expect it handled; `WM_CTLCOLOREDIT`, `WM_NCCALCSIZE` and `WM_GETMINMAXINFO` need no state.
- **Bindings**: the `windows` crate at this commit uses C types: `HWND` and other handles are raw
  pointers (`is_null`), `WPARAM`/`LPARAM` are `usize`/`isize`, `WM_*`/`WS_*` constants are `i32`
  (the shell matches messages as `i32`), many methods return `HRESULT` (`.ok()?`), and `VARIANT` is
  the raw union (`uia.rs` builds them).
- **Title bar**: drawn by the view; elements keyed `shell::CAPTION_*` tell `WM_NCHITTEST` where to
  drag and where minimize, maximize (Snap Layouts) and close are.

## Checking it from an agent session
- **UI Automation** sees every control by name (buttons, rows, tabs, drop-downs, text boxes with
  their Value, sliders). Invoke, SelectionItem, Toggle, ExpandCollapse, Value and RangeValue act
  like clicks, typing and dragging; Focus and Expand scroll the element into view first.
- **My PC**: the switches write the user's real registry (`HKCU\Software\Microsoft\GameBar`,
  `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`). Note their values first and put them back
  exactly (a missing `AutoGameModeEnabled` is deleted again). Resolution, refresh rate, HDR,
  colour profile, brightness and GPU scheduling (UAC) change the user's screen or need a restart:
  ask the user to try them.
- **The window draws itself to a PNG**: start the app with `GAME_MANAGER_CAPTURE=<path>` in its
  environment, then post `WM_APP + 9` to its window; the Read tool shows the PNG. Text boxes are
  separate windows, so their text isn't in the capture (read it through UI Automation).
- **Right-clicks** can be posted (`WM_RBUTTONDOWN`/`WM_RBUTTONUP` at client coordinates), and
  so can a drag (`WM_LBUTTONDOWN`, `WM_MOUSEMOVE` with `MK_LBUTTON`, `WM_LBUTTONUP`). The Menu
  key is a posted `WM_CONTEXTMENU` with `lparam` −1; Shift+F10 reads the real keyboard, so the
  user tries it.
- **The game menu's Uninstall…** hands the game to its store for real: in a check, open its
  dialog and Cancel. `scan -v` prints each game's route without running it.
- **Names repeat**: "ReShade" is a tab, a view pill (both TabItems, the tab first) and a card.
  Pick elements by control type, and the nth match where names repeat.
- **DXVK in an agent session**: when the user's DXVK cache holds a build of an older layout,
  Install has to replace it and fails with os error 4395 (root `CLAUDE.md`). ReShade and LAA can
  be checked; ask the user to try DXVK.

## Game settings and Mods
- **Game settings** (`game_page::game_settings`, `edit.rs`):
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
    there is no NVIDIA driver). The Present Method and flags start with "Original: …" (the
    recorded original, else what the profile holds now); Reset to original sets both back; Save
    applies them through `dxvk::save_settings`; uninstalling DXVK leaves them.
  - ReShade's install mode (DLL or Vulkan layer) depends on DXVK in the game's folder, which
    `request_reshade` reads in the background with the layer's list. DXVK's Install is disabled
    while ReShade is installed as one of DXVK's DLLs (`dxvk::reshade_dll`).
  - Mods use `mod_api`: the API chosen in Game settings, else the only one detected.
  - **LAA**: only for 32-bit games (`game.arch == X86`, the chosen bitness counts). Its status
    comes from `request_laa` (cleared after a scan, a mod job or `ModsLoaded`). `mod_job` carries
    a `ModKind`, so only the working row shows "Working…".
  - **NVIDIA overrides** (`dlss_override_view.rs`): the tab's draft is made for the game it
    shows, and the game's profile read then (`request_overrides`, `OverridesLoaded`); a save makes
    it again from the profile. When the profile no longer holds what Game Manager set
    (`OverrideState::changed_elsewhere`, the NVIDIA app writes the same settings), a warning names
    them and Save sets them again. To change a profile from outside in a check, run
    `app.exe --nvidia-ops` with a plan (`[uri]::EscapeDataString` of its JSON).
