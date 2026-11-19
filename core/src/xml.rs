//! Minimal read-only XML parser for store manifests (EA's `installerdata.xml` and GDF resources,
//! Xbox's `MicrosoftGame.config` and `AppxManifest.xml`).
//!
//! Stores all elements and attributes in two flat arenas so parsing does no per-node heap
//! allocations and borrows text and attribute values directly from the input whenever no entity
//! decoding is needed. Supports namespaces (matched by local tag name), comments, processing
//! instructions, CDATA, DOCTYPE, and standard/numeric XML character references while rejecting
//! malformed, unclosed, or overly nested documents.

use std::borrow::Cow;

/// Maximum element nesting depth accepted; prevents pathological stack/path depth on hostile input.
const MAX_DEPTH: usize = 256;

/// A parsed XML document backed by flat arenas.
pub(crate) struct Document<'a> {
    elements: Vec<Element<'a>>,
    attrs: Vec<(&'a str, Cow<'a, str>)>,
}

struct Element<'a> {
    /// Local name (after `:` when namespace-prefixed).
    name: &'a str,
    /// Full qualified name as written in the opening tag (for matching the closing tag).
    raw_name: &'a str,
    parent: Option<usize>,
    first_child: Option<usize>,
    last_child: Option<usize>,
    next_sibling: Option<usize>,
    attrs_start: u32,
    attrs_end: u32,
    text: Option<Cow<'a, str>>,
}

/// A lightweight handle to an element inside a `Document`.
#[derive(Clone, Copy)]
pub(crate) struct Node<'a, 'd> {
    doc: &'d Document<'a>,
    id: usize,
}

impl<'a> Document<'a> {
    /// Parses a UTF-8 XML document (with optional BOM and `<?xml ...?>` declaration).
    /// Returns `None` if the document is empty, malformed, unclosed, or exceeds `MAX_DEPTH`.
    pub(crate) fn parse(input: &'a str) -> Option<Self> {
        let src = input.trim_prefix('\u{feff}');
        let mut parser = Parser {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            elements: Vec::new(),
            attrs: Vec::new(),
            stack: Vec::new(),
            root_seen: false,
        };
        parser.parse_doc()?;
        if !parser.root_seen || !parser.stack.is_empty() || parser.elements.is_empty() {
            return None;
        }
        Some(Self {
            elements: parser.elements,
            attrs: parser.attrs,
        })
    }

    /// The document's single root element.
    pub(crate) fn root_element(&self) -> Node<'a, '_> {
        Node { doc: self, id: 0 }
    }

    /// Every element in the document, in pre-order (document order).
    pub(crate) fn descendants(&self) -> impl Iterator<Item = Node<'a, '_>> {
        (0..self.elements.len()).map(|id| Node { doc: self, id })
    }
}

impl<'a, 'd> Node<'a, 'd> {
    /// Local tag name (namespace prefix stripped).
    pub(crate) fn tag_name(self) -> &'a str {
        self.doc.elements[self.id].name
    }

    /// Whether the element's local tag name equals `name`.
    pub(crate) fn has_tag_name(self, name: &str) -> bool {
        self.tag_name() == name
    }

    /// Parent element, or `None` for the root element.
    pub(crate) fn parent(self) -> Option<Node<'a, 'd>> {
        self.doc.elements[self.id]
            .parent
            .map(|id| Node { doc: self.doc, id })
    }

    /// Direct child elements, in document order.
    pub(crate) fn children(self) -> impl Iterator<Item = Node<'a, 'd>> {
        let doc = self.doc;
        let mut next = doc.elements[self.id].first_child;
        std::iter::from_fn(move || {
            let id = next?;
            next = doc.elements[id].next_sibling;
            Some(Node { doc, id })
        })
    }

    /// Value of the attribute named `name` (exact or local name).
    pub(crate) fn attribute(self, name: &str) -> Option<&'d str> {
        let el = &self.doc.elements[self.id];
        let slice = &self.doc.attrs[el.attrs_start as usize..el.attrs_end as usize];
        slice
            .iter()
            .find(|(k, _)| *k == name || local_name(k) == name)
            .map(|(_, v)| v.as_ref())
    }

    /// Direct text content of this element (with entities decoded and CDATA included).
    pub(crate) fn text(self) -> Option<&'d str> {
        self.doc.elements[self.id].text.as_deref()
    }

    /// Trimmed, non-empty text of the first child element named `name`.
    pub(crate) fn child_text(self, name: &str) -> Option<&'d str> {
        self.children()
            .find(|c| c.has_tag_name(name))?
            .text()
            .map(str::trim)
            .filter(|t| !t.is_empty())
    }
}

