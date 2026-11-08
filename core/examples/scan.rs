//! Prints every detected game: `cargo run -p gm-core --example scan [-- -v] [-- --icons <dir>]`
//! Each line shows the game's bitness ("?" when unknown). `-v` also prints how each game is
//! launched, where its icon comes from, which program the bitness was read from, the address of
//! its PCGamingWiki page, how Uninstall… would hand it over (read only) and, for 32-bit games,
//! their Large Address Aware status.
//! `--icons <dir>` saves each game's icon there as `<id>.png`.
//! `--graphics` prints each game's graphics APIs from PCGamingWiki and from its files, side by
//! side (no cache; the wiki is asked about once a second), and the API chosen in its settings.
//! `--nvidia` prints the driver's version and its own DLSS DLLs, then each game's NVIDIA driver
//! profile with its Vulkan/OpenGL Present Method and flags and the DLSS override settings it
//! holds (read only).
//! `--pc` prints My PC instead of the games: hardware, displays with their modes and colour
//! profiles, DDC/CI brightness and the Windows switches (read only).

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
    if args.iter().any(|arg| arg == "--pc") {
        print_pc();
        return;
    }

    let report = gm_core::scan_all();
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

/// My PC, as the page reads it.
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
