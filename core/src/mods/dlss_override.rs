//! NVIDIA's driver-level DLSS overrides, per game: settings in the driver profile that holds the
//! game's `.exe` (`nvidia`) make the driver load its own latest DLSS, Ray Reconstruction, Frame
//! Generation and Streamline DLLs, force a model preset, a resolution mode or scaling ratio, a
//! frame-generation count, or Smooth Motion. They are what the NVIDIA app's "DLSS Override" and
//! nvidiaProfileInspector write, and no file of the game changes, so anti-cheat has nothing to
//! object to.
//!
//! - **Settings** come from the shipped `nvidia-overrides.json` (copied from `config/`), after
//!   nvidiaProfileInspector's `CustomSettingNames.xml`: groups of DWORD settings with their values'
//!   names and the driver each needs. NVIDIA adds presets with drivers, so they are data.
//! - **The driver's DLLs**: an "On" item of a DLL override shows the version of the driver's own
//!   copy, from its NGX models folder (`nvidia::ngx_version`).
//! - **Originals**: `mods.json` keeps the `.exe` the overrides were applied to, the choice and what
//!   the profile held before (`nvidia::Originals`), so Reset puts it back and deletes a profile
//!   Game Manager created. The DXVK settings' Present Method may share that profile: a profile
//!   either record says Game Manager created counts as created for both, so whichever goes back
//!   last deletes it.

use super::{ModsFile, dxvk, update};
use crate::nvidia::{self, Originals, Profile};
use crate::{Error, Game, Result, config};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const CATALOG_FILE: &str = "nvidia-overrides.json";

pub const INFO: &str = "NVIDIA's driver can override a game's DLSS through the game's driver \
profile, as the NVIDIA app's DLSS Override does: load the driver's own latest DLLs, force a model \
preset, a resolution mode or a frame generation count.

No file of the game changes, so an online game's anti-cheat has nothing to object to. Reset to \
original puts back what the profile held.

⚠ The game must use DLSS (Smooth Motion aside); the settings do nothing otherwise.
⚠ Some need a recent driver; those the driver is too old for are not shown.
⚠ A DLL override replaces a swapped DLL too, and stays when the game's own DLLs are restored.";

/// The shipped settings, in groups.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub groups: Vec<Group>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Group {
    pub name: String,
    pub settings: Vec<Setting>,
}

/// A DWORD setting of the driver profile.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Setting {
    #[serde(deserialize_with = "number")]
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub hint: String,
    /// The first driver that knows it: "572.83".
    #[serde(default)]
    min_driver: Option<String>,
    /// The NGX model whose driver copy an "On" value loads (`dlss`).
    #[serde(default)]
    pub ngx: Option<String>,
    #[serde(default)]
    values: Vec<Value>,
    #[serde(default)]
    range: Option<Range>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct Value {
    #[serde(deserialize_with = "number")]
    value: u32,
    name: String,
}

/// Values `from` to `to`, shown with `unit`, some with a name ("58% (Balanced)").
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct Range {
    from: u32,
    to: u32,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    named: BTreeMap<u32, String>,
}

/// A number written as one (`3`) or as hex text (`"0x10E41E01"`).
fn number<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Plain(u32),
        Text(String),
    }
    match Number::deserialize(deserializer)? {
        Number::Plain(value) => Ok(value),
        Number::Text(text) => text
            .strip_prefix("0x")
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .ok_or_else(|| serde::de::Error::custom(format!("not a hex number: {text}"))),
    }
}

/// "572.83" as the driver counts it: 57283.
fn driver_number(text: &str) -> Option<u32> {
    let (major, minor) = text.split_once('.')?;
    Some(major.parse::<u32>().ok()? * 100 + minor.parse::<u32>().ok()?)
}

impl Setting {
    /// Its values with their names, in the order shown. "On" values of a DLL override name the
    /// driver's copy (`driver_dll`: "310.9.16").
    pub fn values(&self, driver_dll: Option<&str>) -> Vec<(u32, String)> {
        let mut values: Vec<(u32, String)> = self
            .values
            .iter()
            .map(|value| match (value.value, driver_dll) {
                (1, Some(version)) if self.ngx.is_some() => {
                    (value.value, format!("{} ({version})", value.name))
                }
                _ => (value.value, value.name.clone()),
            })
            .collect();
        if let Some(range) = &self.range {
            values.extend((range.from..=range.to).map(|value| {
                let text = format!("{value}{}", range.unit);
                match range.named.get(&value) {
                    Some(name) => (value, format!("{text} ({name})")),
                    None => (value, text),
                }
            }));
        }
        values
    }

