# Game Manager

Native Windows 11 game launcher/library written in Rust.

## Layout
- `app/` – WinUI 3 UI built with `windows-reactor` (preview, 0.100). Component/message/view model, Elm style.
- `core/` (crate `gm-core`) – game discovery, launching, data model. **No UI code here**, so the UI layer can be swapped.
  - `vdf.rs` – Valve KeyValues parser; `steam.rs` – Steam scanner; `epic.rs` – Epic scanner (current launcher: `%LOCALAPPDATA%\com.epicgames.eglv2\installations.jsonl` log + `cache\prod\catalog-cache.json` for titles; legacy: `%ProgramData%` `*.item` manifests); `launcher.rs` – `ShellExecuteW`.
  - New stores: add a module with `pub fn scan() -> Result<Vec<Game>, String>`, a `Store` variant, and an entry in `SCANNERS` in `lib.rs`.
  - `examples/scan.rs` – prints detected games (headless check of the scanners).

## Toolchain
- Rust pinned to 1.99.1 in `rust-toolchain.toml`, edition 2024, MSVC target.
- WinUI apps are framework-dependent: they need the Windows App Runtime 2.x installed.

## Commands
- Build: `cargo build`
- Run UI: `cargo run -p app`
- Scanner check: `cargo run -p gm-core --example scan`
- Before finishing a change: `cargo fmt`, `cargo clippy --all-targets` (no warnings), `cargo test`.

## Conventions
- Blocking work (disk scans, registry, network) runs via `ComponentContext::spawn_background` and returns a `Message`; never on the UI thread.
- A store that fails to scan adds to `ScanReport::errors`; it must not hide other stores' games.
- Every `unsafe` block gets a `// SAFETY:` comment.
- Game ids are `<store>:<store id>` and must be stable (used as list keys and, later, for saved data).
- `windows-reactor` is a preview: when unsure of an API, read its source in `~/.cargo/registry/src/*/windows-reactor-0.100.0/src` rather than guessing.
