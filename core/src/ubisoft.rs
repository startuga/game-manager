//! Finds games installed through Ubisoft Connect (Uplay before it).
//!
//! **What the launcher leaves in the registry** (32-bit view):
//! - `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher`: its own folder (`InstallDir`), and one
//!   `Installs\<install id>` key per game with the game's folder (`InstallDir`, written with
//!   forward slashes). An entry outlives its folder when the game was removed some other way, so
//!   only a folder that exists counts.
//! - A Windows Uninstall entry `Uplay Install <install id>` with the game's plain name
//!   (`DisplayName`) and its icon (`DisplayIcon`, an `.ico` in the launcher's `data` folder).
//!
//! **Play** goes through Ubisoft Connect, `uplay://launch/<install id>/0`, as the launcher's own
//! shortcuts do: it signs in, syncs saves and starts the game.
//!
//! **The game's program** is not in the registry. Two sources, in this order:
//! 1. **An older launcher's configuration cache**, `<launcher>\cache\configuration\configurations`:
//!    a protobuf list of `{ 1: Ubisoft id, 2: install id, 3: a YAML document }`. The document
//!    names the game, its pictures and its programs (`root.start_game.online.executables`), each
//!    with a folder under the game's (`working_directory.append`) and arguments. Values can be
//!    keys of `localizations.default` (`name: l1`). The first program that exists is the game;
//!    the others (a settings tool) become extra launch actions, started directly.
//!
//!    Current launchers keep this cache encrypted in
//!    `%LOCALAPPDATA%\Ubisoft Game Launcher\cache\configuration\configurations.vdat` and no
//!    longer update the plain file, so it covers only games an older launcher knew. It is read
//!    when it is there and never required.
//! 2. **The largest `.exe` in the game's folder**, outside the launcher's `installer_resources`
//!    (which holds a 66 MB `UplayInstaller.exe`). The user can pick another in Game settings.
//!
//! The YAML is read by a small reader of our own (keys, indentation, quotes): the documents are
//! plain block mappings, and anything it cannot read is left out, never an error.
//!
//! Cover art (for `covers.rs`): the same old cache names a 240 × 280 picture kept in
//! `<launcher>\cache\assets`. It is smaller than a tile and not 2:3, so a cover found in the
//! Steam store by name wins over it.

use crate::text::search_name;
use crate::{Error, Game, IconRef, Launch, LaunchOption, Result, Store, UNINSTALL_KEYS, graphics};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use windows_registry::LOCAL_MACHINE;

const LAUNCHER_KEY: &str = r"SOFTWARE\WOW6432Node\Ubisoft\Launcher";
/// The launcher's own files inside a game's folder: never the game's program.
const INSTALLER_FOLDER: &str = "installer_resources";

/// What the registry says about one installed game.
#[derive(Debug, Default)]
struct Install {
    /// The key's name under `Installs`: digits.
    id: String,
    dir: PathBuf,
    /// The Uninstall entry's `DisplayName`, or empty.
    display_name: String,
    /// The Uninstall entry's `DisplayIcon`.
    display_icon: Option<IconRef>,
}

/// Scans Ubisoft Connect's registry key for installed games.
pub fn scan() -> Result<Vec<Game>> {
    let Ok(installs) = LOCAL_MACHINE.open(format!(r"{LAUNCHER_KEY}\Installs")) else {
        return Ok(Vec::new());
    };
    let ids = installs
        .keys()
        .map_err(Error::windows(format!(r"{LAUNCHER_KEY}\Installs")))?;
    let launcher = Launcher::find();

    let mut games = Vec::new();
    for id in ids {
        // The id goes into the launch link: only what Ubisoft Connect writes there.
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Some(dir) = installs
            .open(&id)
            .and_then(|key| key.get_string("InstallDir"))
            .ok()
            .and_then(|dir| install_dir(&dir))
            .filter(|dir| dir.is_dir())
        else {
            continue;
        };
        let (display_name, display_icon) = uninstall_entry(&id);
        let install = Install {
            id,
            dir,
            display_name,
            display_icon,
        };
        let configuration = launcher
            .as_ref()
            .and_then(|launcher| launcher.configuration(&install.id));
        games.push(build_game(
            &install,
            configuration.as_ref(),
            launcher.as_ref().map(|launcher| launcher.dir.as_path()),
        ));
    }
    Ok(games)
}

