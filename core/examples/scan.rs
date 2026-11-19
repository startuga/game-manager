//! Prints every detected game: `cargo run -p gm-core --example scan [-- -v] [-- --icons <dir>]`
//! Each line shows the game's bitness ("?" when unknown). `-v` also prints how each game is
//! launched, where its icon comes from, which program the bitness was read from, the address of
//! its PCGamingWiki and Nexus Mods pages (Nexus's list of games is downloaded when the cached one
//! is a week old), how Uninstall… would hand it over (read only) and, for 32-bit games, their
//! Large Address Aware status.
//! `--icons <dir>` saves each game's icon there as the app gets it: its best image as it is in the
//! game's files, `<id>.png` or a one-image `<id>.ico`, or the picture file (`.png`, `.jpg`).
//! `--graphics` prints each game's graphics APIs from PCGamingWiki and from its files, side by
//! side (no cache; the wiki is asked about once a second), and the API chosen in its settings.
//! `--nvidia` prints the driver's version and its own DLSS DLLs, then each game's NVIDIA driver
//! profile with its Vulkan/OpenGL Present Method and flags and the DLSS override settings it
//! holds (read only).
//! `--pc` prints My PC instead of the games: hardware, displays with their modes and colour
//! profiles, DDC/CI brightness and the Windows switches (read only).
//! `--covers` prints where each game's cover comes from, or would be looked for, and under it its
//! page art's background and logo (read only: nothing is downloaded or cached).
//! `--saves` prints each game's saves and config folders from PCGamingWiki, as the game page's
//! buttons open them (the wiki is asked about once a second for games not in its cache).
//! `--play` prints each game's last played day and play time as the app shows them (Steam's
//! history merged with Game Manager's), the sessions still open, and the games running now (read
//! only).

#![allow(
    clippy::expect_used,
    reason = "a developer tool: stopping with the reason is the right response to a failure"
)]

