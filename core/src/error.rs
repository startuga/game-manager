//! What can go wrong in `gm-core`: one error enum for the crate and a `Result` alias. Callers can
//! match on what failed; `Display` gives the text the app shows.

use crate::Store;
use crate::vdf::VdfError;
use std::fmt;
use std::path::PathBuf;
use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    /// A file or folder could not be read or written.
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A JSON file does not hold the expected data, or the data could not be written as JSON.
    #[error("{}: {source}", .path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// A Valve KeyValues (`.vdf`) file could not be parsed.
    #[error("{}: {source}", .path.display())]
    Vdf { path: PathBuf, source: VdfError },
    /// A Windows API failed; `context` names what it was working on (a file, a registry key).
    #[error("{context}: {source}")]
    Windows {
        context: String,
        source: windows::core::Error,
    },
    /// A web request was answered with something other than `200 OK`.
    #[error("{url}: {}", http_status(.status))]
    Http { url: String, status: u32 },
    /// A web answer was not the JSON expected.
    #[error("{url}: {source}")]
    WebJson {
        url: String,
        source: serde_json::Error,
    },
    /// A web answer was far larger than expected.
    #[error("{url}: the answer is too large")]
    ResponseTooLarge { url: String },
    /// The folder for the app's files is unknown: this environment variable is not set.
    #[error("{0} is not set")]
    MissingEnvVar(&'static str),
    /// A file or folder that has to exist does not.
    #[error("{} does not exist", .0.display())]
    NotFound(PathBuf),
    /// The Windows shell could not start or open `target`; `code` is what `ShellExecuteW` returned.
    #[error("failed to {action} {target} (code {code})")]
    Shell {
        action: &'static str,
        target: String,
        code: usize,
    },
    /// A game added by hand needs a name.
    #[error("a game needs a name")]
    MissingName,
    /// None of the dropped files is a program or a shortcut.
    #[error("Drop a game's .exe or its shortcut to add it.")]
    NothingToAdd,
    /// More than one program or shortcut was dropped.
    #[error("Drop one game at a time.")]
    SeveralGames,
    /// A dropped shortcut starts a file that is no longer there.
    #[error("The shortcut {name} starts {}, which does not exist.", .target.display())]
    ShortcutTargetMissing { name: String, target: PathBuf },
    /// A dropped shortcut starts something other than a program (a Store app, a folder).
    #[error(
        "The shortcut {name} does not start a program. Steam, Epic, GOG, EA and Xbox games are \
         found automatically."
    )]
    ShortcutNotAProgram { name: String },
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }

    pub(crate) fn json(path: impl Into<PathBuf>) -> impl FnOnce(serde_json::Error) -> Self {
        let path = path.into();
        move |source| Self::Json { path, source }
    }

    pub(crate) fn windows(context: impl fmt::Display) -> impl FnOnce(windows::core::Error) -> Self {
        let context = context.to_string();
        move |source| Self::Windows { context, source }
    }
}

fn http_status(status: &u32) -> String {
    match status {
        429 => "the server is busy (HTTP 429); try again in a minute".to_owned(),
        status => format!("HTTP {status}"),
    }
}

/// The part of a scan that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanPart {
    Store(Store),
    /// The shipped `games.json`.
    GameConfiguration,
    /// The user's `game-settings.json`.
    GameSettings,
}

impl fmt::Display for ScanPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ScanPart::Store(store) => store.name(),
            ScanPart::GameConfiguration => "Game configuration",
            ScanPart::GameSettings => "Game settings",
        })
    }
}

/// A part of `scan_all` that failed. The other parts' games are still listed.
#[derive(Debug, Error)]
#[error("{part}: {error}")]
pub struct ScanError {
    pub part: ScanPart,
    #[source]
    pub error: Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_what_failed() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let io = Error::io(r"C:\x.json")(missing);
        assert!(io.to_string().starts_with(r"C:\x.json: "), "{io}");

        let scan = ScanError {
            part: ScanPart::Store(Store::Steam),
            error: Error::NotFound(PathBuf::from(r"C:\Steam")),
        };
        assert_eq!(scan.to_string(), r"Steam: C:\Steam does not exist");
        assert_eq!(
            ScanError {
                part: ScanPart::GameSettings,
                error: Error::MissingEnvVar("APPDATA"),
            }
            .to_string(),
            "Game settings: APPDATA is not set"
        );
    }
}