/// A registry `InstallDir` (`C:/Games/Child of Light/`) as a path; `None` when it is empty.
fn install_dir(value: &str) -> Option<PathBuf> {
    let dir = value.trim().replace('/', "\\");
    let dir = dir.trim_end_matches('\\');
    match dir {
        "" => None,
        // A drive's root keeps its backslash: `C:` alone is the drive's current folder.
        drive if drive.ends_with(':') => Some(PathBuf::from(format!("{drive}\\"))),
        dir => Some(PathBuf::from(dir)),
    }
}

/// The name and icon of the game's Windows Uninstall entry, `Uplay Install <install id>`.
fn uninstall_entry(id: &str) -> (String, Option<IconRef>) {
    let key = UNINSTALL_KEYS.iter().rev().find_map(|path| {
        LOCAL_MACHINE
            .open(format!(r"{path}\Uplay Install {id}"))
            .ok()
    });
    let Some(key) = key else {
        return (String::new(), None);
    };
    let icon = key
        .get_string("DisplayIcon")
        .ok()
        .and_then(|location| IconRef::parse(&location.replace('/', "\\")));
    (key.get_string("DisplayName").unwrap_or_default(), icon)
}

/// A game from its registry entry and, when an older launcher left one, its configuration.
/// `launcher` is Ubisoft Connect's folder. Looks at the game's folder: which programs exist.
fn build_game(
    install: &Install,
    configuration: Option<&Configuration>,
    launcher: Option<&Path>,
) -> Game {
    let dir = &install.dir;
    let name = [
        install.display_name.trim().to_owned(),
        configuration
            .and_then(|configuration| configuration.name.as_deref())
            .map(search_name)
            .unwrap_or_default(),
        dir.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ]
    .into_iter()
    .find(|name| !name.is_empty())
    .unwrap_or_else(|| install.id.clone());

    // The configuration's programs that exist: the first is the game, the others are extras.
    let mut programs = configuration
        .map(|configuration| configuration.executables.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|executable| {
            let folder = inside(dir, &executable.append)?;
            let path = inside(&folder, &executable.relative)?;
            path.is_file().then_some((executable, folder, path))
        });
    let first = programs
        .next()
        .map(|(executable, _, path)| (path, executable.arguments.as_str()));

    let mut alt_launches: Vec<LaunchOption> = Vec::new();
    for (executable, folder, path) in programs {
        let label = match executable.label.trim() {
            "" => path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            label => search_name(label),
        };
        // The game itself is Play; a program listed twice is offered once.
        let is_game = first
            .as_ref()
            .is_some_and(|(game, arguments)| *game == path && *arguments == executable.arguments);
        let launch = Launch::Exe {
            path,
            args: executable.arguments.clone(),
            working_dir: Some(folder),
        };
        let listed = alt_launches
            .iter()
            .any(|other| other.launch == launch || other.label == label);
        if !is_game && !listed {
            alt_launches.push(LaunchOption { label, launch });
        }
    }
    let program = first.map(|(path, _)| path).or_else(|| largest_program(dir));

    let icon = install
        .display_icon
        .clone()
        .filter(|icon| icon.path.is_file())
        .or_else(|| {
            let file_name = configuration?.icon.as_deref()?;
            let file = inside(&launcher?.join("data"), file_name)?;
            file.is_file().then(|| IconRef::file(file))
        })
        .or_else(|| program.clone().map(IconRef::file));

    Game {
        id: format!("ubisoft:{}", install.id),
        name,
        store: Store::Ubisoft,
        install_dir: Some(dir.clone()),
        launch: Launch::Uri(format!("uplay://launch/{}/0", install.id)),
        alt_launches,
        icon,
        program,
        arch: None,
        edits: None,
        pinned: None,
        hidden: false,
        installed: None,
    }
}