fn local_name(raw: &str) -> &str {
    raw.rsplit_once(':').map_or(raw, |(_, local)| local)
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    elements: Vec<Element<'a>>,
    attrs: Vec<(&'a str, Cow<'a, str>)>,
    stack: Vec<usize>,
    root_seen: bool,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_doc(&mut self) -> Option<()> {
        while self.pos < self.bytes.len() {
            if self.stack.is_empty() {
                self.skip_ws();
                if self.pos >= self.bytes.len() {
                    break;
                }
                if self.peek() != Some(b'<') {
                    // Non-whitespace text outside the root element is invalid XML.
                    return None;
                }
            }
            if self.peek() == Some(b'<') {
                self.parse_markup()?;
            } else {
                self.parse_text()?;
            }
        }
        Some(())
    }

    fn parse_markup(&mut self) -> Option<()> {
        let rest = self.rest();
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after.find("-->")?;
            self.pos += 4 + end + 3;
            return Some(());
        }
        if let Some(after) = rest.strip_prefix("<?") {
            let end = after.find("?>")?;
            self.pos += 2 + end + 2;
            return Some(());
        }
        if let Some(after) = rest.strip_prefix("<![CDATA[") {
            if self.stack.is_empty() {
                return None;
            }
            let end = after.find("]]>")?;
            let cdata = &after[..end];
            self.pos += 9 + end + 3;
            if let Some(&parent) = self.stack.last() {
                push_text(&mut self.elements[parent].text, Cow::Borrowed(cdata));
            }
            return Some(());
        }
        if rest.starts_with("<!DOCTYPE") || rest.starts_with("<!doctype") {
            if !self.stack.is_empty() {
                return None;
            }
            self.skip_doctype()?;
            return Some(());
        }
        if rest.starts_with("</") {
            self.pos += 2;
            let raw_name = self.read_name()?;
            self.skip_ws();
            if self.peek() != Some(b'>') {
                return None;
            }
            self.pos += 1;
            let open_id = self.stack.pop()?;
            if self.elements[open_id].raw_name != raw_name {
                return None;
            }
            return Some(());
        }
        // Opening or self-closing tag.
        if self.stack.is_empty() && self.root_seen {
            // Multiple root elements are invalid XML.
            return None;
        }
        if self.stack.len() >= MAX_DEPTH {
            return None;
        }
        self.pos += 1; // '<'
        let raw_name = self.read_name()?;
        let attrs_start = self.attrs.len() as u32;
        let self_closing = self.parse_attributes()?;
        let attrs_end = self.attrs.len() as u32;

        let parent = self.stack.last().copied();
        let id = self.elements.len();
        if let Some(parent_id) = parent {
            let prev = self.elements[parent_id].last_child;
            match prev {
                Some(prev_id) => self.elements[prev_id].next_sibling = Some(id),
                None => self.elements[parent_id].first_child = Some(id),
            }
            self.elements[parent_id].last_child = Some(id);
        } else {
            self.root_seen = true;
        }
        self.elements.push(Element {
            name: local_name(raw_name),
            raw_name,
            parent,
            first_child: None,
            last_child: None,
            next_sibling: None,
            attrs_start,
            attrs_end,
            text: None,
        });
        if !self_closing {
            self.stack.push(id);
        }
        Some(())
    }

    fn parse_attributes(&mut self) -> Option<bool> {
        loop {
            let before_ws = self.pos;
            self.skip_ws();
            match self.peek()? {
                b'>' => {
                    self.pos += 1;
                    return Some(false);
                }
                b'/' => {
                    self.pos += 1;
                    if self.peek() != Some(b'>') {
                        return None;
                    }
                    self.pos += 1;
                    return Some(true);
                }
                _ => {
                    // Attributes must be separated from the tag name or previous attribute by whitespace.
                    if self.pos == before_ws {
                        return None;
                    }
                    let key = self.read_name()?;
                    self.skip_ws();
                    if self.peek() != Some(b'=') {
                        return None;
                    }
                    self.pos += 1;
                    self.skip_ws();
                    let quote = match self.peek()? {
                        q @ (b'"' | b'\'') => q,
                        _ => return None,
                    };
                    self.pos += 1;
                    let start = self.pos;
                    let rel_end = self.rest().as_bytes().iter().position(|&b| b == quote)?;
                    let raw = &self.src[start..start + rel_end];
                    self.pos = start + rel_end + 1;
                    let val = unescape(raw, true)?;
                    self.attrs.push((key, val));
                }
            }
        }
    }

    fn parse_text(&mut self) -> Option<()> {
        let start = self.pos;
        let rel_end = self
            .rest()
            .as_bytes()
            .iter()
            .position(|&b| b == b'<')
            .unwrap_or(self.bytes.len() - start);
        let raw = &self.src[start..start + rel_end];
        self.pos = start + rel_end;
        let decoded = unescape(raw, false)?;
        if let Some(&parent) = self.stack.last() {
            push_text(&mut self.elements[parent].text, decoded);
        }
        Some(())
    }

    fn read_name(&mut self) -> Option<&'a str> {
        let start = self.pos;
        let first = *self.bytes.get(start)?;
        if !(first.is_ascii_alphabetic() || first == b'_' || first == b':' || first >= 0x80) {
            return None;
        }
        self.pos += 1;
        while let Some(&b) = self.bytes.get(self.pos) {
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':') || b >= 0x80 {
                self.pos += 1;
            } else {
                break;
            }
        }
        Some(&self.src[start..self.pos])
    }

    fn skip_doctype(&mut self) -> Option<()> {
        let mut depth = 0usize;
        let mut quote: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            if let Some(q) = quote {
                if b == q {
                    quote = None;
                }
            } else {
                match b {
                    b'"' | b'\'' => quote = Some(b),
                    b'[' => depth += 1,
                    b']' => depth = depth.checked_sub(1)?,
                    b'>' if depth == 0 => return Some(()),
                    _ => {}
                }
            }
        }
        None
    }
}

