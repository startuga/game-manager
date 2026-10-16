//! Minimal parser for Valve's text KeyValues format (`.vdf` / `.acf`).

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

#[cfg(test)]
mod tests {
    use super::*;

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
