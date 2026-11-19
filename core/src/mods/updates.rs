//! Mod update notices: whether a newer build of an installed ReShade or DXVK is out, so its card
//! can say so (Reinstall installs the latest), and whether a swapped upscaler DLL has a newer
//! version listed (`upscalers::update_for`).
//!
//! - **The installed line** (`Line`): the mod and the variant its build is of. ReShade's record
//!   says which (`built`); an older record says it by its version (`main-…` Development, "(no
//!   add-ons)" No Addons, else Stable), and DXVK's always does (`master-…` Development,
//!   `…-HDR-mod-v…` HDR-mod, else Stable). There is a line only when that is also the variant
//!   Reinstall installs (the one chosen in the mod's settings): otherwise Reinstall changes the
//!   variant, which the card already says.
//! - **The latest build** (`latest`): `reshade::latest` or `dxvk::latest`, kept per line in
//!   `%LOCALAPPDATA%\Game Manager\cache\mod-latest.json` for a day; a failure is not asked again
//!   for an hour, the last answer still used meanwhile. An install records what it downloaded
//!   (`remember`), which costs nothing. So each line asks the web at most once a day (twice
//!   GitHub's API for a development build, once for DXVK's releases, reshade.me's page for
//!   ReShade's releases).
//! - **Newer** (`newer`): a release by its version (`upscalers::Version`, " (no add-ons)" left
//!   out); HDR-mod by the fork's version, then the DXVK it is based on; a development build by its
//!   commit: another commit than the one installed is newer, as the latest run's is the newest.

use super::Build;
use super::dxvk::{self, DxvkState, DxvkVariant};
use super::reshade::{self, ReshadeState, ReshadeVariant};
use super::upscalers::Version;
use crate::{Result, json_file};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_FILE: &str = "mod-latest.json";
/// How long a latest build is kept, and a failure before the web is asked again, in seconds.
const KEEP: u64 = 24 * 60 * 60;
const RETRY: u64 = 60 * 60;
/// What an HDR-mod version holds between DXVK's version and the fork's.
const HDR_MOD: &str = "-HDR-mod-v";

/// A mod and the variant of its builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Line {
    Reshade(ReshadeVariant),
    Dxvk(DxvkVariant),
}

impl Line {
    /// The line of a game's installed ReShade, when Reinstall installs the same variant.
    pub fn of_reshade(state: &ReshadeState) -> Option<Line> {
        let installed = state.installed.as_ref()?;
        let built = state.built.unwrap_or_else(|| {
            if installed.version.starts_with("main-") {
                ReshadeVariant::Development
            } else if installed.version.ends_with("(no add-ons)") {
                ReshadeVariant::NoAddons
            } else {
                ReshadeVariant::Stable
            }
        });
        (built == state.variant).then_some(Line::Reshade(built))
    }

    /// The line of a game's installed DXVK, when Reinstall installs the same variant.
    pub fn of_dxvk(state: &DxvkState) -> Option<Line> {
        let version = &state.installed.as_ref()?.version;
        let built = if version.starts_with("master-") {
            DxvkVariant::Development
        } else if version.contains(HDR_MOD) {
            DxvkVariant::HdrMod
        } else {
            DxvkVariant::Stable
        };
        (built == state.variant).then_some(Line::Dxvk(built))
    }

    /// Its key in the cache file: `reshade-Stable`, `dxvk-HdrMod`.
    fn key(self) -> String {
        match self {
            Line::Reshade(variant) => format!("reshade-{variant:?}"),
            Line::Dxvk(variant) => format!("dxvk-{variant:?}"),
        }
    }

    /// Asks the web for the line's latest build. Blocking.
    fn fetch(self) -> Result<Build> {
        match self {
            Line::Reshade(variant) => reshade::latest(variant),
            Line::Dxvk(variant) => dxvk::latest(variant),
        }
    }
}

/// A line's latest build, as its card names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Latest {
    pub version: String,
    /// Its release page, or its commit.
    pub page: String,
}

/// A line in `mod-latest.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cached {
    /// The latest build known, and when it was asked for (seconds since 1970); empty until one is.
    #[serde(default)]
    version: String,
    #[serde(default)]
    page: String,
    #[serde(default)]
    fetched: u64,
    /// When asking last failed, while it is the newer answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failed: Option<u64>,
}

impl Cached {
    fn latest(&self) -> Option<Latest> {
        (!self.version.is_empty()).then(|| Latest {
            version: self.version.clone(),
            page: self.page.clone(),
        })
    }
}

/// One reader or writer of the cache file at a time.
static CACHE: Mutex<()> = Mutex::new(());

