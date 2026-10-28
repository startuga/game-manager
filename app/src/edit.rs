//! The Game settings expander's draft: the fields as the user types them, and the edits a Save
//! stores. No UI code.

use gm_core::config::GameEdits;
use gm_core::graphics::GraphicsApi;
use gm_core::{Arch, Game, Launch};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditField {
    Name,
    /// The program Play starts.
    Launch,
    Args,
    /// The game's own program.
    Program,
}

/// The expander's drop-downs. Their first item is "As detected", then the choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditChoice {
    Bitness,
    GraphicsApi,
}

/// The bitness a game can be set to (mods such as ReShade and DXVK come in 32 and 64-bit builds).
pub const BITNESS_CHOICES: [Arch; 2] = [Arch::X86, Arch::X64];

impl EditChoice {
    /// The drop-down's items after "As detected".
    pub fn labels(self) -> Vec<&'static str> {
        match self {
            EditChoice::Bitness => BITNESS_CHOICES.iter().map(|arch| arch.label()).collect(),
            EditChoice::GraphicsApi => GraphicsApi::ALL.iter().map(|api| api.label()).collect(),
        }
    }
}

/// The expander's fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditFields {
    pub name: String,
    pub launch: String,
    pub args: String,
    pub program: String,
    /// The chosen bitness; `None` as detected.
    pub arch: Option<Arch>,
    /// The chosen graphics API; `None` as detected.
    pub graphics_api: Option<GraphicsApi>,
}

impl EditFields {
    /// The game's current values: what Play starts and with which arguments, and its program.
    /// A game started through a store link has an empty launch field.
    pub fn of(game: &Game) -> Self {
        let (launch, args) = match &game.launch {
            Launch::Exe { path, args, .. } => (path.display().to_string(), args.clone()),
            Launch::Uri(_) => (
                String::new(),
                game.edits
                    .as_ref()
                    .and_then(|edits| edits.args.clone())
                    .unwrap_or_default(),
            ),
        };
        Self {
            name: game.name.clone(),
            launch,
            args,
            program: game
                .program
                .as_deref()
                .map(|program| program.display().to_string())
                .unwrap_or_default(),
            arch: game.edits.as_ref().and_then(|edits| edits.arch),
            graphics_api: game.edits.as_ref().and_then(|edits| edits.graphics_api),
        }
    }

    pub fn get(&self, field: EditField) -> &str {
        match field {
            EditField::Name => &self.name,
            EditField::Launch => &self.launch,
            EditField::Args => &self.args,
            EditField::Program => &self.program,
        }
    }

    pub fn set(&mut self, field: EditField, value: String) {
        match field {
            EditField::Name => self.name = value,
            EditField::Launch => self.launch = value,
            EditField::Args => self.args = value,
            EditField::Program => self.program = value,
        }
    }

    /// The drop-down's selected item: 0 for "As detected", else the choice's place after it.
    pub fn choice_index(&self, choice: EditChoice) -> usize {
        let position = match choice {
            EditChoice::Bitness => self
                .arch
                .and_then(|arch| BITNESS_CHOICES.iter().position(|&item| item == arch)),
            EditChoice::GraphicsApi => self
                .graphics_api
                .and_then(|api| GraphicsApi::ALL.iter().position(|&item| item == api)),
        };
        position.map_or(0, |position| position + 1)
    }

    /// Sets the choice the drop-down's item `index` stands for; 0 is "As detected".
    pub fn choose(&mut self, choice: EditChoice, index: usize) {
        let position = index.checked_sub(1);
        match choice {
            EditChoice::Bitness => {
                self.arch = position.and_then(|i| BITNESS_CHOICES.get(i).copied());
            }
            EditChoice::GraphicsApi => {
                self.graphics_api = position.and_then(|i| GraphicsApi::ALL.get(i).copied());
            }
        }
    }
}

/// One game's fields being edited, and the values they started from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditDraft {
    pub id: String,
    pub start: EditFields,
    pub fields: EditFields,
}

impl EditDraft {
    pub fn of(game: &Game) -> Self {
        let start = EditFields::of(game);
        Self {
            id: game.id.clone(),
            fields: start.clone(),
            start,
        }
    }

