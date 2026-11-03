//! The user's ReShade choices (ReShade ▸ Shaders and Addons), in
//! `%APPDATA%\Game Manager\reshade.json`:
//!
//! - **Shared**: the shader selection (`shader_packs::Selection`; the packs' defaults until the
//!   user chooses) and the add-ons switched on, for every game with ReShade.
//! - **A game's own** (`games`, by game id): its own shaders, add-ons or both, chosen with the
//!   panels' "Only <game>". A part it has no choice of follows the shared one, and deploying the
//!   shared one leaves a game's own part alone. Uninstalling ReShade keeps it, for a reinstall.
//! - **Profiles**: named shader selections, loaded into either.
//!
//! Fields this version does not know are kept.

use super::shader_packs::{Catalog, Selection};
use crate::{Result, json_file};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SETTINGS_FILE: &str = "reshade.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReshadeSettings {
    /// `None` until the user chooses; then `Selection::defaults`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shaders: Option<Selection>,
    /// The ids of the add-ons switched on.
    #[serde(default)]
    pub addons: Vec<String>,
    /// Games with a choice of their own, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub games: BTreeMap<String, GameChoice>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// The profile the selection came from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

/// A game's own choice; a part left `None` follows the shared one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shaders: Option<Selection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addons: Option<Vec<String>>,
}

/// The two parts of the choice, each shared or a game's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Shaders,
    Addons,
}

impl ReshadeSettings {
    /// The shared shader selection.
    pub fn selection(&self, catalog: &Catalog) -> Selection {
        self.shaders
            .clone()
            .unwrap_or_else(|| Selection::defaults(catalog))
    }

    /// The shaders the game gets: its own, else the shared ones.
    pub fn shaders_for(&self, id: &str, catalog: &Catalog) -> Selection {
        self.own_shaders(id)
            .cloned()
            .unwrap_or_else(|| self.selection(catalog))
    }

    /// The add-ons the game gets: its own, else the shared ones.
    pub fn addons_for(&self, id: &str) -> &[String] {
        self.own_addons(id).unwrap_or(&self.addons)
    }

    pub fn own_shaders(&self, id: &str) -> Option<&Selection> {
        self.games.get(id)?.shaders.as_ref()
    }

    pub fn own_addons(&self, id: &str) -> Option<&[String]> {
        self.games.get(id)?.addons.as_deref()
    }

    /// Whether the game has its own choice of `part`.
    pub fn has_own(&self, id: &str, part: Part) -> bool {
        match part {
            Part::Shaders => self.own_shaders(id).is_some(),
            Part::Addons => self.own_addons(id).is_some(),
        }
    }

    /// Gives the game its own shaders, or (`None`) makes it follow the shared ones.
    pub fn set_own_shaders(&mut self, id: &str, shaders: Option<Selection>) {
        self.change_own(id, |choice| choice.shaders = shaders);
    }

    /// Gives the game its own add-ons, or (`None`) makes it follow the shared ones.
    pub fn set_own_addons(&mut self, id: &str, addons: Option<Vec<String>>) {
        self.change_own(id, |choice| choice.addons = addons);
    }

    /// Changes a game's own choice; one left with no part is removed.
    fn change_own(&mut self, id: &str, change: impl FnOnce(&mut GameChoice)) {
        let choice = self.games.entry(id.to_owned()).or_default();
        change(choice);
        if *choice == GameChoice::default() {
            self.games.remove(id);
        }
    }

    /// Adds a profile of `shaders` named "Profile <n>", the first number free, and makes it the
    /// current one; returns its name.
    pub fn new_profile(&mut self, shaders: Selection) -> String {
        let name = (1..)
            .map(|n| format!("Profile {n}"))
            .find(|name| self.profile_index(name).is_none())
            .unwrap_or_default();
        self.profiles.push(Profile {
            name: name.clone(),
            shaders,
        });
        self.profile = Some(name.clone());
        name
    }

    /// Saves `shaders` into the current profile, or into a new one when there is none.
    pub fn save_profile(&mut self, shaders: Selection) {
        match self
            .profile
            .as_deref()
            .and_then(|name| self.profile_index(name))
        {
            Some(index) => self.profiles[index].shaders = shaders,
            None => {
                self.new_profile(shaders);
            }
        }
    }

    /// Renames a profile; a blank name or one another profile has is refused (false).
    pub fn rename_profile(&mut self, old: &str, new: &str) -> bool {
        let new = new.trim();
        let taken = self
            .profile_index(new)
            .is_some_and(|index| self.profiles[index].name != old);
        let Some(index) = self
            .profile_index(old)
            .filter(|_| !new.is_empty() && !taken)
        else {
            return false;
        };
        self.profiles[index].name = new.to_owned();
        if self.profile.as_deref() == Some(old) {
            self.profile = Some(new.to_owned());
        }
        true
    }