/// `relative` under `dir`; `None` when it would leave the folder (`..`, a drive, a root). An
/// empty `relative` is the folder itself.
fn inside(dir: &Path, relative: &str) -> Option<PathBuf> {
    let relative = relative.trim().replace('/', "\\");
    let relative = Path::new(relative.trim_end_matches('\\'));
    if relative.as_os_str().is_empty() {
        return Some(dir.to_path_buf());
    }
    relative
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
        .then(|| dir.join(relative))
}

/// The largest `.exe` in a game's folder that is not the launcher's own: the game, as a rule.
fn largest_program(dir: &Path) -> Option<PathBuf> {
    graphics::programs_by_size(dir).into_iter().find(|program| {
        !program
            .components()
            .any(|part| part.as_os_str().eq_ignore_ascii_case(INSTALLER_FOLDER))
    })
}

// ---------------------------------------------------------------------------------------------
// An older launcher's configuration cache
// ---------------------------------------------------------------------------------------------

/// Ubisoft Connect's folder, with the game configurations an older launcher cached there.
pub(crate) struct Launcher {
    pub(crate) dir: PathBuf,
    /// The YAML documents by install id; empty when there is no readable cache.
    pub(crate) configurations: HashMap<u64, String>,
}

impl Launcher {
    /// Ubisoft Connect's folder from the registry; `None` when it isn't installed.
    pub(crate) fn find() -> Option<Self> {
        let dir = LOCAL_MACHINE
            .open(LAUNCHER_KEY)
            .and_then(|key| key.get_string("InstallDir"))
            .ok()
            .and_then(|dir| install_dir(&dir))
            .filter(|dir| dir.is_dir())?;
        let configurations = fs::read(dir.join(r"cache\configuration\configurations"))
            .map(|bytes| parse_cache(&bytes))
            .unwrap_or_default();
        Some(Self {
            dir,
            configurations,
        })
    }

    fn configuration(&self, install_id: &str) -> Option<Configuration> {
        let yaml = self.configurations.get(&install_id.parse().ok()?)?;
        Some(parse_configuration(yaml))
    }

    /// The launcher's own picture of a game (240 × 280), when an older launcher cached it.
    pub(crate) fn cached_cover(&self, install_id: &str) -> Option<PathBuf> {
        let thumb = self.configuration(install_id)?.thumb?;
        let file = inside(&self.dir.join(r"cache\assets"), &thumb)?;
        file.is_file().then_some(file)
    }
}

/// A protobuf field's value, as far as the cache needs it.
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
    /// A fixed 32 or 64-bit value: skipped.
    Fixed,
}

/// Reads protobuf fields off the front of `bytes`.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self.bytes.split_first()?;
            self.bytes = rest;
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn take(&mut self, len: u64) -> Option<&'a [u8]> {
        let (taken, rest) = self.bytes.split_at_checked(usize::try_from(len).ok()?)?;
        self.bytes = rest;
        Some(taken)
    }

    /// The next field's number and value; `None` at the end, or where the data stops making sense.
    fn field(&mut self) -> Option<(u64, Field<'a>)> {
        let tag = self.varint()?;
        let value = match tag & 7 {
            0 => Field::Number(self.varint()?),
            1 => self.take(8).map(|_| Field::Fixed)?,
            2 => {
                let len = self.varint()?;
                Field::Bytes(self.take(len)?)
            }
            5 => self.take(4).map(|_| Field::Fixed)?,
            _ => return None,
        };
        Some((tag >> 3, value))
    }
}

/// The cache's YAML documents by install id. Entries without an install id (games that can't be
/// installed on their own, DLC) or without a document are left out; a file cut short gives what
/// came before the cut.
fn parse_cache(bytes: &[u8]) -> HashMap<u64, String> {
    let mut found = HashMap::new();
    let mut file = Reader { bytes };
    while let Some((number, field)) = file.field() {
        let (1, Field::Bytes(entry)) = (number, field) else {
            continue;
        };
        let mut entry = Reader { bytes: entry };
        let (mut install_id, mut yaml) = (0, &[][..]);
        while let Some(field) = entry.field() {
            match field {
                (2, Field::Number(id)) => install_id = id,
                (3, Field::Bytes(text)) => yaml = text,
                _ => {}
            }
        }
        if install_id != 0 && !yaml.is_empty() {
            found
                .entry(install_id)
                .or_insert_with(|| String::from_utf8_lossy(yaml).into_owned());
        }
    }
    found
}

