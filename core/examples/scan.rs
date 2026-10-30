//! Prints every detected game: `cargo run -p gm-core --example scan [-- -v] [-- --icons <dir>]`
//! Each line shows the game's bitness ("?" when unknown). `-v` also prints how each game is
//! launched, where its icon comes from, which program the bitness was read from and the address
//! of its PCGamingWiki page.
//! `--icons <dir>` saves each game's icon there as `<id>.png`.
//! `--graphics` prints each game's graphics APIs from PCGamingWiki and from its files, side by
//! side (no cache; the wiki is asked about once a second), and the API chosen in its settings.
//! `--nvidia` prints each game's NVIDIA driver profile and its Vulkan/OpenGL Present Method and
//! flags (read only).

#![allow(
    clippy::expect_used,
    reason = "a developer tool: stopping with the reason is the right response to a failure"
)]

use gm_core::graphics::GraphicsApi;
use gm_core::icon::IconExtractor;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let verbose = args.iter().any(|arg| arg == "-v");
    let graphics = args.iter().any(|arg| arg == "--graphics");
    let nvidia = args.iter().any(|arg| arg == "--nvidia");
    let icon_dir = args
        .iter()
        .position(|arg| arg == "--icons")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let report = gm_core::scan_all();
    for game in &report.games {
        let arch = game.arch.map_or("?", |arch| arch.label());
        println!(
            "{:<10} {:<50} {:<15} {}",
            game.store.name(),
            game.name,
            arch,
            game.id
        );
        if verbose {
            println!("           play: {:?}", game.launch);
            for alt in &game.alt_launches {
                println!("           {}: {:?}", alt.label, alt.launch);
            }
            println!("           icon: {:?}", game.icon);
            println!("           program: {:?}", game.program);
            println!("           wiki: {}", gm_core::pcgamingwiki::page_url(game));
        }
    }
    println!("{} games", report.games.len());
    for error in &report.errors {
        eprintln!("error: {error}");
    }

    if nvidia {
        use gm_core::nvidia::{SettingState, flags_label, method_label};
        let show = |setting: SettingState, label: fn(u32) -> String| match setting {
            SettingState {
                user_value: Some(value),
                ..
            } => format!("{} (user)", label(value)),
            SettingState {
                value: Some(value), ..
            } => format!("{} (NVIDIA)", label(value)),
            _ => "global".to_owned(),
        };
        for game in &report.games {
            let Some(exe) = game
                .program
                .as_deref()
                .and_then(|program| program.file_name())
            else {
                continue;
            };
            let exe = exe.to_string_lossy();
            match gm_core::nvidia::read(&exe) {
                Ok(Some(state)) => println!(
                    "{:<50} {:<28} {:<45} method: {}, flags: {}",
                    game.name,
                    exe,
                    state.profile.as_deref().unwrap_or("-"),
                    show(state.method, method_label),
                    show(state.flags, flags_label)
                ),
                Ok(None) => {
                    println!("no NVIDIA driver");
                    break;
                }
                Err(error) => println!("{:<50} {exe}: {error}", game.name),
            }
        }
    }

    if graphics {
        let labels = |apis: &[GraphicsApi]| {
            apis.iter()
                .map(|api| api.label())
                .collect::<Vec<_>>()
                .join(", ")
        };
        for game in &report.games {
            let wiki = match gm_core::pcgamingwiki::api_table(game) {
                Ok(Some(info)) => format!("{} ({:?})", labels(&info.apis), info.source),
                Ok(None) => "-".to_owned(),
                Err(error) => format!("error: {error}"),
            };
            let files = gm_core::graphics::from_files(game)
                .map_or_else(|| "-".to_owned(), |info| labels(&info.apis));
            println!("{}\n    wiki: {wiki}\n    files: {files}", game.name);
            if let Some(api) = game.edits.as_ref().and_then(|edits| edits.graphics_api) {
                println!("    chosen: {}", api.label());
            }
        }
    }

    if let Some(dir) = icon_dir {
        std::fs::create_dir_all(&dir).expect("create icon folder");
        let extractor = IconExtractor::new().expect("start WIC");
        for game in &report.games {
            let png = game.icon.as_ref().and_then(|icon| extractor.png(icon, 64));
            match png {
                Some(png) => {
                    let file = dir.join(format!("{}.png", game.id.replace(':', "_")));
                    std::fs::write(&file, &png).expect("write icon");
                    println!("icon ok    {:>6} bytes  {}", png.len(), game.name);
                }
                None => println!("icon MISSING           {}", game.name),
            }
        }
    }
}