    /// `kept` while it still belongs to `game` as it is now, else a fresh draft: another game was
    /// selected, or a scan brought new values (e.g. after a Save).
    pub fn for_game(kept: Option<&EditDraft>, game: &Game) -> Self {
        let fresh = Self::of(game);
        kept.filter(|draft| draft.id == fresh.id && draft.start == fresh.start)
            .cloned()
            .unwrap_or(fresh)
    }

    pub fn is_dirty(&self) -> bool {
        self.fields != self.start
    }

    /// The edits a Save stores: the game's current edits with every field the user changed. A
    /// blank name or path means "as detected". A new launch program also stores the arguments
    /// shown, so Play starts it exactly as the fields say.
    pub fn edits(&self, mut edits: GameEdits) -> GameEdits {
        let (fields, start) = (&self.fields, &self.start);
        let path =
            |text: &str| Some(PathBuf::from(text.trim())).filter(|p| !p.as_os_str().is_empty());
        if fields.name != start.name {
            edits.name = Some(fields.name.trim().to_owned()).filter(|name| !name.is_empty());
        }
        if fields.launch != start.launch {
            edits.launch = path(&fields.launch);
            if edits.launch.is_some() {
                edits.args = Some(fields.args.trim().to_owned());
            }
        }
        if fields.args != start.args {
            edits.args = Some(fields.args.trim().to_owned());
        }
        if fields.program != start.program {
            edits.program = path(&fields.program);
        }
        if fields.arch != start.arch {
            edits.arch = fields.arch;
        }
        if fields.graphics_api != start.graphics_api {
            edits.graphics_api = fields.graphics_api;
        }
        edits
    }
}