fn cache_file() -> Result<PathBuf> {
    Ok(json_file::local_file("cache")?.join(CACHE_FILE))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The line's latest build: kept a day, asked again after a failure only an hour later (the last
/// answer meanwhile); `None` when none is known. Blocking: a few web requests at most.
pub fn latest(line: Line) -> Result<Option<Latest>> {
    latest_in(&cache_file()?, line, now(), Line::fetch)
}

fn latest_in(
    file: &Path,
    line: Line,
    now: u64,
    fetch: impl FnOnce(Line) -> Result<Build>,
) -> Result<Option<Latest>> {
    let _lock = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut cache: BTreeMap<String, Cached> = json_file::load(file).unwrap_or_default();
    let key = line.key();
    let known = cache.get(&key).cloned().unwrap_or_default();
    let fresh = !known.version.is_empty() && now.saturating_sub(known.fetched) < KEEP;
    let waiting = known
        .failed
        .is_some_and(|failed| now.saturating_sub(failed) < RETRY);
    if fresh || waiting {
        return Ok(known.latest());
    }
    match fetch(line) {
        Ok(build) => {
            let cached = Cached {
                version: build.version,
                page: build.page,
                fetched: now,
                failed: None,
            };
            let latest = cached.latest();
            cache.insert(key, cached);
            json_file::save(file, &cache)?;
            Ok(latest)
        }
        Err(error) => {
            let had = known.latest();
            cache.insert(
                key,
                Cached {
                    failed: Some(now),
                    ..known
                },
            );
            json_file::save(file, &cache)?;
            match had {
                Some(latest) => Ok(Some(latest)),
                None => Err(error),
            }
        }
    }
}

/// Records the build an install just downloaded as its line's latest. A failure is ignored: it
/// only means the web is asked later.
pub(crate) fn remember(line: Line, build: &Build) {
    if let Ok(file) = cache_file() {
        remember_in(&file, line, build, now());
    }
}

fn remember_in(file: &Path, line: Line, build: &Build, now: u64) {
    let _lock = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut cache: BTreeMap<String, Cached> = json_file::load(file).unwrap_or_default();
    cache.insert(
        line.key(),
        Cached {
            version: build.version.clone(),
            page: build.page.clone(),
            fetched: now,
            failed: None,
        },
    );
    let _ = json_file::save(file, &cache);
}

/// Whether `latest` is newer than the `installed` version of `line`.
pub fn newer(line: Line, installed: &str, latest: &str) -> bool {
    let installed = installed.trim_end_matches(" (no add-ons)");
    if installed == latest {
        return false;
    }
    match line {
        Line::Reshade(ReshadeVariant::Development) | Line::Dxvk(DxvkVariant::Development) => true,
        Line::Dxvk(DxvkVariant::HdrMod) => {
            // The fork's version first, then the DXVK it is based on.
            let parts = |version: &str| {
                let (dxvk, fork) = version.split_once(HDR_MOD)?;
                Some((Version::parse(fork)?, Version::parse(dxvk)?))
            };
            match (parts(installed), parts(latest)) {
                (Some(installed), Some(latest)) => latest > installed,
                _ => true,
            }
        }
        Line::Reshade(_) | Line::Dxvk(_) => {
            match (Version::parse(installed), Version::parse(latest)) {
                (Some(installed), Some(latest)) => latest > installed,
                _ => true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::mods::Installed;
    use std::cell::Cell;
    use std::fs;

    fn build(version: &str) -> Build {
        Build {
            version: version.into(),
            page: format!("https://example.com/{version}"),
            url: String::new(),
            file_name: String::new(),
            sha256: None,
        }
    }

    fn installed(version: &str) -> Option<Installed> {
        Some(Installed {
            version: version.into(),
            page: String::new(),
            folder: PathBuf::new(),
            files: Vec::new(),
            backups: Vec::new(),
        })
    }

    #[test]
    fn releases_compare_by_version_and_builds_by_commit() {
        let stable = Line::Reshade(ReshadeVariant::Stable);
        assert!(newer(stable, "6.6.2", "6.7.0"));
        assert!(!newer(stable, "6.7.0", "6.6.2"));
        assert!(!newer(stable, "6.7.0", "6.7"));
        assert!(newer(
            Line::Reshade(ReshadeVariant::NoAddons),
            "6.6.2 (no add-ons)",
            "6.7.0"
        ));
        assert!(!newer(
            Line::Reshade(ReshadeVariant::NoAddons),
            "6.7.0 (no add-ons)",
            "6.7.0"
        ));
        let dxvk = Line::Dxvk(DxvkVariant::Stable);
        assert!(newer(dxvk, "2.7", "2.7.1"));
        assert!(!newer(dxvk, "2.7.1", "2.7.1"));
        let hdr = Line::Dxvk(DxvkVariant::HdrMod);
        assert!(newer(hdr, "3.0.2-HDR-mod-v0.3.4", "3.0.2-HDR-mod-v0.3.5"));
        assert!(newer(hdr, "3.0.2-HDR-mod-v0.3.4", "3.1-HDR-mod-v0.3.4"));
        assert!(!newer(hdr, "3.1-HDR-mod-v0.3.5", "3.0.2-HDR-mod-v0.3.4"));
        let development = Line::Dxvk(DxvkVariant::Development);
        assert!(newer(development, "master-0cfe089", "master-1a2b3c4"));
        assert!(!newer(development, "master-0cfe089", "master-0cfe089"));
    }

    #[test]
    fn the_line_is_the_installed_variant_when_reinstall_keeps_it() {
        let reshade = |variant, built, version: &str| ReshadeState {
            variant,
            built,
            installed: installed(version),
            ..ReshadeState::default()
        };
        use ReshadeVariant::{Development, NoAddons, Stable};
        assert_eq!(
            Line::of_reshade(&reshade(Stable, Some(Stable), "6.6.2")),
            Some(Line::Reshade(Stable))
        );
        // Reinstall would change the variant: no notice.
        assert_eq!(
            Line::of_reshade(&reshade(Development, Some(Stable), "6.6.2")),
            None
        );
        // An older record, by its version.
        assert_eq!(
            Line::of_reshade(&reshade(NoAddons, None, "6.6.2 (no add-ons)")),
            Some(Line::Reshade(NoAddons))
        );
        assert_eq!(
            Line::of_reshade(&reshade(Development, None, "main-1a2b3c4")),
            Some(Line::Reshade(Development))
        );
        assert_eq!(Line::of_reshade(&ReshadeState::default()), None);

        let dxvk = |variant, version: &str| DxvkState {
            variant,
            installed: installed(version),
            ..DxvkState::default()
        };
        assert_eq!(
            Line::of_dxvk(&dxvk(DxvkVariant::HdrMod, "3.0.2-HDR-mod-v0.3.4")),
            Some(Line::Dxvk(DxvkVariant::HdrMod))
        );
        assert_eq!(
            Line::of_dxvk(&dxvk(DxvkVariant::Development, "master-0cfe089")),
            Some(Line::Dxvk(DxvkVariant::Development))
        );
        assert_eq!(
            Line::of_dxvk(&dxvk(DxvkVariant::Stable, "master-0cfe089")),
            None
        );
    }

    #[test]
    fn latest_builds_are_kept_a_day_and_failures_an_hour() {
        let dir = std::env::temp_dir().join(format!("GM-ModLatest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(CACHE_FILE);
        let line = Line::Dxvk(DxvkVariant::Stable);
        let asked = Cell::new(0);
        let answer = |version: &'static str| {
            let asked = &asked;
            move |_| {
                asked.set(asked.get() + 1);
                Ok(build(version))
            }
        };
        let fail = |_| {
            asked.set(asked.get() + 1);
            Err(Error::NoRelease { url: "x".into() })
        };
        let start = 1_795_000_000;

        // Nothing known and the web unreachable: the error, and not asked again for an hour.
        assert!(latest_in(&file, line, start, fail).is_err());
        assert_eq!(latest_in(&file, line, start + 60, fail).unwrap(), None);
        assert_eq!(asked.get(), 1);

        let got = latest_in(&file, line, start + RETRY, answer("2.7"))
            .unwrap()
            .unwrap();
        assert_eq!(got.version, "2.7");
        assert_eq!(got.page, "https://example.com/2.7");
        // Kept for a day.
        let later = start + RETRY + KEEP - 1;
        assert_eq!(
            latest_in(&file, line, later, answer("2.8"))
                .unwrap()
                .unwrap()
                .version,
            "2.7"
        );
        assert_eq!(asked.get(), 2);
        // A day later, asked again; a failure keeps the last answer for an hour.
        let next_day = start + RETRY + KEEP;
        assert_eq!(
            latest_in(&file, line, next_day, fail)
                .unwrap()
                .unwrap()
                .version,
            "2.7"
        );
        assert_eq!(
            latest_in(&file, line, next_day + 60, answer("2.8"))
                .unwrap()
                .unwrap()
                .version,
            "2.7"
        );
        assert_eq!(asked.get(), 3);
        // An install's build is remembered, at no cost.
        remember_in(&file, line, &build("2.8"), next_day + 120);
        assert_eq!(
            latest_in(&file, line, next_day + 180, fail)
                .unwrap()
                .unwrap()
                .version,
            "2.8"
        );
        assert_eq!(asked.get(), 3);
        // Each line apart.
        let other = Line::Reshade(ReshadeVariant::Stable);
        assert_eq!(
            latest_in(&file, other, next_day + 180, answer("6.7.0"))
                .unwrap()
                .unwrap()
                .version,
            "6.7.0"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Asks the web for every line's latest build.
    #[test]
    #[ignore = "downloads: reshade.me and GitHub"]
    fn live_mod_latest() {
        for variant in ReshadeVariant::ALL {
            let build = Line::Reshade(variant).fetch().unwrap();
            println!(
                "ReShade {}: {} ({})",
                variant.name(),
                build.version,
                build.page
            );
        }
        for variant in DxvkVariant::ALL {
            let build = Line::Dxvk(variant).fetch().unwrap();
            println!(
                "DXVK {}: {} ({})",
                variant.name(),
                build.version,
                build.page
            );
        }
    }
}