use gm_core::graphics::GraphicsApi;
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
    if args.iter().any(|arg| arg == "--pc") {
        print_pc();
        return;
    }

    let report = gm_core::scan_all();
    if args.iter().any(|arg| arg == "--play") {
        print_play(&report.games);
        return;
    }
    let mods = gm_core::mods::load().unwrap_or_default();
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
            println!("           nexus: {}", gm_core::nexus_mods::page_url(game));
            match gm_core::uninstall::route(game) {
                Some(route) => println!("           uninstall: {route:?}"),
                None => println!("           uninstall: none (added by hand)"),
            }
            if game.arch == Some(gm_core::Arch::X86) {
                match gm_core::mods::laa::status(game, &mods) {
                    Ok(laa) => println!(
                        "           laa: {:?}{}",
                        laa.status,
                        if laa.steam_drm {
                            ", Steam DRM wrapper"
                        } else {
                            ""
                        }
                    ),
                    Err(error) => println!("           laa: {error}"),
                }
            }
        }
    }
    println!("{} games", report.games.len());
    for error in &report.errors {
        eprintln!("error: {error}");
    }

    if args.iter().any(|arg| arg == "--covers") {
        for game in &report.games {
            println!("{:<50} {}", game.name, gm_core::covers::describe(game));
            println!("{:<50} {}", "", gm_core::page_art::describe(game));
        }
    }

    if args.iter().any(|arg| arg == "--saves") {
        for game in &report.games {
            match gm_core::game_data::folders(game) {
                Ok(folders) => println!(
                    "{}\n           saves:  {:?}\n           config: {:?}",
                    game.name, folders.saves, folders.config
                ),
                Err(error) => println!("{}: {error}", game.name),
            }
        }
    }

    if nvidia {
        use gm_core::nvidia::{SettingState, flags_label, method_label};
        let show = |setting: SettingState, label: &dyn Fn(u32) -> String| match setting {
            SettingState {
                user_value: Some(value),
                ..
            } => format!("{} (user)", label(value)),
            SettingState {
                value: Some(value), ..
            } => format!("{} (NVIDIA)", label(value)),
            _ => "global".to_owned(),
        };
        let overrides = gm_core::mods::dlss_override::catalog().expect("read the overrides");
        match gm_core::mods::dlss_override::driver(&overrides) {
            Ok(Some(driver)) => println!("driver {}, its DLLs {:?}", driver.label(), driver.dlls),
            Ok(None) => println!("no NVIDIA driver"),
            Err(error) => println!("driver: {error}"),
        }
        let ids: Vec<u32> = overrides.settings().map(|setting| setting.id).collect();
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
                Ok(Some(state)) => {
                    println!(
                        "{:<50} {:<28} {:<45} method: {}, flags: {}",
                        game.name,
                        exe,
                        state.profile.as_deref().unwrap_or("-"),
                        show(state.method, &method_label),
                        show(state.flags, &flags_label)
                    );
                    let Ok(Some(profile)) = gm_core::nvidia::read_settings(&exe, &ids) else {
                        continue;
                    };
                    for (id, setting) in &profile.settings {
                        if setting.value.is_none() {
                            continue;
                        }
                        let Some(known) = overrides.setting(*id) else {
                            continue;
                        };
                        println!(
                            "    {}: {}",
                            known.name,
                            show(*setting, &|value| known.label(value))
                        );
                    }
                }
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
        for game in &report.games {
            let bytes = game.icon.as_ref().and_then(gm_core::icon::read);
            match bytes {
                Some(bytes) => {
                    // The icon's best image as it is in the game's files, or the picture.
                    let (extension, image) = if gm_core::icon::is_ico(&bytes) {
                        let images = gm_core::icon::images(&bytes);
                        let image = images.first().map(|i| (i.width, i.height, i.bits));
                        ("ico", image.filter(|_| images.len() == 1))
                    } else if gm_core::icon::is_png(&bytes) {
                        ("png", gm_core::icon::describe(&bytes))
                    } else {
                        ("jpg", None)
                    };
                    let name = format!("{}.{extension}", game.id.replace(':', "_"));
                    std::fs::write(dir.join(name), &bytes).expect("write icon");
                    let size = image.map_or_else(
                        || "?".to_owned(),
                        |(width, height, bits)| format!("{width} x {height}, {bits} bits"),
                    );
                    println!(
                        "icon ok    {:>7} bytes  {extension}  {size:<20}  {}",
                        bytes.len(),
                        game.name
                    );
                }
                None => println!("icon MISSING           {}", game.name),
            }
        }
    }
}

/// My PC, as the page reads it.
/// `--play`: Steam's play history and Game Manager's own, merged as the app shows them, the
/// sessions still open, and the watched games running now (read only).
fn print_play(games: &[gm_core::Game]) {
    let name = |id: &str| {
        games
            .iter()
            .find(|game| game.id == id)
            .map_or_else(|| id.to_owned(), |game| game.name.clone())
    };
    let day = |unix: u64| {
        gm_core::local_time::local(unix).map_or_else(
            || unix.to_string(),
            |at| {
                format!(
                    "{}-{:02}-{:02} {:02}:{:02}",
                    at.year, at.month, at.day, at.hour, at.minute
                )
            },
        )
    };
    match gm_core::steam::play_times() {
        Ok(Some((file, apps))) => {
            println!("Steam: {} ({} apps played)", file.display(), apps.len())
        }
        Ok(None) => println!("Steam: no account's local config found"),
        Err(error) => println!("Steam: {error}"),
    }
    let steam = gm_core::play::steam_play().unwrap_or_default();
    let history = gm_core::play::load().unwrap_or_default();
    println!(
        "Game Manager's history: {} games, {} sessions open",
        history.games.len(),
        history.open.len()
    );
    let merged = gm_core::play::merged(&history.games, &steam);
    let mut played: Vec<_> = games
        .iter()
        .filter_map(|game| Some((game, merged.get(&game.id)?)))
        .collect();
    played.sort_by_key(|(_, play)| std::cmp::Reverse(play.last_played));
    for (game, play) in played {
        println!(
            "  {:<40} last played {:<16} {:>6} min",
            game.name,
            play.last_played.map_or_else(|| "never".to_owned(), day),
            play.seconds / 60
        );
    }
    for (id, session) in &history.open {
        println!(
            "  open: {} since {}, {} s counted",
            name(id),
            day(session.started),
            session.played
        );
    }
    for (id, restore) in &history.displays {
        println!(
            "  display settings to put back for {}: {} (HDR {:?}, mode {:?})",
            name(id),
            restore.name,
            restore.hdr_before,
            restore.mode.map(|change| change.before)
        );
    }
    let watch = gm_core::play::Watchlist::new(games, &history.open);
    match gm_core::play::running(&watch) {
        Ok(seen) if seen.is_empty() => println!("Running now: no game"),
        Ok(seen) => {
            for process in seen {
                println!(
                    "Running now: {} (process {}, {})",
                    name(&process.game),
                    process.pid,
                    process.program.display()
                );
            }
        }
        Err(error) => println!("Running now: {error}"),
    }
}

