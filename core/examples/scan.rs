//! Prints every detected game: `cargo run -p gm-core --example scan [-- -v] [-- --icons <dir>]`
//! Each line shows the game's bitness ("?" when unknown). `-v` also prints how each game is
//! launched, where its icon comes from, which program the bitness was read from and the address
//! of its PCGamingWiki page.
//! `--icons <dir>` saves each game's icon there as `<id>.png`.
//! `--graphics` prints each game's graphics APIs from PCGamingWiki and from its files, side by
//! side (no cache; the wiki is asked about once a second).

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

    if graphics {
        let labels = |apis: &[GraphicsApi]| {
            apis.iter()
                .map(|api| api.label())
                .collect::<Vec<_>>()
                .join(", ")
        };
        for game in &report.games {
            let table = gm_core::pcgamingwiki::api_table(game);
            let files = gm_core::graphics::from_files(game);
            let wiki = match &table {
                Ok(Some(table)) => format!(
                    "{} [{}{}]",
                    labels(&table.apis),
                    table.page,
                    if table.found_by_name { ", by name" } else { "" }
                ),
                Ok(None) => "-".to_owned(),
                Err(error) => format!("error: {error}"),
            };
            let shown = match table {
                Ok(Some(table)) => gm_core::graphics::choose(table, || files.clone()),
                _ => files.clone(),
            };
            let shown = shown.map_or_else(
                || "-".to_owned(),
                |info| format!("{} ({:?})", labels(&info.apis), info.source),
            );
            let files = files.map_or_else(|| "-".to_owned(), |info| labels(&info.apis));
            println!(
                "{}\n    wiki: {wiki}\n    files: {files}\n    shown: {shown}",
                game.name
            );
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
