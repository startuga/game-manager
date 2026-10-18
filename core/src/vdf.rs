//! Minimal parsers for Valve's KeyValues formats: text (`.vdf` / `.acf`) and binary (the entries
//! of Steam's `appinfo.vdf`).

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Looks up a child by key. VDF keys are case-insensitive.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Value::Str(_) => None,
        }
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(entries) => entries,
            Value::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct VdfError {
    pub offset: usize,
    pub message: &'static str,
}

impl fmt::Display for VdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "VDF parse error at byte {}: {}",
            self.offset, self.message
        )
    }
}

impl std::error::Error for VdfError {}

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn error(&self, message: &'static str) -> VdfError {
        VdfError {
            offset: self.pos,
            message,
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            let rest = &self.src[self.pos..];
            let trimmed = rest.trim_start();
            self.pos += rest.len() - trimmed.len();
            if trimmed.starts_with("//") {
                self.pos += trimmed.find('\n').unwrap_or(trimmed.len());
            } else if trimmed.starts_with('[') {
                // Platform conditionals such as [$WIN32] are ignored.
                self.pos += trimmed.find(']').map_or(trimmed.len(), |i| i + 1);
            } else {
                return;
            }
        }
    }

    fn next(&mut self) -> Result<Option<Token>, VdfError> {
        self.skip_trivia();
        let mut chars = self.src[self.pos..].chars();
        let Some(c) = chars.next() else {
            return Ok(None);
        };
        match c {
            '{' => {
                self.pos += 1;
                Ok(Some(Token::Open))
            }
            '}' => {
                self.pos += 1;
                Ok(Some(Token::Close))
            }
            '"' => {
                self.pos += 1;
                let mut out = String::new();
                let mut chars = self.src[self.pos..].char_indices();
                while let Some((i, c)) = chars.next() {
                    match c {
                        '"' => {
                            self.pos += i + 1;
                            return Ok(Some(Token::Str(out)));
                        }
                        '\\' => match chars.next() {
                            Some((_, 'n')) => out.push('\n'),
                            Some((_, 't')) => out.push('\t'),
                            Some((_, other)) => out.push(other),
                            None => break,
                        },
                        _ => out.push(c),
                    }
                }
                Err(self.error("unterminated string"))
            }
            _ => {
                let rest = &self.src[self.pos..];
                let end = rest
                    .find(|c: char| c.is_whitespace() || matches!(c, '{' | '}' | '"'))
                    .unwrap_or(rest.len());
                self.pos += end;
                Ok(Some(Token::Str(rest[..end].to_owned())))
            }
        }
    }
}

/// Parses a VDF document into its root object.
pub fn parse(src: &str) -> Result<Value, VdfError> {
    let mut lexer = Lexer {
        src: src.strip_prefix('\u{feff}').unwrap_or(src),
        pos: 0,
    };
    let entries = parse_entries(&mut lexer, false)?;
    Ok(Value::Obj(entries))
}

fn parse_entries(lexer: &mut Lexer, nested: bool) -> Result<Vec<(String, Value)>, VdfError> {
    let mut entries = Vec::new();
    loop {
        let key = match lexer.next()? {
            Some(Token::Str(key)) => key,
            Some(Token::Close) if nested => return Ok(entries),
            None if !nested => return Ok(entries),
            None => return Err(lexer.error("unexpected end of input")),
            Some(_) => return Err(lexer.error("expected key")),
        };
        let value = match lexer.next()? {
            Some(Token::Str(s)) => Value::Str(s),
            Some(Token::Open) => Value::Obj(parse_entries(lexer, true)?),
            _ => return Err(lexer.error("expected value")),
        };
        entries.push((key, value));
    }
}

/// Where the keys of binary KeyValues come from.
#[derive(Clone, Copy)]
pub enum BinaryKeys<'a> {
    /// Null-terminated strings in place (`appinfo.vdf` up to version 28).
    Inline,
    /// `u32` indexes into a string table stored elsewhere (`appinfo.vdf` version 29).
    Table(&'a [String]),
}

/// Parses binary KeyValues into a root object. Numbers are kept as their decimal text, so the
/// result reads like text VDF.
pub fn parse_binary(data: &[u8], keys: BinaryKeys) -> Result<Value, VdfError> {
    let mut reader = BinaryReader { data, pos: 0, keys };
    Ok(Value::Obj(reader.entries(false)?))
}

const BINARY_OBJECT: u8 = 0x00;
const BINARY_STRING: u8 = 0x01;
const BINARY_INT32: u8 = 0x02;
const BINARY_FLOAT32: u8 = 0x03;
const BINARY_POINTER: u8 = 0x04;
const BINARY_COLOR: u8 = 0x06;
const BINARY_UINT64: u8 = 0x07;
const BINARY_END: u8 = 0x08;
const BINARY_INT64: u8 = 0x0A;
const BINARY_ALT_END: u8 = 0x0B;

struct BinaryReader<'a> {
    data: &'a [u8],
    pos: usize,
    keys: BinaryKeys<'a>,
}

