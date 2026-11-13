# app (the window)

A native Win32 window drawn with Direct2D and DirectWrite: windows-rs's `windows-window` (the HWND
and message loop), `windows-canvas` (Direct2D, `system` host) and `windows-text` (DirectWrite). No
WinUI, no XAML, no Windows App Runtime, and no GDI: the window has no child windows, and nothing
but Direct2D draws it (the app's `Cargo.toml` leaves out `wingdi`, so a GDI call doesn't
build). These crates are 0.101 previews that change often: when unsure of an API, hover it or go
to its definition with the LSP tool, or read the source of the commit in `Cargo.lock`
(`~/.cargo/git/checkouts/windows-rs-*/<short rev>/crates/libs/{window,canvas,text,threading}`,
samples in `crates/samples/`) instead of guessing.

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
    and `covers.rs` (the core's covers kept as their files, the stores' originals decoded at full
    size only while their tiles are near the screen, a stand-in until each original arrives;
    Choose and Reset cover).
  - `icons.rs` (game icons: the core's best image of each kept, decoded for the 24 and 64 DIP
    sizes at the window's pixel scaling, again when it changes; `icon` draws one).
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
    tokens), `widgets.rs` (controls), `image.rs` (icons at the pixels they take and covers at
    full size, decoded in pure Rust through `gm_core::picture`; the PNG capture), `text_edit.rs` (a
    text box's editing: caret, selection, words, undo; tested).
- `shell/` – the window: `mod.rs` (`App`, `run`, messages → toolkit), `window.rs` (title bar,
  frame, DPI), `text_input.rs` (text boxes' keys, characters, clicks, clipboard, caret blink,
  menu and IME), `ctx.rs` (background work, effects), `dialogs.rs` (Open dialog, start error),
  `efficiency.rs`.

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
- **Text boxes** are drawn (`widgets::text_box`, `Kind::Edit`), like every other control.
  - **The app owns the text**: a box's editing state (`text_edit::TextEdit` in `UiState::edits`:
    caret, selection, undo) sends each change through `on_change`, and `sync_edits` takes the
    view's text after every layout. A text the box didn't send (Escape emptied it, Reset) puts
    the caret at the end and starts its history over.
  - **Input** is the shell's (`text_input.rs`'s `//!` doc lists the keys): `WM_CHAR` types, the
    point of a click becomes an offset through the text's own DirectWrite layout, the clipboard is
    `gm_core::clipboard`'s text, and the IME composes inline (IMM32).
  - **Drawing** (`render.rs`): the text scrolled so the caret shows, the selection, the blinking
    caret, an IME composition underlined, an accent line under the focused box; disabled boxes
    are dimmed like any control.
  - **Keys**: Enter sends a box's `on_enter`; Escape empties a box made with `.clear_on_escape()`,
    else goes on to the window; Tab moves on and selects the next box's text.
  - **Ctrl+F** focuses the box `App::search_box` names (the sidebar's `SEARCH_KEY`), its text
    selected; a dialog whose first control is a text box opens with it focused and selected.
  - **The text box menu** (right-click, or the Menu key at the caret): Cut, Copy, Paste and
    Select all, a toolkit popup (`input::EditMenu`) like the drop-down's list.
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
- **Pictures**: `el::image` draws a decoded picture, uploaded once per `key` as a Direct2D
  bitmap and kept while frames draw it: one the latest frame didn't draw is dropped
  (`render::retain_drawn`), so a page left behind frees its pictures' GPU memory. Two seconds
  later (`TRIM_TIMER`) `Renderer::trim` has Direct2D release the textures it cached for drawing
  them (layers, pre-downscaled pictures), flushes Direct3D, and trims the driver's buffers
  (`IDXGIDevice3::Trim`): without it the GPU memory stayed held on an idle window.
  - **Fitted** (the default): icons are made at the exact pixel size they take for `Ctx::scale`
    (`image::decode_icons`), and a picture whose pixels match its place on screen is drawn on
    whole pixels, so nothing resamples it again; `App::scale_changed` makes them again. An icon
    image of exactly that size shows untouched.
  - **`.cover()`**: a cover is the store's original at full size (`image::decode_cover`). It
    fills its box, cut around its centre, and Direct2D scales it as it draws, with its
    high-quality cubic filter, inside a layer that rounds its corners (`.radius`). A new display
    scaling needs nothing made again.
  - **Only near the screen**: a keyed element made with `.watch()` (a Library tile) is reported
    to `App::near_screen` while it is on the screen or within one window height of it
    (`input::near_keys`); the shell lays out the app's answer before the frame is drawn.
    `covers.rs` keeps only each cover's file and decodes the covers of those tiles, dropping the
    others' pixels: off the Library none are held.
  - A tile's `hover_overlay` tints over its children (a picture hides a hover fill).
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
  expect it handled; `WM_NCCALCSIZE` and `WM_GETMINMAXINFO` need no state.
- **Bindings**: the `windows` crate at this commit uses C types: `HWND` and other handles are raw
  pointers (`is_null`), `WPARAM`/`LPARAM` are `usize`/`isize`, `WM_*`/`WS_*` constants are `i32`
  (the shell matches messages as `i32`), many methods return `HRESULT` (`.ok()?`), and `VARIANT` is
  the raw union (`uia.rs` builds them).
- **Title bar**: drawn by the view; elements keyed `shell::CAPTION_*` tell `WM_NCHITTEST` where to
  drag and where minimize, maximize (Snap Layouts) and close are.

## Checking it from an agent session
The `check-app` skill (`.claude/skills/check-app/`) runs and drives the window: UI Automation
(every control by name, with its patterns), posted typing, keys, right-clicks and drags, the
window's own drawing (`GAME_MANAGER_CAPTURE`, `WM_APP + 9`) and its real pixels. Its `SKILL.md`
lists what can't be checked here and the rules (My PC's registry, the clipboard, Uninstall…).

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