/// A launch program or game program to save must exist.
pub fn check_paths(edits: &GameEdits) -> Result<(), String> {
    for path in [&edits.launch, &edits.program].into_iter().flatten() {
        if !path.is_file() {
            return Err(format!("{} does not exist", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::Store;
    use std::path::PathBuf;

    fn game(launch: Launch) -> Game {
        Game {
            id: "gog:1".into(),
            name: "The Witcher 3".into(),
            store: Store::Gog,
            install_dir: Some(PathBuf::from(r"C:\GOG\W3")),
            launch,
            alt_launches: Vec::new(),
            icon: None,
            program: Some(PathBuf::from(r"C:\GOG\W3\bin\x64_dx12\witcher3.exe")),
            arch: None,
            edits: None,
            favorite: false,
        }
    }

    fn exe(path: &str, args: &str) -> Launch {
        Launch::Exe {
            path: PathBuf::from(path),
            args: args.into(),
            working_dir: None,
        }
    }

    #[test]
    fn fields_start_from_the_game() {
        let fields = EditFields::of(&game(exe(r"C:\GOG\W3\REDprelauncher.exe", "-x")));
        assert_eq!(fields.name, "The Witcher 3");
        assert_eq!(fields.launch, r"C:\GOG\W3\REDprelauncher.exe");
        assert_eq!(fields.args, "-x");
        assert_eq!(fields.program, r"C:\GOG\W3\bin\x64_dx12\witcher3.exe");

        // A store link: no launch program; the arguments are the user's Steam launch options.
        let mut steam = game(Launch::Uri("steam://run/292030//-x/".into()));
        steam.edits = Some(GameEdits {
            args: Some("-x".into()),
            ..GameEdits::default()
        });
        let fields = EditFields::of(&steam);
        assert_eq!((fields.launch.as_str(), fields.args.as_str()), ("", "-x"));
    }

    #[test]
    fn save_stores_only_changed_fields() {
        let witcher = game(exe(r"C:\GOG\W3\REDprelauncher.exe", "-x"));
        let mut draft = EditDraft::of(&witcher);
        assert!(!draft.is_dirty());
        assert_eq!(draft.edits(GameEdits::default()), GameEdits::default());

        // Changed name and arguments; an earlier program edit is kept.
        draft.fields.set(EditField::Name, "  W3  ".into());
        draft.fields.set(EditField::Args, "-skip ".into());
        assert!(draft.is_dirty());
        let earlier = GameEdits {
            program: Some(PathBuf::from(r"C:\GOG\W3\bin\x64\witcher3.exe")),
            ..GameEdits::default()
        };
        assert_eq!(
            draft.edits(earlier.clone()),
            GameEdits {
                name: Some("W3".into()),
                args: Some("-skip".into()),
                ..earlier
            }
        );

        // Blank name and paths go back to the detected values.
        let mut draft = EditDraft::of(&witcher);
        draft.fields.set(EditField::Name, " ".into());
        draft.fields.set(EditField::Program, String::new());
        let reset = draft.edits(GameEdits {
            name: Some("W3".into()),
            program: Some(PathBuf::from(r"C:\x.exe")),
            ..GameEdits::default()
        });
        assert_eq!((reset.name, reset.program), (None, None));
    }

    #[test]
    fn a_new_launch_program_keeps_the_arguments_shown() {
        let mut steam = game(Launch::Uri("steam://rungameid/292030".into()));
        steam.store = Store::Steam;
        let mut draft = EditDraft::of(&steam);
        draft.fields.set(
            EditField::Launch,
            r"C:\Steam\W3\bin\x64\witcher3.exe".into(),
        );
        let edits = draft.edits(GameEdits::default());
        assert_eq!(
            edits.launch,
            Some(PathBuf::from(r"C:\Steam\W3\bin\x64\witcher3.exe"))
        );
        assert_eq!(edits.args, Some(String::new()));
    }

    #[test]
    fn drop_down_items_map_to_choices() {
        let mut fields = EditFields::default();
        assert_eq!(fields.choice_index(EditChoice::Bitness), 0);
        assert_eq!(fields.choice_index(EditChoice::GraphicsApi), 0);
        fields.choose(EditChoice::Bitness, 1);
        fields.choose(EditChoice::GraphicsApi, 4);
        assert_eq!(fields.arch, Some(Arch::X86));
        assert_eq!(fields.graphics_api, Some(GraphicsApi::DirectX11));
        assert_eq!(fields.choice_index(EditChoice::Bitness), 1);
        assert_eq!(fields.choice_index(EditChoice::GraphicsApi), 4);
        assert_eq!(EditChoice::GraphicsApi.labels()[3], "DirectX 11");
        assert_eq!(
            EditChoice::Bitness.labels(),
            ["32-bit (x86)", "64-bit (x64)"]
        );

        // "As detected", and an index past the items, choose nothing.
        fields.choose(EditChoice::Bitness, 0);
        fields.choose(EditChoice::GraphicsApi, 99);
        assert_eq!((fields.arch, fields.graphics_api), (None, None));
    }

    #[test]
    fn save_stores_changed_choices() {
        let mut witcher = game(exe(r"C:\GOG\W3\REDprelauncher.exe", ""));
        let saved = GameEdits {
            graphics_api: Some(GraphicsApi::DirectX12),
            ..GameEdits::default()
        };
        witcher.edits = Some(saved.clone());
        let mut draft = EditDraft::of(&witcher);
        assert_eq!(draft.fields.graphics_api, Some(GraphicsApi::DirectX12));
        assert_eq!(draft.fields.arch, None);

        draft.fields.choose(EditChoice::Bitness, 2);
        assert!(draft.is_dirty());
        assert_eq!(
            draft.edits(saved.clone()),
            GameEdits {
                arch: Some(Arch::X64),
                ..saved.clone()
            }
        );
        // Back to "As detected".
        draft.fields.choose(EditChoice::GraphicsApi, 0);
        assert_eq!(draft.edits(saved).graphics_api, None);
    }

    #[test]
    fn a_draft_is_kept_only_for_the_same_game_as_it_was() {
        let witcher = game(exe(r"C:\GOG\W3\REDprelauncher.exe", ""));
        let mut draft = EditDraft::of(&witcher);
        draft.fields.set(EditField::Name, "W3".into());
        assert_eq!(EditDraft::for_game(Some(&draft), &witcher), draft);

        // A scan brought new values (e.g. after Save): start again from them.
        let mut renamed = witcher.clone();
        renamed.name = "W3".into();
        assert_eq!(
            EditDraft::for_game(Some(&draft), &renamed),
            EditDraft::of(&renamed)
        );
        let mut other = witcher;
        other.id = "gog:2".into();
        assert_eq!(
            EditDraft::for_game(Some(&draft), &other),
            EditDraft::of(&other)
        );
        assert_eq!(EditDraft::for_game(None, &other), EditDraft::of(&other));
    }
}
