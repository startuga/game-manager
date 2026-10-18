//! Finds games installed through the Xbox app (PC Game Pass and Microsoft Store games).
//!
//! These games are Windows packages. Windows lists the current user's packages through its
//! package API (`PackageManager`); a package is a game when its install folder has a
//! `MicrosoftGame.config`, the file Xbox PC games ship with. The Xbox app may install them on any
//! drive: the package folder under `WindowsApps` is a link to the real `XboxGames\...\Content`.
//!
//! Packaged games cannot be started by running their `.exe`; Windows starts them through their
//! package identity, `shell:AppsFolder\<PackageFamilyName>!<AppId>`, as the Start menu does.

use crate::text::{child_text, read_text, xml_body};
use crate::{Game, IconRef, Launch, LaunchOption, Store};
use std::fs;
use std::path::{Path, PathBuf};
use windows::Management::Deployment::PackageManager;
use windows::core::HSTRING;

/// What the scanner uses from `MicrosoftGame.config`.
#[derive(Debug, Default, PartialEq)]
struct GameConfig {
    store_id: String,
    /// `<Executable Id>` values, PC executables first.
    executable_ids: Vec<String>,
    default_display_name: String,
    override_display_name: String,
    /// Logo files from `<ShellVisuals>`, best for an icon first.
    logos: Vec<String>,
}

/// Scans the current user's packages for Xbox PC games.
pub fn scan() -> Result<Vec<Game>, String> {
    let manager = PackageManager::new().map_err(|e| format!("PackageManager: {e}"))?;
    // An empty security ID means the current user, which needs no admin rights.
    let packages = manager
        .FindPackagesByUserSecurityId(&HSTRING::new())
        .map_err(|e| format!("listing packages: {e}"))?;

    let mut games = Vec::new();
    for package in packages {
        if package.IsFramework().unwrap_or(true) {
            continue;
        }
        let Ok(path) = package.InstalledPath() else {
            continue;
        };
        let dir = PathBuf::from(path.to_os_string());
        let Some(config) =
            read_text(&dir.join("MicrosoftGame.config")).and_then(|text| parse_game_config(&text))
        else {
            continue;
        };
        let Some(family) = package
            .Id()
            .and_then(|id| id.FamilyName())
            .ok()
            .map(|name| name.to_string_lossy())
        else {
            continue;
        };
        let app_ids = fs::read_to_string(dir.join("AppxManifest.xml"))
            .map(|text| parse_manifest_app_ids(&text))
            .unwrap_or_default();
        let display_name = package
            .DisplayName()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();

        if let Some(mut game) = build_game(&family, &display_name, &dir, &config, &app_ids) {
            game.icon = resolve_logo(&dir, &config.logos);
            games.push(game);
        }
    }
    Ok(games)
}

fn parse_game_config(text: &str) -> Option<GameConfig> {
    let doc = roxmltree::Document::parse(xml_body(text)).ok()?;
    let root = doc.root_element();
    if root.tag_name().name() != "Game" {
        return None;
    }
    let element = |name: &str| {
        doc.descendants()
            .find(|n| n.is_element() && n.tag_name().name() == name)
    };
    let attr = |node: Option<roxmltree::Node>, name: &str| {
        node.and_then(|n| n.attribute(name))
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };

    let mut executables: Vec<roxmltree::Node> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Executable")
        .collect();
    // Prefer executables meant for PC over other device families.
    executables.sort_by_key(|n| {
        n.attribute("TargetDeviceFamily")
            .is_some_and(|f| !f.eq_ignore_ascii_case("PC"))
    });

    let visuals = element("ShellVisuals");
    Some(GameConfig {
        store_id: child_text(root, "StoreId").unwrap_or_default().to_owned(),
        executable_ids: executables
            .iter()
            .filter_map(|n| n.attribute("Id"))
            .map(str::to_owned)
            .collect(),
        default_display_name: attr(visuals, "DefaultDisplayName"),
        override_display_name: attr(executables.first().copied(), "OverrideDisplayName"),
        logos: ["Square150x150Logo", "Square44x44Logo", "StoreLogo"]
            .into_iter()
            .map(|name| attr(visuals, name))
            .filter(|logo| !logo.is_empty())
            .collect(),
    })
}

/// `Id`s of the `<Application>` entries in `AppxManifest.xml`.
fn parse_manifest_app_ids(text: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml_body(text)) else {
        return Vec::new();
    };
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Application")
        .filter(|n| n.parent().is_some_and(|p| p.has_tag_name("Applications")))
        .filter_map(|n| n.attribute("Id"))
        .map(str::to_owned)
        .collect()
}

/// The package app to start: the one the game config names, else the manifest's first.
fn choose_app_id(config_ids: &[String], manifest_ids: &[String]) -> Option<String> {
    config_ids
        .iter()
        .find(|id| manifest_ids.iter().any(|m| m.eq_ignore_ascii_case(id)))
        .or_else(|| manifest_ids.first())
        .or_else(|| config_ids.first())
        .cloned()
}