fn print_pc() {
    use gm_core::pc::{self, format_bytes};
    let started = std::time::Instant::now();
    let pc = pc::read();
    println!("machine: {} (read in {:?})", pc.machine, started.elapsed());
    match &pc.cpu {
        Ok(cpu) => println!(
            "cpu: {}, {} cores, {} threads, base {} MHz",
            cpu.name, cpu.cores, cpu.threads, cpu.base_mhz
        ),
        Err(error) => println!("cpu: {error}"),
    }
    match &pc.gpus {
        Ok(gpus) => {
            for gpu in gpus {
                println!(
                    "gpu: {} ({}), {} VRAM, driver {} of {}",
                    gpu.name,
                    gpu.vendor.name(),
                    format_bytes(gpu.vram),
                    gpu.driver,
                    gpu.driver_date
                );
            }
        }
        Err(error) => println!("gpus: {error}"),
    }
    match &pc.memory {
        Ok(memory) => {
            println!(
                "memory: {}, {} available",
                format_bytes(memory.total),
                format_bytes(memory.available)
            );
            for module in &memory.modules {
                println!(
                    "    {} {} {} MT/s {} {}",
                    format_bytes(module.size),
                    module.kind,
                    module.speed,
                    module.manufacturer,
                    module.part_number
                );
            }
        }
        Err(error) => println!("memory: {error}"),
    }
    match &pc.system {
        Ok(system) => println!(
            "windows: {} (build {})\nboard: {}\nbios: {}",
            system.windows, system.build, system.board, system.bios
        ),
        Err(error) => println!("system: {error}"),
    }
    match &pc.drives {
        Ok(drives) => {
            for drive in drives {
                println!(
                    "drive: {} {} {}: {} free of {}",
                    drive.root,
                    drive.label,
                    drive.file_system,
                    format_bytes(drive.free),
                    format_bytes(drive.total)
                );
            }
        }
        Err(error) => println!("drives: {error}"),
    }
    match &pc.displays {
        Ok(displays) if displays.is_empty() => {
            println!("displays: none active (a TV in standby or a display switched off)");
        }
        Ok(displays) => {
            for display in displays {
                println!(
                    "display: {} ({}) {} × {} at {} Hz, at {:?}{}{}, HDR {}",
                    display.name,
                    display.gdi_name,
                    display.width,
                    display.height,
                    display.refresh_hz,
                    display.position,
                    if display.primary { ", main" } else { "" },
                    if display.built_in { ", built in" } else { "" },
                    match (display.hdr_supported, display.hdr_on) {
                        (_, true) => "on",
                        (true, false) => "off",
                        (false, false) => "not supported",
                    }
                );
                println!("  id: {}", display.id);
                println!(
                    "    {} modes, current {:?}, first {:?}",
                    display.modes.len(),
                    display.current,
                    display.modes.first()
                );
                println!(
                    "    colour profiles {:?}, default {:?}",
                    display.color_profiles, display.color_profile
                );
                let started = std::time::Instant::now();
                println!(
                    "    brightness {:?} (read in {:?})",
                    pc::brightness(display),
                    started.elapsed()
                );
            }
        }
        Err(error) => println!("displays: {error}"),
    }
    println!("switches: {:?}", pc.switches);
}