    /// A value's name; one the catalog doesn't know as hex.
    pub fn label(&self, value: u32) -> String {
        self.values(None)
            .into_iter()
            .find(|(known, _)| *known == value)
            .map_or_else(|| format!("0x{value:08X}"), |(_, name)| name)
    }

    /// Whether the driver (`62112`) knows this setting; an unknown driver is given the benefit.
    pub fn available(&self, driver: Option<u32>) -> bool {
        match (self.min_driver.as_deref().and_then(driver_number), driver) {
            (Some(needed), Some(driver)) => driver >= needed,
            _ => true,
        }
    }
}

impl Catalog {
    pub fn settings(&self) -> impl Iterator<Item = &Setting> {
        self.groups.iter().flat_map(|group| &group.settings)
    }

    pub fn setting(&self, id: u32) -> Option<&Setting> {
        self.settings().find(|setting| setting.id == id)
    }

    fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.settings().map(|setting| setting.id)
    }
}

/// The shipped settings. Blocking: reads a file.
pub fn catalog() -> Result<Catalog> {
    let file = config::shipped_file(Path::new(CATALOG_FILE))
        .ok_or_else(|| Error::ToolMissing(PathBuf::from(CATALOG_FILE)))?;
    let text = fs::read_to_string(&file).map_err(Error::io(&file))?;
    serde_json::from_str(&text).map_err(Error::json(CATALOG_FILE))
}

/// What the NVIDIA driver offers: its version and its own DLLs' versions, by NGX model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Driver {
    /// 62112 for 621.12.
    pub version: u32,
    pub dlls: BTreeMap<String, String>,
}

impl Driver {
    pub fn label(&self) -> String {
        nvidia::driver_label(self.version)
    }

    /// The driver's copy of the setting's DLL, for its "On" item.
    pub fn dll(&self, setting: &Setting) -> Option<&str> {
        self.dlls.get(setting.ngx.as_deref()?).map(String::as_str)
    }
}

/// The driver's facts; `None` without an NVIDIA graphics driver. Blocking: loads NVAPI and lists
/// the NGX models folder.
pub fn driver(catalog: &Catalog) -> Result<Option<Driver>> {
    let Some(version) = nvidia::driver_version()? else {
        return Ok(None);
    };
    let dlls = catalog
        .settings()
        .filter_map(|setting| setting.ngx.as_deref())
        .filter_map(|model| Some((model.to_owned(), nvidia::ngx_version(model)?)))
        .collect();
    Ok(Some(Driver { version, dlls }))
}

/// A game's overrides in `mods.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DlssOverride {
    /// The program's file name the profile was found by, so going back reaches the same profile
    /// after the game's program changed.
    pub exe: String,
    /// The values chosen, by setting id.
    #[serde(default)]
    pub choice: BTreeMap<u32, u32>,
    pub original: Originals,
}

/// The game's profile as the dialog shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverrideState {
    pub exe: String,
    /// The settings now, every catalog setting's.
    pub profile: Profile,
    pub record: Option<DlssOverride>,
}

impl OverrideState {
    /// The recorded original user value of a setting Game Manager changed: `Some(None)` when it
    /// had none.
    pub fn original(&self, id: u32) -> Option<Option<u32>> {
        self.record.as_ref()?.original.values.get(&id).copied()
    }

    pub fn choice(&self) -> BTreeMap<u32, u32> {
        self.record
            .as_ref()
            .map(|record| record.choice.clone())
            .unwrap_or_default()
    }
}

/// The `.exe` whose profile the overrides go into: the one recorded, else the game's program's.
fn exe(game: &Game, record: Option<&DlssOverride>) -> Result<String> {
    if let Some(record) = record {
        return Ok(record.exe.clone());
    }
    game.program
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| Error::NotInstallable(dxvk::Blocked::NoProgram.to_string()))
}