    pub fn delete_profile(&mut self, name: &str) {
        self.profiles.retain(|profile| profile.name != name);
        if self.profile.as_deref() == Some(name) {
            self.profile = None;
        }
    }

    fn profile_index(&self, name: &str) -> Option<usize> {
        self.profiles
            .iter()
            .position(|profile| profile.name.eq_ignore_ascii_case(name))
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
pub fn load() -> Result<ReshadeSettings> {
    json_file::load(&file()?)
}

/// Saves the user's choices. Blocking.
pub fn save(settings: &ReshadeSettings) -> Result<()> {
    save_to(&file()?, settings)
}

fn save_to(file: &Path, settings: &ReshadeSettings) -> Result<()> {
    json_file::save(file, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::shader_packs::Pick;
    use std::fs;

    #[test]
    fn profiles_are_made_saved_renamed_and_deleted() {
        let mut settings = ReshadeSettings::default();
        let mut sweet = Selection::default();
        sweet.packs.insert("SweetFX".into(), Pick::default());
        // Saving with no current profile makes one.
        settings.save_profile(sweet.clone());
        assert_eq!(settings.profile.as_deref(), Some("Profile 1"));
        assert_eq!(settings.new_profile(Selection::default()), "Profile 2");
        settings.save_profile(sweet.clone());
        assert_eq!(settings.profiles[1].shaders, sweet);
        // A blank name, or another profile's, is refused.
        assert!(!settings.rename_profile("Profile 2", "  "));
        assert!(!settings.rename_profile("Profile 2", "profile 1"));
        assert!(settings.rename_profile("Profile 2", " HDR "));
        assert_eq!(settings.profile.as_deref(), Some("HDR"));
        settings.delete_profile("HDR");
        assert_eq!(settings.profile, None);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.new_profile(Selection::default()), "Profile 2");
    }

    #[test]
    fn a_game_has_its_own_choice_or_follows_the_shared_one() {
        let mut settings = ReshadeSettings {
            addons: vec!["swapchain".into()],
            ..ReshadeSettings::default()
        };
        let catalog = Catalog::default();
        let mut sweet = Selection::default();
        sweet.packs.insert("SweetFX".into(), Pick::default());
        settings.shaders = Some(Selection::default());

        settings.set_own_shaders("steam:1", Some(sweet.clone()));
        assert_eq!(settings.shaders_for("steam:1", &catalog), sweet);
        assert_eq!(
            settings.shaders_for("steam:2", &catalog),
            Selection::default()
        );
        // Its add-ons still follow the shared ones.
        assert_eq!(settings.addons_for("steam:1"), ["swapchain"]);
        assert!(settings.has_own("steam:1", Part::Shaders));
        assert!(!settings.has_own("steam:1", Part::Addons));

        // No add-ons is a choice of its own too.
        settings.set_own_addons("steam:1", Some(Vec::new()));
        assert!(settings.addons_for("steam:1").is_empty());
        settings.set_own_shaders("steam:1", None);
        assert_eq!(
            settings.shaders_for("steam:1", &catalog),
            Selection::default()
        );
        assert!(settings.games.contains_key("steam:1"));
        // A game left with no choice of its own is dropped.
        settings.set_own_addons("steam:1", None);
        assert!(settings.games.is_empty());
    }

    #[test]
    fn keeps_unknown_fields_and_defaults_the_selection() {
        let dir = std::env::temp_dir().join(format!("GM-ReShadeSettings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(SETTINGS_FILE);
        fs::write(&file, r#"{ "addons": ["swapchain"], "theme": "dark" }"#).unwrap();
        let mut settings: ReshadeSettings = json_file::load(&file).unwrap();
        assert_eq!(settings.addons, ["swapchain"]);
        assert_eq!(settings.shaders, None);

        let mut chosen = Selection::default();
        chosen.packs.insert("SweetFX".into(), Pick::default());
        settings.profiles.push(Profile {
            name: "Profile 1".into(),
            shaders: chosen.clone(),
        });
        settings.shaders = Some(chosen.clone());
        // An older file has no games of their own, and none are written.
        assert!(settings.games.is_empty());
        save_to(&file, &settings).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(json["theme"], "dark");
        assert!(json["shaders"]["packs"]["SweetFX"].is_object());
        assert!(json.get("games").is_none());
        settings.set_own_addons("steam:1", Some(vec!["swapchain".into()]));
        save_to(&file, &settings).unwrap();
        let again: ReshadeSettings = json_file::load(&file).unwrap();
        assert_eq!(again, settings);
        assert_eq!(again.selection(&Catalog::default()), chosen);
        fs::remove_dir_all(dir).unwrap();
    }
}