/// What the scanner uses of a game's configuration.
#[derive(Debug, Default, PartialEq)]
struct Configuration {
    /// As the launcher shows it, with ™ and ®.
    name: Option<String>,
    /// A file in the launcher's `cache\assets`.
    thumb: Option<String>,
    /// A file in the launcher's `data`.
    icon: Option<String>,
    executables: Vec<Executable>,
}

/// One program of `root.start_game.online.executables`.
#[derive(Debug, Default, PartialEq)]
struct Executable {
    /// `path.relative`: the program, under the working directory.
    relative: String,
    /// `working_directory.append`: a folder under the game's, or empty.
    append: String,
    arguments: String,
    /// What the launcher calls it: `shortcut_name`, else `description`, else `internal_name`.
    label: String,
}

/// One `key: value` line of a YAML block mapping.
#[derive(Debug)]
struct Line<'a> {
    /// The column of the key; for the first key of a list item, after its `- `.
    indent: usize,
    key: &'a str,
    value: String,
    /// The first key of a list item.
    item: bool,
}

/// The `key: value` lines of a YAML document; comments and anything else are skipped.
fn yaml_lines(yaml: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut item = false;
    for raw in yaml.lines() {
        let mut text = raw.trim_start_matches([' ', '\t']);
        let mut indent = raw.len() - text.len();
        // `- key: value` starts a list item, and so does a `-` on a line of its own.
        while let Some(rest) = text.strip_prefix('-')
            && (rest.is_empty() || rest.starts_with(' '))
        {
            let after = rest.trim_start_matches(' ');
            indent += text.len() - after.len();
            text = after;
            item = true;
        }
        let text = text.trim_end();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        // The key ends at the first colon followed by a space or the line's end.
        let colon = text
            .match_indices(':')
            .map(|(at, _)| at)
            .find(|&at| text[at + 1..].is_empty() || text[at + 1..].starts_with(' '));
        let Some(colon) = colon else {
            // A plain list item (`- value`): not a mapping's first key.
            item = false;
            continue;
        };
        lines.push(Line {
            indent,
            key: text[..colon].trim().trim_matches(['"', '\'']),
            value: yaml_scalar(&text[colon + 1..]),
            item: std::mem::take(&mut item),
        });
    }
    lines
}

/// A scalar without its quotes: `"…"` (with `\"`), `'…'` (with `''`), or plain text up to a
/// ` #` comment.
fn yaml_scalar(text: &str) -> String {
    let text = text.trim();
    let mut out = String::new();
    if let Some(quoted) = text.strip_prefix('"') {
        let mut chars = quoted.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => out.extend(chars.next()),
                c => out.push(c),
            }
        }
    } else if let Some(quoted) = text.strip_prefix('\'') {
        let mut chars = quoted.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\'' if chars.peek() == Some(&'\'') => out.extend(chars.next()),
                '\'' => break,
                c => out.push(c),
            }
        }
    } else {
        out.push_str(text.split(" #").next().unwrap_or_default().trim_end());
    }
    out
}

/// The line of `block` named `key` among its top keys, with the lines under it.
fn child<'a, 'y>(block: &'a [Line<'y>], key: &str) -> Option<(&'a Line<'y>, &'a [Line<'y>])> {
    let indent = block.first()?.indent;
    let at = block
        .iter()
        .position(|line| line.indent == indent && line.key == key)?;
    let rest = &block[at + 1..];
    let end = rest
        .iter()
        .position(|line| line.indent <= indent)
        .unwrap_or(rest.len());
    Some((&block[at], &rest[..end]))
}

/// The lines under the key at `path` (`["root", "start_game"]`); empty when there is none.
fn under<'a, 'y>(mut lines: &'a [Line<'y>], path: &[&str]) -> &'a [Line<'y>] {
    for key in path {
        lines = child(lines, key).map_or(&[][..], |(_, under)| under);
    }
    lines
}