fn build_game(
    family: &str,
    display_name: &str,
    dir: &Path,
    config: &GameConfig,
    manifest_ids: &[String],
) -> Option<Game> {
    let app_id = choose_app_id(&config.executable_ids, manifest_ids)?;

    // Names starting with "ms-resource:" are unresolved resource references.
    let name = [
        display_name,
        &config.default_display_name,
        &config.override_display_name,
    ]
    .into_iter()
    .map(str::trim)
    .find(|name| !name.is_empty() && !name.starts_with("ms-resource:"))
    .unwrap_or(family)
    .to_owned();

    let alt_launches = if config.store_id.is_empty() {
        Vec::new()
    } else {
        vec![LaunchOption {
            label: "Open in Xbox app".to_owned(),
            launch: Launch::Uri(format!("msxbox://game/?productId={}", config.store_id)),
        }]
    };

    Some(Game {
        id: format!("xbox:{family}"),
        name,
        store: Store::Xbox,
        install_dir: Some(dir.to_path_buf()),
        launch: Launch::Uri(format!(r"shell:AppsFolder\{family}!{app_id}")),
        alt_launches,
        icon: None,
    })
}

/// The first logo that exists, either as named or as its largest size variant.
fn resolve_logo(dir: &Path, logos: &[String]) -> Option<IconRef> {
    logos
        .iter()
        .find_map(|logo| resolve_resource_file(dir, logo))
        .map(IconRef::file)
}

/// Package images are often stored only as variants such as `logo.scale-200.png` or
/// `logo.targetsize-256_altform-unplated.png`; this picks the largest one.
fn resolve_resource_file(dir: &Path, relative: &str) -> Option<PathBuf> {
    let path = dir.join(relative.replace('/', "\\").trim_start_matches('\\'));
    if path.is_file() {
        return Some(path);
    }
    let parent = path.parent()?;
    let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();

    fs::read_dir(parent)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_ascii_lowercase();
            let qualifiers = name.strip_prefix(&format!("{stem}."))?;
            let qualifiers = qualifiers.strip_suffix(&format!(".{ext}"))?;
            // High-contrast variants are drawn for a different background.
            if qualifiers.contains("contrast-") {
                return None;
            }
            Some((qualifier_size(qualifiers), entry.path()))
        })
        .max_by_key(|(size, _)| *size)
        .map(|(_, path)| path)
}