/// The settings to read: the catalog's, and any recorded ones it no longer lists.
fn ids(catalog: &Catalog, record: Option<&DlssOverride>) -> Vec<u32> {
    let mut ids: BTreeSet<u32> = catalog.ids().collect();
    if let Some(record) = record {
        ids.extend(record.choice.keys());
        ids.extend(record.original.values.keys());
    }
    ids.into_iter().collect()
}

fn no_driver() -> Error {
    Error::Nvapi {
        action: "find the NVIDIA driver",
        status: nvidia::NVIDIA_DEVICE_NOT_FOUND,
    }
}

/// The game's profile and record; `None` without an NVIDIA graphics driver. Blocking: reads
/// `mods.json` and the driver settings.
pub fn state(game: &Game, catalog: &Catalog) -> Result<Option<OverrideState>> {
    let record = super::load()?.dlss_override(&game.id).cloned();
    let exe = exe(game, record.as_ref())?;
    let Some(profile) = nvidia::read_settings(&exe, &ids(catalog, record.as_ref()))? else {
        return Ok(None);
    };
    Ok(Some(OverrideState {
        exe,
        profile,
        record,
    }))
}

/// Gives the game's profile the `choice` (setting id → value; a setting left out goes back to
/// what it was), as administrator when the driver asks for it, and returns `mods.json` as saved.
/// Blocking: NVAPI, maybe a UAC prompt.
pub fn save(game: &Game, catalog: &Catalog, choice: BTreeMap<u32, u32>) -> Result<ModsFile> {
    let mods = super::load()?;
    let record = mods.dlss_override(&game.id).cloned();
    let exe = exe(game, record.as_ref())?;
    let ids = ids(catalog, record.as_ref());
    let profile = nvidia::read_settings(&exe, &ids)?.ok_or_else(no_driver)?;
    // A profile DXVK's settings created is Game Manager's too.
    let ours = profile.name.as_deref().is_some_and(|name| {
        mods.dxvk(&game.id)
            .and_then(|dxvk| dxvk.nvidia_original.as_ref())
            .is_some_and(|other| other.created(name))
    });
    let wanted: Vec<(u32, Option<u32>)> = ids
        .iter()
        .map(|&id| (id, choice.get(&id).copied()))
        .collect();
    let (plan, original) = nvidia::plan_settings(
        &exe,
        &nvidia::new_profile_name(&game.name),
        &profile,
        &wanted,
        record.as_ref().map(|record| &record.original),
        ours,
    );
    nvidia::run(&plan)?;
    update(&game.id, |mods| {
        mods.dlss_override = original.map(|original| DlssOverride {
            exe,
            choice,
            original,
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvidia::{DrsPlan, NvidiaChoice, ProfileState, SettingState, plan, plan_settings};

    fn shipped() -> Catalog {
        serde_json::from_str(include_str!("../../../config/nvidia-overrides.json")).unwrap()
    }

    #[test]
    fn the_catalog_parses_with_unique_ids() {
        let catalog = shipped();
        assert_eq!(catalog.groups.len(), 4);
        let ids: Vec<u32> = catalog.ids().collect();
        let unique: BTreeSet<u32> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len());
        assert_eq!(ids.len(), 16);
        let preset = catalog.setting(0x10E4_1DF3).unwrap();
        assert_eq!(preset.label(0x00FF_FFFF), "Recommended (latest model)");
        assert_eq!(preset.label(11), "Preset K (Transformer)");
        assert_eq!(preset.label(99), "0x00000063");
        let ratio = catalog.setting(0x10E4_1DF5).unwrap();
        let values = ratio.values(None);
        assert_eq!(values.len(), 1 + 68);
        assert_eq!(ratio.label(58), "58% (Balanced)");
        assert_eq!(ratio.label(59), "59%");
        for setting in catalog.settings() {
            let values = setting.values(None);
            let unique: BTreeSet<u32> = values.iter().map(|(value, _)| *value).collect();
            assert_eq!(unique.len(), values.len(), "{}", setting.name);
        }
    }

    #[test]
    fn dll_overrides_name_the_drivers_copy() {
        let catalog = shipped();
        let sr = catalog.setting(0x10E4_1E01).unwrap();
        let driver = Driver {
            version: 62112,
            dlls: BTreeMap::from([("dlss".to_owned(), "310.9.16".to_owned())]),
        };
        assert_eq!(
            sr.values(driver.dll(sr))[1].1,
            "On: the driver's latest (310.9.16)"
        );
        assert_eq!(sr.values(None)[1].1, "On: the driver's latest");
        assert_eq!(driver.label(), "621.12");
    }

    #[test]
    fn settings_newer_than_the_driver_are_hidden() {
        let catalog = shipped();
        let streamline = catalog.setting(0x10E4_1E06).unwrap();
        assert!(!streamline.available(Some(61655)));
        assert!(streamline.available(Some(61656)));
        assert!(streamline.available(None));
        assert!(catalog.setting(0x10E4_1E01).unwrap().available(Some(1)));
        assert_eq!(driver_number("572.83"), Some(57283));
    }

    fn profile(name: Option<&str>, settings: &[(u32, Option<u32>)]) -> Profile {
        Profile {
            name: name.map(str::to_owned),
            settings: settings
                .iter()
                .map(|&(id, user_value)| {
                    (
                        id,
                        SettingState {
                            value: user_value,
                            user_value,
                        },
                    )
                })
                .collect(),
        }
    }

    const SR: u32 = 0x10E4_1E01;
    const PRESET: u32 = 0x10E4_1DF3;
    const NEW: &str = "Test (Game Manager)";

    fn plan_for(
        state: &Profile,
        wanted: &[(u32, Option<u32>)],
        original: Option<&Originals>,
        ours: bool,
    ) -> (DrsPlan, Option<Originals>) {
        plan_settings("test.exe", NEW, state, wanted, original, ours)
    }

    #[test]
    fn only_settings_changed_go_back() {
        // The user had set a preset; Game Manager turns on the DLL override only.
        let before = profile(Some("Game"), &[(SR, None), (PRESET, Some(4))]);
        let (first, original) = plan_for(&before, &[(SR, Some(1)), (PRESET, None)], None, false);
        assert_eq!(first.set, [(SR, 1)]);
        assert!(first.delete.is_empty() && first.create_profile.is_none());
        let original = original.unwrap();
        assert_eq!(original.values, BTreeMap::from([(SR, None)]));

        // Then the preset too: its value before is recorded the first time.
        let during = profile(Some("Game"), &[(SR, Some(1)), (PRESET, Some(4))]);
        let (second, original) = plan_for(
            &during,
            &[(SR, Some(1)), (PRESET, Some(11))],
            Some(&original),
            false,
        );
        assert_eq!(second.set, [(SR, 1), (PRESET, 11)]);
        let original = original.unwrap();
        assert_eq!(
            original.values,
            BTreeMap::from([(SR, None), (PRESET, Some(4))])
        );

        // Back: the override goes, the user's preset returns, the profile stays.
        let now = profile(Some("Game"), &[(SR, Some(1)), (PRESET, Some(11))]);
        let (back, kept) = plan_for(&now, &[(SR, None), (PRESET, None)], Some(&original), false);
        assert_eq!(back.delete, [SR]);
        assert_eq!(back.set, [(PRESET, 4)]);
        assert!(!back.delete_profile);
        assert_eq!(kept, None);
    }

    #[test]
    fn a_created_profile_goes_when_the_last_record_goes_back() {
        // The overrides create the profile; DXVK's settings then count it as theirs too.
        let none = profile(None, &[(SR, None)]);
        let (first, overrides) = plan_for(&none, &[(SR, Some(1))], None, false);
        assert_eq!(first.create_profile.as_deref(), Some(NEW));
        let overrides = overrides.unwrap();
        assert!(overrides.created(NEW));

        let dxvk_before = ProfileState {
            profile: Some(NEW.to_owned()),
            ..ProfileState::default()
        };
        let wanted = NvidiaChoice {
            present_method: Some(1),
            present_flags: None,
        };
        let ours = overrides.created(NEW);
        let (dxvk_plan, dxvk) = plan("test.exe", NEW, &dxvk_before, wanted, None, ours);
        assert_eq!(dxvk_plan.create_profile, None);
        let dxvk = dxvk.unwrap();
        assert!(dxvk.created(NEW));

        // Either going back asks to delete the profile; the driver keeps it while the other's
        // settings are still in it (`apply` checks it is empty).
        let set = SettingState {
            value: Some(1),
            user_value: Some(1),
        };
        let dxvk_now = ProfileState {
            profile: Some(NEW.to_owned()),
            method: set,
            flags: SettingState::default(),
        };
        let (dxvk_back, _) = plan(
            "test.exe",
            NEW,
            &dxvk_now,
            NvidiaChoice::default(),
            Some(&dxvk),
            false,
        );
        assert!(dxvk_back.delete_profile);
        let now = profile(Some(NEW), &[(SR, Some(1))]);
        let (back, kept) = plan_for(&now, &[(SR, None)], Some(&overrides), false);
        assert_eq!(back.delete, [SR]);
        assert!(back.delete_profile);
        assert_eq!(kept, None);
    }

    #[test]
    fn records_survive_mods_json() {
        let record = DlssOverride {
            exe: "game.exe".into(),
            choice: BTreeMap::from([(SR, 1)]),
            original: Originals {
                profile: "Game".into(),
                created_profile: false,
                values: BTreeMap::from([(SR, None), (PRESET, Some(4))]),
            },
        };
        let text = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<DlssOverride>(&text).unwrap(), record);
    }

    /// Applies overrides to a made-up program's new profile in the real driver settings, reads
    /// them back and goes back (the profile is deleted); then once more with DXVK's Present Method
    /// set first, so the profile is shared. Writes may need administrator rights (NVAPI −137).
    #[test]
    #[ignore = "changes the NVIDIA driver settings; run by hand on an NVIDIA PC"]
    fn live_overrides_round_trip() {
        let exe = "gm-dlss-override-test.exe";
        let name = "Game Manager DLSS override test (Game Manager)";
        let ids = [SR, PRESET];
        let read = || nvidia::read_settings(exe, &ids).unwrap();
        // Without administrator rights the driver may refuse: nothing is saved then.
        let write = |plan: &DrsPlan| match nvidia::apply(plan) {
            Err(Error::Nvapi {
                status: nvidia::INVALID_USER_PRIVILEGE,
                ..
            }) => {
                eprintln!("NVAPI writes need administrator rights here: run this test elevated");
                false
            }
            other => {
                other.unwrap();
                true
            }
        };
        let Some(before) = read() else {
            return;
        };
        assert_eq!(before.name, None, "{exe} already has a profile");
        let wanted = [(SR, Some(1)), (PRESET, Some(0x00FF_FFFF))];
        let (first, original) = plan_settings(exe, name, &before, &wanted, None, false);
        if !write(&first) {
            return;
        }
        let during = read().unwrap();
        assert_eq!(during.name.as_deref(), Some(name));
        assert_eq!(during.setting(SR).user_value, Some(1));
        assert_eq!(during.setting(PRESET).user_value, Some(0x00FF_FFFF));
        let back = [(SR, None), (PRESET, None)];
        let (undo, kept) = plan_settings(exe, name, &during, &back, original.as_ref(), false);
        assert_eq!(kept, None);
        assert!(write(&undo));
        assert_eq!(read().unwrap().name, None);

        // Shared: DXVK's method first creates the profile, then the overrides; DXVK goes back
        // first (the profile stays), then the overrides (it goes).
        let dxvk_wanted = NvidiaChoice {
            present_method: Some(1),
            present_flags: None,
        };
        let dxvk_state = nvidia::read(exe).unwrap().unwrap();
        let (dxvk_plan, dxvk) = plan(exe, name, &dxvk_state, dxvk_wanted, None, false);
        assert!(write(&dxvk_plan));
        let shared = read().unwrap();
        let ours = dxvk.as_ref().unwrap().created(name);
        let (first, original) = plan_settings(exe, name, &shared, &wanted, None, ours);
        if !write(&first) {
            return;
        }
        let dxvk_state = nvidia::read(exe).unwrap().unwrap();
        let (dxvk_back, _) = plan(
            exe,
            name,
            &dxvk_state,
            NvidiaChoice::default(),
            dxvk.as_ref(),
            false,
        );
        assert!(write(&dxvk_back));
        let during = read().unwrap();
        assert_eq!(during.name.as_deref(), Some(name), "the overrides keep it");
        let (undo, _) = plan_settings(exe, name, &during, &back, original.as_ref(), false);
        assert!(write(&undo));
        assert_eq!(read().unwrap().name, None);
    }
}