/// The items of a list of mappings: each starts at a `- key:` of the list's own indent.
fn items<'a, 'y>(list: &'a [Line<'y>]) -> Vec<&'a [Line<'y>]> {
    let Some(indent) = list.first().map(|line| line.indent) else {
        return Vec::new();
    };
    let starts: Vec<usize> = (0..list.len())
        .filter(|&at| list[at].item && list[at].indent == indent)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| &list[start..starts.get(n + 1).copied().unwrap_or(list.len())])
        .collect()
}

/// The name, pictures and programs of a game's configuration document.
fn parse_configuration(yaml: &str) -> Configuration {
    let lines = yaml_lines(yaml);
    let root = under(&lines, &["root"]);
    let texts = under(&lines, &["localizations", "default"]);
    // A value can be a key of the default texts (`name: GAMENAME`).
    let text = |lines: &[Line], key: &str| {
        let value = &child(lines, key)?.0.value;
        let value = child(texts, value).map_or(value, |(line, _)| &line.value);
        (!value.is_empty()).then(|| value.clone())
    };
    let executables = items(under(root, &["start_game", "online", "executables"]))
        .into_iter()
        .filter_map(|item| {
            Some(Executable {
                relative: text(under(item, &["path"]), "relative")?,
                append: text(under(item, &["working_directory"]), "append").unwrap_or_default(),
                arguments: text(item, "arguments").unwrap_or_default(),
                label: ["shortcut_name", "description", "internal_name"]
                    .into_iter()
                    .find_map(|key| text(item, key))
                    .unwrap_or_default(),
            })
        })
        .collect();
    Configuration {
        name: text(root, "name"),
        thumb: text(root, "thumb_image"),
        icon: text(root, "icon_image"),
        executables,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed copies of real configuration documents.

    /// Install 232: values are keys of the default texts; two programs; the list's dashes at
    /// its key's own indent; LF line ends.
    const BEYOND_GOOD_AND_EVIL: &str = "version: 2.0
root:
  name: GAMENAME
  thumb_image: THUMBIMAGE
  icon_image: ICONIMAGE
  background_color: 000000
  start_game:
    online:
      after_game_report_enabled: no
      executables:
      - path:
          relative: BGE.exe
        working_directory:
          register: HKEY_LOCAL_MACHINE\\SOFTWARE\\Ubisoft\\Beyond Good & Evil\\Install path
        internal_name: Game
        description: EXE_DESCRIPTION_1
      - path:
          relative: SettingsApplication.exe
        working_directory:
          register: HKEY_LOCAL_MACHINE\\SOFTWARE\\Ubisoft\\Beyond Good & Evil\\Install path
        internal_name: Settings
        description: EXE_DESCRIPTION_2
    offline:
      executables:
      - path:
          relative: Offline.exe
localizations:
  default:
    GAMENAME: \"Beyond Good and Evil™\"
    THUMBIMAGE: 3b7a697bedd6e12e81519f3bc1dcf5a4.jpg
    HELPURL: http://support.ubi.com
    ICONIMAGE: b210fc1356a7938eb80daf2278b160b7.ico
    EXE_DESCRIPTION_1: Game
    EXE_DESCRIPTION_2: Settings
  fr-FR:
    GAMENAME: \"Beyond Good and Evil™ (FR)\"
";

    /// Install 1848: CRLF; a folder under the game's and arguments; a `trial` block whose
    /// `arguments` is not the program's.
    const CHRONICLES_RUSSIA: &str = "version: 2.0\r\nroot:\r\n  name: Assassin’s Creed® Chronicles Russia\r\n  thumb_image: l1\r\n  help_url: http://support.ubi.com\r\n  icon_image: l3\r\n  start_game:\r\n    online:\r\n      overlay_required: yes\r\n      executables:\r\n      - shortcut_name: Assassin’s Creed® Chronicles Russia\r\n        path:\r\n          relative: ACCGame-Win32-Shipping.exe\r\n        trial:\r\n          arguments: -trial\r\n        working_directory:\r\n          register: HKEY_LOCAL_MACHINE\\SOFTWARE\\Ubisoft\\Launcher\\Installs\\1848\\InstallDir\r\n          append: Binaries\\Win32\r\n        arguments: -seekfreeloadingpcconsole\r\n        icon_image: 108bbb65468623a118dbd6bcb5680d0c.ico\r\n      pc_bang: {}\r\n    offline:\r\n      executables: []\r\nlocalizations:\r\n  default:\r\n    l1: 239ea35ca6be64d0e10120b818f567d4.jpg\r\n    l3: ef399b53ac336806eba9582c6f93c0b0.ico\r\n  ru-RU:\r\n    l1: 25ff6b82460bfdc6f6c84e2f5df46dfc.jpg\r\n";

    /// Install 95: a quoted name; the list indented under its key; `path:` with a trailing
    /// space.
    const CHAOS_THEORY: &str = "# Splinter Cell
version: 2.0
root:
  name: \"Tom Clancy's Splinter Cell Chaos Theory™\"
  thumb_image: 5afce18505053ddcecd8f0cbfc8041e8.jpg
  icon_image: c6ab5b11d7dc4ad6372d63637fa0fd9c.ico
  start_game:
    online:
      executables:
        - path:
            relative: splintercell3.exe
          working_directory:
            register: HKEY_LOCAL_MACHINE\\SOFTWARE\\Ubisoft\\Tom Clancy's Splinter Cell Chaos Theory\\InstallDir
            append: system
";

    fn executable(relative: &str, append: &str, arguments: &str, label: &str) -> Executable {
        Executable {
            relative: relative.into(),
            append: append.into(),
            arguments: arguments.into(),
            label: label.into(),
        }
    }

    #[test]
    fn reads_names_pictures_and_programs_of_a_configuration() {
        assert_eq!(
            parse_configuration(BEYOND_GOOD_AND_EVIL),
            Configuration {
                name: Some("Beyond Good and Evil™".into()),
                thumb: Some("3b7a697bedd6e12e81519f3bc1dcf5a4.jpg".into()),
                icon: Some("b210fc1356a7938eb80daf2278b160b7.ico".into()),
                executables: vec![
                    executable("BGE.exe", "", "", "Game"),
                    executable("SettingsApplication.exe", "", "", "Settings"),
                ],
            }
        );
        assert_eq!(
            parse_configuration(CHRONICLES_RUSSIA),
            Configuration {
                name: Some("Assassin’s Creed® Chronicles Russia".into()),
                thumb: Some("239ea35ca6be64d0e10120b818f567d4.jpg".into()),
                icon: Some("ef399b53ac336806eba9582c6f93c0b0.ico".into()),
                executables: vec![executable(
                    "ACCGame-Win32-Shipping.exe",
                    r"Binaries\Win32",
                    "-seekfreeloadingpcconsole",
                    "Assassin’s Creed® Chronicles Russia",
                )],
            }
        );
        assert_eq!(
            parse_configuration(CHAOS_THEORY),
            Configuration {
                name: Some("Tom Clancy's Splinter Cell Chaos Theory™".into()),
                thumb: Some("5afce18505053ddcecd8f0cbfc8041e8.jpg".into()),
                icon: Some("c6ab5b11d7dc4ad6372d63637fa0fd9c.ico".into()),
                executables: vec![executable("splintercell3.exe", "system", "", "")],
            }
        );
    }

    #[test]
    fn a_document_it_cannot_read_gives_nothing() {
        for yaml in [
            "",
            "not yaml at all",
            "root:\n  name: ''\n",
            "- a\n- b\n",
            "root: {}\n",
        ] {
            assert_eq!(
                parse_configuration(yaml),
                Configuration::default(),
                "{yaml:?}"
            );
        }
        // Quotes, doubled quotes, escapes and comments.
        assert_eq!(
            yaml_scalar(" 'Tom Clancy''s Rainbow Six® Siege' "),
            "Tom Clancy's Rainbow Six® Siege"
        );
        assert_eq!(yaml_scalar(r#""Say \"hi\"" # a comment"#), r#"Say "hi""#);
        assert_eq!(yaml_scalar("Anno 1800 # the trial"), "Anno 1800");
        assert_eq!(
            yaml_scalar("Prince of Persia: The Sands of Time™"),
            "Prince of Persia: The Sands of Time™"
        );
    }

    fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        while value >= 0x80 {
            out.push((value & 0x7F) as u8 | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
        out
    }

    /// One entry of the cache: `{ 1: id, 2: install id, 3: yaml }` as field 1 of the file.
    fn cache_entry(id: u64, install_id: u64, yaml: &str) -> Vec<u8> {
        let mut entry = [vec![0x08], varint(id), vec![0x10], varint(install_id)].concat();
        entry.extend(
            [
                vec![0x1A],
                varint(yaml.len() as u64),
                yaml.as_bytes().to_vec(),
            ]
            .concat(),
        );
        [vec![0x0A], varint(entry.len() as u64), entry].concat()
    }

    #[test]
    fn reads_the_cache_by_install_id() {
        let long = format!("root:\n  name: {}\n", "x".repeat(300));
        let bytes = [
            cache_entry(232, 232, "root:\n  name: BGE\n"),
            // Far Cry 3: the Ubisoft id is not the install id.
            cache_entry(101, 46, "root:\n  name: Far Cry 3\n"),
            // No install id (DLC), and no document.
            cache_entry(80, 0, "root:\n  name: DLC\n"),
            cache_entry(4553, 4553, ""),
            // A fixed 32-bit field nobody asked for is skipped.
            vec![0x15, 1, 2, 3, 4],
            cache_entry(13504, 13504, &long),
        ]
        .concat();
        let cache = parse_cache(&bytes);
        let mut ids: Vec<u64> = cache.keys().copied().collect();
        ids.sort_unstable();
        assert_eq!(ids, [46, 232, 13504]);
        assert_eq!(cache[&46], "root:\n  name: Far Cry 3\n");
        assert_eq!(cache[&13504], long);
        // Cut short: what came before the cut.
        let cut = parse_cache(&bytes[..bytes.len() - 20]);
        assert_eq!(cut.len(), 2);
        assert!(parse_cache(b"\xFF\xFF\xFF").is_empty());
        assert!(parse_cache(&[]).is_empty());
    }

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Ubisoft-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(file: &Path, len: usize) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, vec![0; len]).unwrap();
    }

    #[test]
    fn a_game_plays_through_the_launcher_and_knows_its_program() {
        let root = temp_dir("game");
        let launcher = root.join("launcher");
        let dir = root.join("Beyond Good and Evil");
        write(&dir.join("BGE.exe"), 10);
        write(&dir.join("SettingsApplication.exe"), 50);
        write(
            &launcher.join(r"data\b210fc1356a7938eb80daf2278b160b7.ico"),
            1,
        );
        let configuration = parse_configuration(BEYOND_GOOD_AND_EVIL);
        let mut install = Install {
            id: "232".into(),
            dir: dir.clone(),
            display_name: "Beyond Good and Evil".into(),
            display_icon: Some(IconRef::file(root.join("missing.ico"))),
        };

        let game = build_game(&install, Some(&configuration), Some(&launcher));
        assert_eq!(game.id, "ubisoft:232");
        assert_eq!(game.name, "Beyond Good and Evil");
        assert_eq!(game.store, Store::Ubisoft);
        assert_eq!(game.install_dir.as_deref(), Some(dir.as_path()));
        assert_eq!(game.launch, Launch::Uri("uplay://launch/232/0".into()));
        // The configuration's first program, not the largest one.
        assert_eq!(game.program, Some(dir.join("BGE.exe")));
        assert_eq!(
            game.alt_launches,
            [LaunchOption {
                label: "Settings".into(),
                launch: Launch::Exe {
                    path: dir.join("SettingsApplication.exe"),
                    args: String::new(),
                    working_dir: Some(dir.clone()),
                },
            }]
        );
        // The Uninstall entry's icon is gone: the configuration's, in the launcher's data.
        assert_eq!(
            game.icon,
            Some(IconRef::file(
                launcher.join(r"data\b210fc1356a7938eb80daf2278b160b7.ico")
            ))
        );

        // No Uninstall entry: the configuration's name without ™; no launcher: the program's icon.
        install.display_name.clear();
        install.display_icon = None;
        let game = build_game(&install, Some(&configuration), None);
        assert_eq!(game.name, "Beyond Good and Evil");
        assert_eq!(game.icon, Some(IconRef::file(dir.join("BGE.exe"))));

        // No configuration (a current launcher): the folder's name and its largest program.
        let game = build_game(&install, None, Some(&launcher));
        assert_eq!(game.name, "Beyond Good and Evil");
        assert_eq!(game.program, Some(dir.join("SettingsApplication.exe")));
        assert!(game.alt_launches.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn programs_are_looked_for_under_the_games_folder_only() {
        let root = temp_dir("programs");
        let dir = root.join("Chronicles");
        let game_exe = dir.join(r"Binaries\Win32\ACCGame-Win32-Shipping.exe");
        write(&game_exe, 30);
        write(&dir.join(r"installer_resources\UplayInstaller.exe"), 60);
        write(&root.join("outside.exe"), 90);
        let install = Install {
            id: "1848".into(),
            dir: dir.clone(),
            ..Default::default()
        };

        let configuration = parse_configuration(CHRONICLES_RUSSIA);
        let game = build_game(&install, Some(&configuration), None);
        assert_eq!(game.name, "Assassin’s Creed Chronicles Russia");
        assert_eq!(game.program.as_deref(), Some(game_exe.as_path()));
        assert!(game.alt_launches.is_empty());
        // Without the configuration: the largest program that is not the launcher's installer.
        let game = build_game(&install, None, None);
        assert_eq!(game.name, "Chronicles");
        assert_eq!(game.program.as_deref(), Some(game_exe.as_path()));

        // A configuration can't point outside the game's folder.
        let outside = Configuration {
            executables: vec![
                executable(r"..\outside.exe", "", "", "Up"),
                executable("outside.exe", "..", "", "Up too"),
                executable(
                    &root.join("outside.exe").to_string_lossy(),
                    "",
                    "",
                    "Absolute",
                ),
            ],
            ..Default::default()
        };
        let game = build_game(&install, Some(&outside), None);
        assert_eq!(game.program.as_deref(), Some(game_exe.as_path()));
        assert!(game.alt_launches.is_empty());
        assert_eq!(
            inside(&dir, "Binaries/Win32/"),
            Some(dir.join(r"Binaries\Win32"))
        );
        assert_eq!(inside(&dir, ""), Some(dir.clone()));
        assert_eq!(inside(&dir, r"C:\Windows"), None);
        assert_eq!(inside(&dir, r"\\server\share"), None);

        // An empty folder: still a game, with nothing known about its program.
        let empty = Install {
            id: "7".into(),
            dir: root.join("Empty"),
            ..Default::default()
        };
        let game = build_game(&empty, None, None);
        assert_eq!((game.program, game.icon), (None, None));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn registry_folders_become_paths() {
        let dir = |value: &str| install_dir(value).map(|dir| dir.to_string_lossy().into_owned());
        assert_eq!(
            dir("C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Child of Light/"),
            Some(
                r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Child of Light".into()
            )
        );
        assert_eq!(
            dir(r"D:\Games\Anno 1800"),
            Some(r"D:\Games\Anno 1800".into())
        );
        assert_eq!(dir("E:/"), Some(r"E:\".into()));
        assert_eq!(dir("  "), None);
    }

    #[test]
    fn the_launchers_own_picture_is_its_cached_thumbnail() {
        let root = temp_dir("cover");
        let thumb = root.join(r"cache\assets\3b7a697bedd6e12e81519f3bc1dcf5a4.jpg");
        write(&thumb, 1);
        let launcher = Launcher {
            dir: root.clone(),
            configurations: HashMap::from([
                (232, BEYOND_GOOD_AND_EVIL.to_owned()),
                // Its picture was never downloaded.
                (95, CHAOS_THEORY.to_owned()),
                (7, "root:\n  thumb_image: ..\\..\\secret.jpg\n".to_owned()),
            ]),
        };
        assert_eq!(launcher.cached_cover("232"), Some(thumb));
        assert_eq!(launcher.cached_cover("95"), None);
        assert_eq!(launcher.cached_cover("7"), None);
        assert_eq!(launcher.cached_cover("1848"), None);
        assert_eq!(launcher.cached_cover("abc"), None);
        fs::remove_dir_all(&root).unwrap();
    }
}
