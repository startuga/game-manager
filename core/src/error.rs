//! What can go wrong in `gm-core`: one error enum for the crate and a `Result` alias. Callers can
//! match on what failed; `Display` gives the text the app shows.

use crate::Store;
use crate::vdf::VdfError;
use std::fmt;
use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub enum Error {
    /// A file or folder could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A JSON file does not hold the expected data, or the data could not be written as JSON.
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// A Valve KeyValues (`.vdf`) file could not be parsed.
    Vdf { path: PathBuf, source: VdfError },
    /// A Windows API failed; `context` names what it was working on (a file, a registry key).
    Windows {
        context: String,
        source: windows::core::Error,
    },
    /// A web request was answered with something other than `200 OK`.
    Http { url: String, status: u32 },
    /// A web answer was not the JSON expected.
    WebJson {
        url: String,
        source: serde_json::Error,
    },
    /// A web answer was far larger than expected.
    ResponseTooLarge { url: String },
    /// An address that is not `https://host/…`.
    InvalidUrl(String),
    /// A program that ships next to Game Manager is missing.
    ToolMissing(PathBuf),
    /// 7-Zip could not unpack an archive; `code` is its exit code.
    Extract { archive: PathBuf, code: i32 },
    /// 7-Zip could not pack a folder into an archive; `code` is its exit code.
    Pack {
        folder: PathBuf,
        archive: PathBuf,
        code: i32,
    },
    /// An archive lacks what had to be in it (`what`: "TAR archive").
    NotInArchive {
        archive: PathBuf,
        what: &'static str,
    },
    /// Work ran longer than it may and was stopped (`what`: "7-Zip unpacking …").
    TookTooLong { what: String, minutes: u64 },
    /// A file is larger than what reads it takes (`limit_mb`).
    FileTooLarge { path: PathBuf, limit_mb: u64 },
    /// A download the cache doesn't hold, by its kind and version as asked for.
    NotADownload { kind: String, version: String },
    /// Windows gave text that isn't Unicode for `what`.
    UnreadableText { what: String },
    /// The user said No when Windows asked for administrator rights.
    ElevationRefused,
    /// The copy of Game Manager running as administrator failed; `code` is its exit code, the
    /// HRESULT of what failed.
    Elevated { code: u32 },
    /// Windows refused a display mode; `code` is what `ChangeDisplaySettingsExW` answered.
    DisplayModeRefused {
        display: String,
        width: u32,
        height: u32,
        hz: u32,
        code: i32,
    },
    /// A display mode was saved, but Windows needs a restart to use it.
    DisplayModeNeedsRestart(String),
    /// A display did not take a change; `what` names it ("brightness").
    DisplayRefused { display: String, what: &'static str },
    /// A display a game's settings name isn't connected (or is off).
    DisplayNotConnected(String),
    /// Windows didn't say where the Documents folder is (save backups go there).
    NoDocumentsFolder,
    /// Windows didn't say where the desktop is (shortcuts go there).
    NoDesktopFolder,
    /// A folder to back up holds no file.
    EmptyFolder(PathBuf),
    /// A folder to back up is larger than the limit.
    FolderTooLarge { folder: PathBuf, limit_gb: u64 },
    /// Something an earlier step left is where Game Manager must put a folder.
    InTheWay(PathBuf),
    /// A release list or build list answered with nothing to download.
    NoRelease { url: String },
    /// GitHub lists no SHA-256 for a development build, so it can't be checked and isn't
    /// downloaded.
    NoDigest { url: String },
    /// A download is not the file GitHub lists: its SHA-256 differs, so it isn't used.
    DigestMismatch { url: String },
    /// A downloaded mod has no build for Windows in it.
    NoModBuild(PathBuf),
    /// The mod cannot be installed in this game; the text says why.
    NotInstallable(String),
    /// A job did and saved its work, but left some of it out; the text names what.
    Incomplete(String),
    /// An NVIDIA driver settings call failed; `status` is NVAPI's.
    Nvapi { action: &'static str, status: i32 },
    /// The folder for the app's files is unknown: this environment variable is not set.
    MissingEnvVar(&'static str),
    /// A file or folder that has to exist does not.
    NotFound(PathBuf),
    /// The Windows shell could not start or open `target`; `code` is what `ShellExecuteW` returned.
    Shell {
        action: &'static str,
        target: String,
        code: usize,
    },
    /// A game added by hand needs a name.
    MissingName,
    /// None of the dropped files is a program or a shortcut.
    NothingToAdd,
    /// More than one program or shortcut was dropped.
    SeveralGames,
    /// A dropped shortcut starts a file that is no longer there.
    ShortcutTargetMissing { name: String, target: PathBuf },
    /// A dropped shortcut starts something other than a program (a Store app, a folder).
    ShortcutNotAProgram { name: String },
    /// A file chosen as a game's cover is not a picture Windows can read.
    NotAPicture(PathBuf),
    /// A secret too long for Credential Manager (`credentials.rs`).
    SecretTooLong { target: String, limit: usize },
    /// SteamGridDB has no API key here: Settings ▸ SteamGridDB asks for one.
    NoSteamGridDbKey,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Json { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Vdf { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Windows { context, source } => write!(f, "{context}: {source}"),
            Self::Http { url, status } => write!(f, "{url}: {}", http_status(status)),
            Self::WebJson { url, source } => write!(f, "{url}: {source}"),
            Self::ResponseTooLarge { url } => write!(f, "{url}: the answer is too large"),
            Self::InvalidUrl(url) => write!(f, "{url} is not an https address"),
            Self::ToolMissing(path) => write!(
                f,
                "{} is missing: it ships in Game Manager's folder",
                path.display()
            ),
            Self::Extract { archive, code } => {
                write!(
                    f,
                    "7-Zip could not unpack {} (exit code {code})",
                    archive.display()
                )
            }
            Self::Pack {
                folder,
                archive,
                code,
            } => write!(
                f,
                "7-Zip could not pack {} into {} (exit code {code})",
                folder.display(),
                archive.display()
            ),
            Self::NotInArchive { archive, what } => {
                write!(f, "{} holds no {what}", archive.display())
            }
            Self::TookTooLong { what, minutes } => {
                write!(f, "{what} took over {minutes} minutes and was stopped")
            }
            Self::FileTooLarge { path, limit_mb } => {
                write!(f, "{} is larger than {limit_mb} MB", path.display())
            }
            Self::NotADownload { kind, version } => {
                write!(f, "{kind} {version} is not a download in Game Manager's cache")
            }
            Self::UnreadableText { what } => {
                write!(f, "Windows gave unreadable text for {what}")
            }
            Self::ElevationRefused => {
                f.write_str("Administrator rights were refused, so nothing was changed.")
            }
            Self::Elevated { code } => write!(
                f,
                "The change made as administrator failed: {}",
                hresult_message(code)
            ),
            Self::DisplayModeRefused {
                display,
                width,
                height,
                hz,
                code,
            } => write!(
                f,
                "Windows refused {width} × {height} at {hz} Hz on {display} (code {code})."
            ),
            Self::DisplayModeNeedsRestart(display) => {
                write!(f, "Windows needs a restart to use the new mode on {display}.")
            }
            Self::DisplayRefused { display, what } => {
                write!(f, "{display} did not take the new {what}.")
            }
            Self::DisplayNotConnected(display) => write!(f, "{display} isn't connected."),
            Self::NoDocumentsFolder => f.write_str(
                "Windows didn't say where your Documents folder is, so save backups have nowhere to go.",
            ),
            Self::NoDesktopFolder => f.write_str(
                "Windows didn't say where your desktop is, so the shortcut has nowhere to go.",
            ),
            Self::EmptyFolder(path) => {
                write!(f, "{} is empty: there is nothing to back up.", path.display())
            }
            Self::FolderTooLarge { folder, limit_gb } => write!(
                f,
                "{} is larger than {limit_gb} GB, too large to back up.",
                folder.display()
            ),
            Self::InTheWay(path) => write!(
                f,
                "{} is in the way: move or delete it, then try again.",
                path.display()
            ),
            Self::NoRelease { url } => write!(f, "{url} lists no build to download"),
            Self::NoDigest { url } => {
                write!(f, "GitHub lists no SHA-256 for {url}, so it was not downloaded")
            }
            Self::DigestMismatch { url } => write!(
                f,
                "{url} did not download as GitHub lists it (its SHA-256 differs), so it was not used"
            ),
            Self::NoModBuild(path) => {
                write!(f, "{} holds no 32 or 64-bit Windows build", path.display())
            }
            Self::NotInstallable(message) | Self::Incomplete(message) => f.write_str(message),
            Self::Nvapi { action, status } => write!(
                f,
                "NVIDIA driver settings: could not {action}: {} (NVAPI {status})",
                crate::nvidia::status_text(*status)
            ),
            Self::MissingEnvVar(name) => write!(f, "{name} is not set"),
            Self::NotFound(path) => write!(f, "{} does not exist", path.display()),
            Self::Shell {
                action,
                target,
                code,
            } => write!(f, "failed to {action} {target} (code {code})"),
            Self::MissingName => f.write_str("a game needs a name"),
            Self::NothingToAdd => f.write_str("Drop a game's .exe or its shortcut to add it."),
            Self::SeveralGames => f.write_str("Drop one game at a time."),
            Self::ShortcutTargetMissing { name, target } => write!(
                f,
                "The shortcut {name} starts {}, which does not exist.",
                target.display()
            ),
            Self::ShortcutNotAProgram { name } => write!(
                f,
                "The shortcut {name} does not start a program. Steam, Epic, GOG, EA and Xbox games are found automatically."
            ),
            Self::NotAPicture(path) => {
                write!(f, "{} is not a picture Windows can open.", path.display())
            }
            Self::SecretTooLong { target, limit } => {
                write!(f, "{target}: a secret of more than {limit} bytes can't be kept")
            }
            Self::NoSteamGridDbKey => {
                f.write_str("SteamGridDB needs your API key: add it in Settings")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } | Self::WebJson { source, .. } => Some(source),
            Self::Vdf { source, .. } => Some(source),
            Self::Windows { source, .. } => Some(source),
            _ => None,
        }
    }
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

fn hresult_message(code: &u32) -> String {
    windows::core::HRESULT(*code as i32).message()
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
#[derive(Debug)]
pub struct ScanError {
    pub part: ScanPart,
    pub error: Error,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.part, self.error)
    }
}

impl std::error::Error for ScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
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
