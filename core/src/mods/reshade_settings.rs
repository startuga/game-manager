//! The user's ReShade choices for every game (ReShade ▸ Global Shaders and Global Addons), in
//! `%APPDATA%\Game Manager\reshade.json`: the shader selection (`shader_packs::Selection`; the
//! packs' defaults until the user chooses), the add-ons switched on, and named profiles of
//! shader selections. Fields this version does not know are kept.

use super::shader_packs::{Catalog, Selection};
use crate::{Result, json_file};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SETTINGS_FILE: &str = "reshade.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalSettings {
    /// `None` until the user chooses; then `Selection::defaults`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shaders: Option<Selection>,
    /// The ids of the add-ons switched on.
    #[serde(default)]
    pub addons: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// The profile the selection came from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl GlobalSettings {
    /// The shader selection to deploy.
    pub fn selection(&self, catalog: &Catalog) -> Selection {
        self.shaders
            .clone()
            .unwrap_or_else(|| Selection::defaults(catalog))
    }
}

/// A named shader selection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub shaders: Selection,
}

fn file() -> Result<PathBuf> {
    json_file::user_file(SETTINGS_FILE)
}

/// The user's choices; the defaults when there is no file yet. Blocking.
pub fn load() -> Result<GlobalSettings> {
    json_file::load(&file()?)
}

/// Saves the user's choices. Blocking.
pub fn save(settings: &GlobalSettings) -> Result<()> {
    save_to(&file()?, settings)
}

fn save_to(file: &Path, settings: &GlobalSettings) -> Result<()> {
    json_file::save(file, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::shader_packs::Pick;
    use std::fs;

    #[test]
    fn keeps_unknown_fields_and_defaults_the_selection() {
        let dir = std::env::temp_dir().join(format!("GM-ReShadeSettings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(SETTINGS_FILE);
        fs::write(&file, r#"{ "addons": ["swapchain"], "theme": "dark" }"#).unwrap();
        let mut settings: GlobalSettings = json_file::load(&file).unwrap();
        assert_eq!(settings.addons, ["swapchain"]);
        assert_eq!(settings.shaders, None);

        let mut chosen = Selection::default();
        chosen.packs.insert("SweetFX".into(), Pick::default());
        settings.profiles.push(Profile {
            name: "Profile 1".into(),
            shaders: chosen.clone(),
        });
        settings.shaders = Some(chosen.clone());
        save_to(&file, &settings).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(json["theme"], "dark");
        assert!(json["shaders"]["packs"]["SweetFX"].is_object());
        let again: GlobalSettings = json_file::load(&file).unwrap();
        assert_eq!(again, settings);
        assert_eq!(again.selection(&Catalog::default()), chosen);
        fs::remove_dir_all(dir).unwrap();
    }
}