/// The number in `scale-N` or `targetsize-N`, used to rank variants (0 when absent).
fn qualifier_size(qualifiers: &str) -> u32 {
    qualifiers
        .split('_')
        .find_map(|q| {
            q.strip_prefix("targetsize-")
                .or_else(|| q.strip_prefix("scale-"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real MicrosoftGame.config of Wolfenstein: Enemy Territory (trimmed).
    const WOLFENSTEIN_CONFIG: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<Game configVersion="1">
    <Identity Name="BethesdaSoftworks.WolfensteinEnemyTerritory" Publisher="CN=21E520D9-F467-4438-A16E-79ADDBE4ECB1" Version="1.5.0.0"/>
    <StoreId>9N3PVJ211XZ2</StoreId>
    <ExecutableList>
        <Executable Name="Launcher.exe"
            	    Id="Game"
           		    TargetDeviceFamily="PC"
           		    OverrideDisplayName="Wolfenstein: Enemy Territory"/>
    </ExecutableList>
    <ShellVisuals DefaultDisplayName="Wolfenstein: Enemy Territory"
                  PublisherDisplayName="Bethesda Softworks"
                  StoreLogo="WolfET_storelogo.png"
                  Square150x150Logo="WolfET_logo.png"
                  Square44x44Logo="WolfET_smalllogo.png"
                  BackgroundColor="#000040"/>
</Game>"##;

    // Real AppxManifest.xml of the same package (trimmed).
    const WOLFENSTEIN_MANIFEST: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<Package xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10" xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
  <Identity Name="BethesdaSoftworks.WolfensteinEnemyTerritory" Publisher="CN=21E520D9-F467-4438-A16E-79ADDBE4ECB1" Version="1.5.0.0" ProcessorArchitecture="x86" />
  <Properties>
    <DisplayName>Wolfenstein: Enemy Territory</DisplayName>
    <Logo>WolfET_storelogo.png</Logo>
  </Properties>
  <Applications>
    <Application Id="Game" Executable="GameLaunchHelper.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="Wolfenstein: Enemy Territory" Square150x150Logo="WolfET_logo.png" Square44x44Logo="WolfET_smalllogo.png" />
    </Application>
  </Applications>
</Package>"##;

    const FAMILY: &str = "BethesdaSoftworks.WolfensteinEnemyTerritory_3275kfvn8vcwc";

    fn dir() -> PathBuf {
        PathBuf::from(
            r"C:\Program Files\WindowsApps\BethesdaSoftworks.WolfensteinEnemyTerritory_1.5.0.0_x86__3275kfvn8vcwc",
        )
    }

    #[test]
    fn parses_game_config() {
        let config = parse_game_config(WOLFENSTEIN_CONFIG).unwrap();
        assert_eq!(config.store_id, "9N3PVJ211XZ2");
        assert_eq!(config.executable_ids, ["Game"]);
        assert_eq!(config.default_display_name, "Wolfenstein: Enemy Territory");
        assert_eq!(config.override_display_name, "Wolfenstein: Enemy Territory");
        assert_eq!(
            config.logos,
            [
                "WolfET_logo.png",
                "WolfET_smalllogo.png",
                "WolfET_storelogo.png"
            ]
        );
    }

    #[test]
    fn rejects_other_xml_and_broken_files() {
        assert!(parse_game_config("<Package/>").is_none());
        assert!(parse_game_config("<Game><StoreId>").is_none());
        assert!(parse_manifest_app_ids("not xml").is_empty());
    }

    #[test]
    fn builds_launch_and_xbox_app_link() {
        let config = parse_game_config(WOLFENSTEIN_CONFIG).unwrap();
        let app_ids = parse_manifest_app_ids(WOLFENSTEIN_MANIFEST);
        assert_eq!(app_ids, ["Game"]);

        let game = build_game(
            FAMILY,
            "Wolfenstein: Enemy Territory",
            &dir(),
            &config,
            &app_ids,
        )
        .unwrap();
        assert_eq!(game.id, format!("xbox:{FAMILY}"));
        assert_eq!(game.name, "Wolfenstein: Enemy Territory");
        assert_eq!(game.store, Store::Xbox);
        assert_eq!(
            game.launch,
            Launch::Uri(format!(r"shell:AppsFolder\{FAMILY}!Game"))
        );
        assert_eq!(
            game.alt_launches,
            [LaunchOption {
                label: "Open in Xbox app".into(),
                launch: Launch::Uri("msxbox://game/?productId=9N3PVJ211XZ2".into()),
            }]
        );
    }

    #[test]
    fn no_store_id_means_no_xbox_app_link() {
        let mut config = parse_game_config(WOLFENSTEIN_CONFIG).unwrap();
        config.store_id.clear();
        let game = build_game(FAMILY, "", &dir(), &config, &["Game".into()]).unwrap();
        assert!(game.alt_launches.is_empty());
    }

    #[test]
    fn chooses_the_app_named_by_the_config() {
        let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            choose_app_id(&ids(&["Game"]), &ids(&["Editor", "game"])),
            Some("Game".into())
        );
        // No match: the manifest's first app, then the config's.
        assert_eq!(
            choose_app_id(&ids(&["Game"]), &ids(&["App"])),
            Some("App".into())
        );
        assert_eq!(choose_app_id(&ids(&["Game"]), &[]), Some("Game".into()));
        assert_eq!(choose_app_id(&[], &[]), None);
    }

    #[test]
    fn name_skips_unresolved_resources() {
        let mut config = parse_game_config(WOLFENSTEIN_CONFIG).unwrap();
        config.default_display_name = "ms-resource:GameName".into();
        config.override_display_name.clear();
        let game = build_game(FAMILY, "", &dir(), &config, &["Game".into()]).unwrap();
        assert_eq!(game.name, FAMILY);

        let game = build_game(
            FAMILY,
            "ms-resource:Title",
            &dir(),
            &config,
            &["Game".into()],
        )
        .unwrap();
        assert_eq!(game.name, FAMILY);
    }

    #[test]
    fn resolves_logo_files_and_size_variants() {
        let dir = std::env::temp_dir().join(format!("gm-core-xbox-test-{}", std::process::id()));
        let assets = dir.join("Assets");
        fs::create_dir_all(&assets).unwrap();
        for name in [
            "Logo.scale-100.png",
            "Logo.scale-200.png",
            "Logo.scale-200_contrast-black.png",
            "Small.png",
        ] {
            fs::write(assets.join(name), b"png").unwrap();
        }

        // Exact file.
        assert_eq!(
            resolve_logo(&dir, &["Assets/Small.png".into()]),
            Some(IconRef::file(assets.join("Small.png")))
        );
        // Only variants exist: the largest, not the high-contrast one.
        assert_eq!(
            resolve_logo(&dir, &[r"Assets\Logo.png".into()]),
            Some(IconRef::file(assets.join("Logo.scale-200.png")))
        );
        // Missing first choice falls through to the next.
        assert_eq!(
            resolve_logo(&dir, &["Missing.png".into(), "Assets/Small.png".into()]),
            Some(IconRef::file(assets.join("Small.png")))
        );
        assert_eq!(resolve_logo(&dir, &["Missing.png".into()]), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ranks_size_qualifiers() {
        assert_eq!(qualifier_size("scale-200"), 200);
        assert_eq!(qualifier_size("targetsize-256_altform-unplated"), 256);
        assert_eq!(qualifier_size("altform-unplated"), 0);
    }
}