impl BinaryReader<'_> {
    fn entries(&mut self, nested: bool) -> Result<Vec<(String, Value)>, VdfError> {
        let mut entries = Vec::new();
        loop {
            let Some(&kind) = self.data.get(self.pos) else {
                return if nested {
                    Err(self.error("unexpected end of input"))
                } else {
                    Ok(entries)
                };
            };
            self.pos += 1;
            if kind == BINARY_END || kind == BINARY_ALT_END {
                return Ok(entries);
            }
            let key = self.key()?;
            let value = match kind {
                BINARY_OBJECT => Value::Obj(self.entries(true)?),
                BINARY_STRING => Value::Str(self.c_string()?),
                BINARY_INT32 => Value::Str(i32::from_le_bytes(self.bytes()?).to_string()),
                BINARY_FLOAT32 => Value::Str(f32::from_le_bytes(self.bytes()?).to_string()),
                BINARY_POINTER | BINARY_COLOR => {
                    Value::Str(u32::from_le_bytes(self.bytes()?).to_string())
                }
                BINARY_UINT64 => Value::Str(u64::from_le_bytes(self.bytes()?).to_string()),
                BINARY_INT64 => Value::Str(i64::from_le_bytes(self.bytes()?).to_string()),
                _ => return Err(self.error("unknown binary value type")),
            };
            entries.push((key, value));
        }
    }

    fn key(&mut self) -> Result<String, VdfError> {
        match self.keys {
            BinaryKeys::Inline => self.c_string(),
            BinaryKeys::Table(table) => {
                let index = u32::from_le_bytes(self.bytes()?) as usize;
                table
                    .get(index)
                    .cloned()
                    .ok_or_else(|| self.error("key index outside the string table"))
            }
        }
    }

    fn c_string(&mut self) -> Result<String, VdfError> {
        let rest = &self.data[self.pos..];
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| self.error("unterminated string"))?;
        self.pos += len + 1;
        Ok(String::from_utf8_lossy(&rest[..len]).into_owned())
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], VdfError> {
        let bytes = self
            .data
            .get(self.pos..self.pos + N)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| self.error("unexpected end of input"))?;
        self.pos += N;
        Ok(bytes)
    }

    fn error(&self, message: &'static str) -> VdfError {
        VdfError {
            offset: self.pos,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binary KeyValues for `appinfo { appid 620980; config { launch { 0 { executable … } } } }`,
    /// with each key written by `key`.
    fn binary_app(key: impl Fn(&mut Vec<u8>, &str)) -> Vec<u8> {
        let mut data = Vec::new();
        let open = |data: &mut Vec<u8>, name: &str| {
            data.push(BINARY_OBJECT);
            key(data, name);
        };
        open(&mut data, "appinfo");
        data.push(BINARY_INT32);
        key(&mut data, "appid");
        data.extend(620980i32.to_le_bytes());
        open(&mut data, "config");
        open(&mut data, "launch");
        open(&mut data, "0");
        data.push(BINARY_STRING);
        key(&mut data, "executable");
        data.extend(b"Beat Saber.exe\0");
        data.push(BINARY_UINT64);
        key(&mut data, "size");
        data.extend(5_000_000_000u64.to_le_bytes());
        data.extend([BINARY_END; 5]);
        data
    }

    fn check_app(root: &Value) {
        let app = root.get("appinfo").unwrap();
        assert_eq!(app.get_str("appid"), Some("620980"));
        let launch = app
            .get("config")
            .unwrap()
            .get("launch")
            .unwrap()
            .get("0")
            .unwrap();
        assert_eq!(launch.get_str("executable"), Some("Beat Saber.exe"));
        assert_eq!(launch.get_str("size"), Some("5000000000"));
    }

    #[test]
    fn parses_binary_with_inline_keys() {
        let data = binary_app(|data, name| {
            data.extend(name.as_bytes());
            data.push(0);
        });
        check_app(&parse_binary(&data, BinaryKeys::Inline).unwrap());
    }

    #[test]
    fn parses_binary_with_a_string_table() {
        let table: Vec<String> = [
            "appinfo",
            "appid",
            "config",
            "launch",
            "0",
            "executable",
            "size",
        ]
        .map(String::from)
        .to_vec();
        let data = binary_app(|data, name| {
            let index = table.iter().position(|k| k == name).unwrap() as u32;
            data.extend(index.to_le_bytes());
        });
        check_app(&parse_binary(&data, BinaryKeys::Table(&table)).unwrap());
    }

    #[test]
    fn rejects_broken_binary() {
        let inline = |data: &mut Vec<u8>, name: &str| {
            data.extend(name.as_bytes());
            data.push(0);
        };
        let mut truncated = binary_app(inline);
        truncated.truncate(truncated.len() - 3);
        assert!(parse_binary(&truncated, BinaryKeys::Inline).is_err());
        assert!(parse_binary(&[0x05, b'k', 0, 0], BinaryKeys::Inline).is_err());
        assert!(parse_binary(&[BINARY_STRING, 7, 0, 0, 0], BinaryKeys::Table(&[])).is_err());
    }

    #[test]
    fn parses_library_folders() {
        let src = r#"
"libraryfolders"
{
    // comment
    "0"
    {
        "path"      "C:\\Program Files (x86)\\Steam"
        "apps"
        {
            "228980"    "123"
        }
    }
    "1" { "path" "D:\\SteamLibrary" }
}
"#;
        let root = parse(src).unwrap();
        let folders = root.get("LibraryFolders").unwrap();
        assert_eq!(folders.entries().len(), 2);
        assert_eq!(
            folders.get("0").unwrap().get_str("path"),
            Some(r"C:\Program Files (x86)\Steam")
        );
        assert_eq!(
            folders.get("1").unwrap().get_str("path"),
            Some(r"D:\SteamLibrary")
        );
        assert_eq!(
            folders
                .get("0")
                .unwrap()
                .get("apps")
                .unwrap()
                .get_str("228980"),
            Some("123")
        );
    }

    #[test]
    fn reports_unterminated_string() {
        assert!(parse(r#""key" "value"#).is_err());
    }

    #[test]
    fn reports_unclosed_object() {
        assert!(parse(r#""a" { "b" "c""#).is_err());
    }
}