fn push_text<'a>(slot: &mut Option<Cow<'a, str>>, chunk: Cow<'a, str>) {
    if chunk.is_empty() {
        return;
    }
    match slot {
        None => *slot = Some(chunk),
        Some(existing) => existing.to_mut().push_str(&chunk),
    }
}

/// Decodes standard XML entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`) and numeric
/// character references (`&#...;`, `&#x...;`), borrowing `raw` without allocation when no `&`
/// (or attribute whitespace normalization) is present.
fn unescape(raw: &str, attr: bool) -> Option<Cow<'_, str>> {
    let needs_work = if attr {
        raw.bytes()
            .any(|b| matches!(b, b'&' | b'\r' | b'\n' | b'\t'))
    } else {
        raw.as_bytes().contains(&b'&')
    };
    if !needs_work {
        return Some(Cow::Borrowed(raw));
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while !rest.is_empty() {
        let next = rest
            .bytes()
            .position(|b| b == b'&' || (attr && matches!(b, b'\r' | b'\n' | b'\t')));
        let Some(idx) = next else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..idx]);
        let bytes = rest.as_bytes();
        if bytes[idx] != b'&' {
            // XML 1.0 attribute-value whitespace normalization (`\r\n` counts as one space).
            out.push(' ');
            if bytes[idx] == b'\r' && bytes.get(idx + 1) == Some(&b'\n') {
                rest = &rest[idx + 2..];
            } else {
                rest = &rest[idx + 1..];
            }
            continue;
        }
        let after_amp = &rest[idx + 1..];
        let semi = after_amp.find(';')?;
        let entity = &after_amp[..semi];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let code = if let Some(hex) = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                {
                    u32::from_str_radix(hex, 16).ok()?
                } else {
                    let dec = entity.strip_prefix('#')?;
                    dec.parse::<u32>().ok()?
                };
                out.push(char::from_u32(code)?);
            }
        }
        rest = &after_amp[semi + 1..];
    }
    Some(Cow::Owned(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MANIFEST: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- comment -->
<Package xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10">
  <Identity Name="Foo &amp; Bar" Version="1.5.0.0" />
  <Properties>
    <DisplayName>Tom &amp; Jerry&#39;s &#x2605; Game</DisplayName>
    <Notes><![CDATA[<raw & literal>]]></Notes>
  </Properties>
  <Applications>
    <Application Id="Game" Executable="Game.exe">
      <uap:VisualElements DisplayName="Game" Square150x150Logo="logo.png" />
    </Application>
  </Applications>
</Package>"##;

    #[test]
    fn parses_elements_attributes_entities_cdata_and_namespaces() {
        let doc = Document::parse(SAMPLE_MANIFEST).unwrap();
        let root = doc.root_element();
        assert_eq!(root.tag_name(), "Package");
        assert!(root.parent().is_none());

        let identity = doc
            .descendants()
            .find(|n| n.has_tag_name("Identity"))
            .unwrap();
        assert_eq!(identity.attribute("Name"), Some("Foo & Bar"));
        assert_eq!(identity.attribute("Version"), Some("1.5.0.0"));

        let props = doc
            .descendants()
            .find(|n| n.has_tag_name("Properties"))
            .unwrap();
        assert_eq!(
            props.child_text("DisplayName"),
            Some("Tom & Jerry's ★ Game")
        );
        assert_eq!(props.child_text("Notes"), Some("<raw & literal>"));

        let visual = doc
            .descendants()
            .find(|n| n.has_tag_name("VisualElements"))
            .unwrap();
        assert_eq!(visual.parent().map(Node::tag_name), Some("Application"));
        assert_eq!(visual.attribute("Square150x150Logo"), Some("logo.png"));
    }

    #[test]
    fn rejects_malformed_and_deeply_nested_xml() {
        for bad in [
            "",
            "   ",
            "not xml",
            "<a/>extra",
            "<a/><b/>",
            "<a><b></a>",
            "<a><b>",
            "<a b=\"unclosed/>",
            "<a b=&unknown;/>",
            "<a b=\"&unknown;\"/>",
            "<!-- unclosed",
            "<?xml unclosed",
            "<a><![CDATA[unclosed</a>",
            "<a b=\"1\"c=\"2\"/>",
        ] {
            assert!(Document::parse(bad).is_none(), "accepted bad xml: {bad:?}");
        }
        let deep = format!(
            "{}{}",
            "<a>".repeat(MAX_DEPTH + 2),
            "</a>".repeat(MAX_DEPTH + 2)
        );
        assert!(Document::parse(&deep).is_none());
    }

    #[test]
    fn handles_bom_doctype_single_quotes_and_split_text() {
        let input = "\u{feff}<?xml version='1.0' encoding='utf-16'?>\n\
            <!DOCTYPE root [ <!ELEMENT root ANY> ]>\n\
            <root attr='line1\r\nline2\t&quot;ok&quot;'>hello<!-- mid --> world</root>";
        let doc = Document::parse(input).unwrap();
        let root = doc.root_element();
        assert_eq!(root.attribute("attr"), Some("line1 line2 \"ok\""));
        assert_eq!(root.text(), Some("hello world"));
    }
}
