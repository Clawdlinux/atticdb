//! Small edits to strict JSON files, made as text so they change only the bytes they must.
//!
//! Setup adds zerodb's parts to hand-kept files such as Claude Code's `settings.json`. Writing
//! them back through [`Value`] would sort every key and reformat the file. So each edit splices
//! text at byte spans found by a strict RFC 8259 parser. That parser refuses a UTF-8 BOM,
//! comments, trailing commas and duplicate keys, compared after decoding escapes.
//!
//! Every edit checks itself. It applies the same change to the original's [`Value`], and
//! returns the edited text only when that text parses to the same value.
//!
//! Layout:
//! - A new item copies the indentation of its previous sibling. With no sibling, it goes 1
//!   unit deeper than the line holding its parent's opening bracket.
//! - The unit is the leading whitespace of the first indented line, else 2 spaces.
//! - A container that sits on 1 line, from its opening bracket to its closing one, gets the
//!   new item inline. It copies the gap between the last 2 items, else uses `, `.
//! - Any other container gets the new item on its own line, pretty-printed and indented like
//!   the first line of its previous sibling.
//! - An empty container stays inline only when the whole file is 1 line, so `{"a":[]}`
//!   becomes `{"a":["x"]}`. Otherwise it opens up: `[`, the new item on its own line, `]`.
//! - A new object member goes last. Values on their own lines are pretty-printed, with keys
//!   in sorted order, as [`Value`] keeps them.
//! - Removing every element of an array leaves `[]`.
//! - CRLF files stay CRLF. Bytes after the root value, such as a final newline, never change.

use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fmt;

/// serde_json refuses deeper nesting, so this parser does too.
const MAX_DEPTH: usize = 127;

/// Bytes of a duplicate key shown in an error. Config files can hold secrets.
const SHOW: usize = 40;

/// Why an edit was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The text starts with a UTF-8 byte order mark.
    Bom,
    /// The text is not strict JSON.
    Syntax {
        /// The byte offset of the problem.
        at: usize,
        /// What is wrong there.
        what: &'static str,
    },
    /// An object holds the same key twice, compared after decoding escapes.
    DuplicateKey {
        /// The byte offset of the second key.
        at: usize,
        /// The decoded key, cut to its first 40 bytes.
        key: String,
    },
    /// A value on the path has the wrong type.
    WrongType {
        /// The keys that lead to the value.
        path: Vec<String>,
        /// The type the edit needs.
        want: &'static str,
        /// The type the file holds.
        found: &'static str,
    },
    /// The edit was refused because its result did not verify: the edited text was not
    /// strict JSON, or did not hold the same edit applied to serde_json's [`Value`].
    Check {
        /// The path the edit was for.
        path: Vec<String>,
        /// Why the result failed. It never shows more than 40 bytes of the file.
        why: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bom => f.write_str("the file starts with a UTF-8 byte order mark"),
            Self::Syntax { at, what } => write!(f, "invalid JSON at byte {at}: {what}"),
            Self::DuplicateKey { at, key } => write!(f, "duplicate key {key:?} at byte {at}"),
            Self::WrongType { path, want, found } => {
                write!(f, "{path:?} should be {want} but is {found}")
            }
            Self::Check { path, why } => {
                write!(f, "refused the edit at {path:?}, as the result did not verify: {why}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Appends `value` to the array at `path`, a list of literal keys. Missing objects on the way
/// and the array itself are created.
pub fn append(text: &str, path: &[&str], value: &Value) -> Result<String, Error> {
    let root = parse(text)?;
    let mut want = original(text, path)?;
    let (node, found) = walk(&root, path)?;
    let edited = match path.get(found) {
        Some(key) => {
            let mut nested = Value::Array(vec![value.clone()]);
            for inner in path[found + 1..].iter().rev() {
                nested = Value::Object(Map::from_iter([((*inner).to_owned(), nested)]));
            }
            insert(text, node, Some(key), &nested)
        }
        None if matches!(node.kind, Kind::Array(_)) => insert(text, node, None, value),
        None => return Err(wrong_type(path, "an array", node)),
    };
    push(&mut want, path, value)?;
    check(&edited, &want, path)?;
    Ok(edited)
}

/// Removes the elements of the array at `path` for which `pred` is true, with the commas and
/// whitespace they own. Returns the text and how many went. A missing path removes 0.
pub fn remove(
    text: &str,
    path: &[&str],
    pred: impl Fn(&Value) -> bool,
) -> Result<(String, usize), Error> {
    let root = parse(text)?;
    let mut want = original(text, path)?;
    let (node, found) = walk(&root, path)?;
    if found < path.len() {
        return Ok((text.to_owned(), 0));
    }
    let Kind::Array(items) = &node.kind else {
        return Err(wrong_type(path, "an array", node));
    };
    let mut at = &mut want;
    for key in path {
        at = at.get_mut(*key).ok_or_else(|| refused(path, "the path differs in serde_json"))?;
    }
    let values = match at {
        Value::Array(values) if values.len() == items.len() => values,
        _ => return Err(refused(path, "the array differs in serde_json")),
    };
    let hits: Vec<bool> = values.iter().map(pred).collect();
    let removed = hits.iter().filter(|hit| **hit).count();
    if removed == 0 {
        return Ok((text.to_owned(), 0));
    }
    let kept = std::mem::take(values).into_iter().zip(&hits).filter(|(_, hit)| !**hit);
    *values = kept.map(|(value, _)| value).collect();
    let edited = cut(text, node, items, &hits);
    check(&edited, &want, path)?;
    Ok((edited, removed))
}

/// The original text as serde_json reads it.
fn original(text: &str, path: &[&str]) -> Result<Value, Error> {
    serde_json::from_str(text)
        .map_err(|err| refused(path, format!("serde_json rejects the original text: {err}")))
}

/// Refuses `edited` unless it is strict JSON that serde_json reads as `want`. The reasons
/// come from the strict parser and serde_json, which name places but show no file content
/// beyond a duplicate key's first 40 bytes.
fn check(edited: &str, want: &Value, path: &[&str]) -> Result<(), Error> {
    parse(edited)
        .map_err(|err| refused(path, format!("the edited text is not strict JSON: {err}")))?;
    let got: Value = serde_json::from_str(edited)
        .map_err(|err| refused(path, format!("serde_json rejects the edited text: {err}")))?;
    if got != *want {
        return Err(refused(path, "the edited text holds a different value"));
    }
    Ok(())
}

fn refused(path: &[&str], why: impl Into<String>) -> Error {
    Error::Check { path: path.iter().map(|key| (*key).to_owned()).collect(), why: why.into() }
}

/// The edit [`append`] makes, applied to a [`Value`].
fn push(root: &mut Value, path: &[&str], value: &Value) -> Result<(), Error> {
    let mut at = root;
    for (i, key) in path.iter().enumerate() {
        let Value::Object(map) = at else {
            return Err(refused(path, "the path differs in serde_json"));
        };
        let fresh =
            if i + 1 == path.len() { Value::Array(Vec::new()) } else { Value::Object(Map::new()) };
        at = map.entry(*key).or_insert(fresh);
    }
    match at {
        Value::Array(items) => {
            items.push(value.clone());
            Ok(())
        }
        _ => Err(refused(path, "the path differs in serde_json")),
    }
}

/// The deepest node on `path` and how many keys led to it. Each step must be an object.
fn walk<'a>(root: &'a Node, path: &[&str]) -> Result<(&'a Node, usize), Error> {
    let mut node = root;
    for (i, key) in path.iter().enumerate() {
        let Kind::Object(members) = &node.kind else {
            return Err(wrong_type(&path[..i], "an object", node));
        };
        match members.iter().find(|member| member.key == *key) {
            Some(member) => node = &member.value,
            None => return Ok((node, i)),
        }
    }
    Ok((node, path.len()))
}

fn wrong_type(path: &[&str], want: &'static str, node: &Node) -> Error {
    let path = path.iter().map(|key| (*key).to_owned()).collect();
    Error::WrongType { path, want, found: node.type_name() }
}

/// `text` with a new last item in `container`: the member `key: value` in an object, or the
/// element `value` in an array.
fn insert(text: &str, container: &Node, key: Option<&str>, value: &Value) -> String {
    let (nl, unit) = style(text);
    let spans = container.spans();
    let mut colon = ": ";
    if let Kind::Object(members) = &container.kind {
        if let Some(last) = members.last() {
            let between = &text[last.key_end..last.value.start];
            if !between.contains('\n') {
                colon = between;
            }
        }
    }
    let label = key.map(|key| format!("{}{colon}", Value::from(key))).unwrap_or_default();
    let one_line = !text[container.start..container.end].contains('\n');
    let (from, to, new) = match spans.last() {
        Some(&(_, end)) if one_line => {
            let gap = match spans.as_slice() {
                [.., (_, prev_end), (last_start, _)] => &text[*prev_end..*last_start],
                _ => ", ",
            };
            (end, end, format!("{gap}{label}{value}"))
        }
        Some(&(start, end)) => {
            let indent = line_indent(text, start);
            (end, end, format!(",{nl}{indent}{label}{}", render(value, indent, unit, nl)))
        }
        None if !text.trim_end().contains('\n') => {
            (container.start + 1, container.end - 1, format!("{label}{value}"))
        }
        None => {
            let outer = line_indent(text, container.start);
            let indent = format!("{outer}{unit}");
            let item = render(value, &indent, unit, nl);
            (
                container.start + 1,
                container.end - 1,
                format!("{nl}{indent}{label}{item}{nl}{outer}"),
            )
        }
    };
    format!("{}{new}{}", &text[..from], &text[to..])
}

/// `text` without the elements of `array` marked in `hits`, with the commas and whitespace
/// they own. A run of removed elements takes the gap after it, or the gap before it when it
/// ends the array, so removing what [`insert`] added restores the bytes.
fn cut(text: &str, array: &Node, items: &[Node], hits: &[bool]) -> String {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if !hits[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while hits.get(j + 1) == Some(&true) {
            j += 1;
        }
        spans.push(match (items.get(j + 1), i.checked_sub(1)) {
            (Some(next), _) => (items[i].start, next.start),
            (None, Some(prev)) => (items[prev].end, items[j].end),
            (None, None) => (array.start + 1, array.end - 1),
        });
        i = j + 1;
    }
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for (start, end) in spans {
        out.push_str(&text[from..start]);
        from = end;
    }
    out.push_str(&text[from..]);
    out
}

/// `value` pretty-printed with `unit` per level. Lines after the first start with `indent`.
fn render(value: &Value, indent: &str, unit: &str, nl: &str) -> String {
    let mut out = String::new();
    // Strings hold no raw newlines, so every line break here is layout.
    for (i, line) in format!("{value:#}").split('\n').enumerate() {
        if i > 0 {
            out.push_str(nl);
            out.push_str(indent);
        }
        let body = line.trim_start_matches(' ');
        out.push_str(&unit.repeat((line.len() - body.len()) / 2));
        out.push_str(body);
    }
    out
}

/// The file's line break and indent unit.
fn style(text: &str) -> (&'static str, &str) {
    let nl = match text.find('\n') {
        Some(i) if text[..i].ends_with('\r') => "\r\n",
        _ => "\n",
    };
    let unit = text.split('\n').find_map(|line| {
        let width = line.bytes().take_while(|b| matches!(b, b' ' | b'\t')).count();
        (width > 0 && width < line.trim_end_matches('\r').len()).then(|| &line[..width])
    });
    (nl, unit.unwrap_or("  "))
}

/// The leading spaces and tabs of the line holding byte `at`.
fn line_indent(text: &str, at: usize) -> &str {
    let line = &text[text[..at].rfind('\n').map_or(0, |i| i + 1)..];
    &line[..line.bytes().take_while(|b| matches!(b, b' ' | b'\t')).count()]
}

/// A parsed value and its byte span.
#[derive(Debug)]
struct Node {
    start: usize,
    end: usize,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    /// A string, number, boolean or null, named for errors.
    Scalar(&'static str),
}

/// An object member. Its span runs from `start`, the key's opening quote, to `value.end`.
#[derive(Debug)]
struct Member {
    /// The key with its escapes decoded.
    key: String,
    start: usize,
    /// The byte after the key's closing quote.
    key_end: usize,
    value: Node,
}

impl Node {
    fn type_name(&self) -> &'static str {
        match self.kind {
            Kind::Object(_) => "an object",
            Kind::Array(_) => "an array",
            Kind::Scalar(name) => name,
        }
    }

    /// The span of each member or element.
    fn spans(&self) -> Vec<(usize, usize)> {
        match &self.kind {
            Kind::Object(members) => members.iter().map(|m| (m.start, m.value.end)).collect(),
            Kind::Array(items) => items.iter().map(|item| (item.start, item.end)).collect(),
            Kind::Scalar(_) => Vec::new(),
        }
    }
}

/// Parses `text` as 1 strict RFC 8259 value, with optional whitespace around it.
fn parse(text: &str) -> Result<Node, Error> {
    if text.starts_with('\u{feff}') {
        return Err(Error::Bom);
    }
    let mut parser = Parser { text, pos: 0, depth: 0 };
    parser.ws();
    let root = parser.value()?;
    parser.ws();
    if parser.pos < text.len() {
        return Err(parser.unexpected("unexpected text after the root value"));
    }
    Ok(root)
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn fail(&self, what: &'static str) -> Error {
        Error::Syntax { at: self.pos, what }
    }

    /// The error for the byte at `pos`, where `expected` names what should be there.
    fn unexpected(&self, expected: &'static str) -> Error {
        self.fail(match self.peek() {
            None => "unexpected end of text",
            Some(b'/') => "comments are not JSON",
            Some(_) => expected,
        })
    }

    fn value(&mut self) -> Result<Node, Error> {
        let start = self.pos;
        let kind = match self.peek() {
            Some(b'{') => Kind::Object(self.object()?),
            Some(b'[') => Kind::Array(self.array()?),
            Some(b'"') => {
                self.string()?;
                Kind::Scalar("a string")
            }
            Some(b'-' | b'0'..=b'9') => {
                self.number()?;
                Kind::Scalar("a number")
            }
            Some(b't') => self.literal("true", "a boolean")?,
            Some(b'f') => self.literal("false", "a boolean")?,
            Some(b'n') => self.literal("null", "null")?,
            _ => return Err(self.unexpected("expected a value")),
        };
        Ok(Node { start, end: self.pos, kind })
    }

    /// Steps into a container at its opening bracket.
    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.fail("nesting is too deep"));
        }
        self.pos += 1;
        self.ws();
        Ok(())
    }

    /// After an item: `true` at the closing bracket, `false` after a comma.
    fn next(&mut self, close: u8, expected: &'static str) -> Result<bool, Error> {
        self.ws();
        match self.peek() {
            Some(b',') => {
                let comma = self.pos;
                self.pos += 1;
                self.ws();
                if self.peek() == Some(close) {
                    return Err(Error::Syntax { at: comma, what: "trailing comma" });
                }
                Ok(false)
            }
            Some(byte) if byte == close => {
                self.pos += 1;
                self.depth -= 1;
                Ok(true)
            }
            _ => Err(self.unexpected(expected)),
        }
    }

    fn array(&mut self) -> Result<Vec<Node>, Error> {
        self.enter()?;
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(items);
        }
        loop {
            items.push(self.value()?);
            if self.next(b']', "expected ',' or ']'")? {
                return Ok(items);
            }
        }
    }

    fn object(&mut self) -> Result<Vec<Member>, Error> {
        self.enter()?;
        let mut members = Vec::new();
        let mut seen = HashSet::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(members);
        }
        loop {
            if self.peek() != Some(b'"') {
                return Err(self.unexpected("expected a string key"));
            }
            let start = self.pos;
            let key = self.string()?;
            let key_end = self.pos;
            if !seen.insert(key.clone()) {
                let mut end = SHOW.min(key.len());
                while !key.is_char_boundary(end) {
                    end -= 1;
                }
                return Err(Error::DuplicateKey { at: start, key: key[..end].to_owned() });
            }
            self.ws();
            if self.peek() != Some(b':') {
                return Err(self.unexpected("expected ':'"));
            }
            self.pos += 1;
            self.ws();
            let value = self.value()?;
            members.push(Member { key, start, key_end, value });
            if self.next(b'}', "expected ',' or '}'")? {
                return Ok(members);
            }
        }
    }

    /// Reads the string at `pos` and returns it decoded.
    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1;
        let mut out = String::new();
        let mut run = self.pos;
        loop {
            match self.peek() {
                None => return Err(self.fail("unterminated string")),
                Some(b'"') => {
                    out.push_str(&self.text[run..self.pos]);
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    out.push_str(&self.text[run..self.pos]);
                    self.escape(&mut out)?;
                    run = self.pos;
                }
                Some(byte) if byte < 0x20 => return Err(self.fail("control character in string")),
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Decodes the escape at `pos` into `out`. A surrogate pair takes 2 `\u` escapes.
    fn escape(&mut self, out: &mut String) -> Result<(), Error> {
        let at = self.pos;
        self.pos += 1;
        let simple = match self.peek() {
            Some(b'"') => '"',
            Some(b'\\') => '\\',
            Some(b'/') => '/',
            Some(b'b') => '\u{8}',
            Some(b'f') => '\u{c}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b't') => '\t',
            Some(b'u') => {
                let lone = Error::Syntax { at, what: "lone surrogate in \\u escape" };
                self.pos += 1;
                let mut code = self.hex4()?;
                if (0xd800..0xdc00).contains(&code) {
                    if !self.text[self.pos..].starts_with("\\u") {
                        return Err(lone);
                    }
                    self.pos += 2;
                    let low = self.hex4()?;
                    if !(0xdc00..0xe000).contains(&low) {
                        return Err(lone);
                    }
                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                }
                out.push(char::from_u32(code).ok_or(lone)?);
                return Ok(());
            }
            _ => return Err(Error::Syntax { at, what: "invalid escape" }),
        };
        self.pos += 1;
        out.push(simple);
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let digits = self.text.get(self.pos..self.pos + 4);
        let code = digits
            .filter(|digits| digits.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .ok_or(self.fail("invalid \\u escape"))?;
        self.pos += 4;
        Ok(code)
    }

    /// Skips digits and returns how many.
    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Result<(), Error> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.fail("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if self.digits() == 0 {
                return Err(self.fail("invalid number"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return Err(self.fail("invalid number"));
            }
        }
        // serde_json refuses numbers too large for an f64.
        if !self.text[start..self.pos].parse::<f64>().is_ok_and(f64::is_finite) {
            return Err(Error::Syntax { at: start, what: "number out of range" });
        }
        Ok(())
    }

    fn literal(&mut self, word: &'static str, name: &'static str) -> Result<Kind, Error> {
        if !self.text[self.pos..].starts_with(word) {
            return Err(self.fail("invalid literal"));
        }
        self.pos += word.len();
        Ok(Kind::Scalar(name))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{append, remove};
    use serde_json::{json, Value};

    /// An edit and its arguments, with values written as JSON.
    enum Edit {
        Append(&'static [&'static str], &'static str),
        /// The path, the elements to drop, and how many of them the array holds.
        Remove(&'static [&'static str], &'static [&'static str], usize),
    }
    use Edit::{Append, Remove};

    /// Name, input, edit, and the exact output text or error message.
    type Case = (&'static str, &'static str, Edit, Result<&'static str, &'static str>);

    const CASES: &[Case] = &[
        (
            "empty object in a 1-line file, nested path created",
            "{}",
            Append(&["hooks", "SessionStart"], r#"{"matcher": "startup"}"#),
            Ok(r#"{"hooks": {"SessionStart":[{"matcher":"startup"}]}}"#),
        ),
        (
            "empty array in a 1-line file",
            r#"{"a":[]}"#,
            Append(&["a"], r#""x""#),
            Ok(r#"{"a":["x"]}"#),
        ),
        (
            "multi-line object in an array that opens and closes on its lines",
            "{\n  \"h\": [{\n    \"a\": 1\n  }]\n}\n",
            Append(&["h"], r#"{"b": 2}"#),
            Ok("{\n  \"h\": [{\n    \"a\": 1\n  },\n  {\n    \"b\": 2\n  }]\n}\n"),
        ),
        (
            "remove what was appended after a multi-line object",
            "{\n  \"h\": [{\n    \"a\": 1\n  },\n  {\n    \"b\": 2\n  }]\n}\n",
            Remove(&["h"], &[r#"{"b": 2}"#], 1),
            Ok("{\n  \"h\": [{\n    \"a\": 1\n  }]\n}\n"),
        ),
        (
            "remove the first of 2 multi-line objects",
            "{\n  \"h\": [{\n    \"a\": 1\n  }, {\n    \"b\": 2\n  }]\n}\n",
            Remove(&["h"], &[r#"{"a": 1}"#], 1),
            Ok("{\n  \"h\": [{\n    \"b\": 2\n  }]\n}\n"),
        ),
        (
            "remove the last of 2 multi-line objects",
            "{\n  \"h\": [{\n    \"a\": 1\n  }, {\n    \"b\": 2\n  }]\n}\n",
            Remove(&["h"], &[r#"{"b": 2}"#], 1),
            Ok("{\n  \"h\": [{\n    \"a\": 1\n  }]\n}\n"),
        ),
        (
            "missing array where its object exists",
            "{\n  \"permissions\": {\n    \"deny\": [\n      \"Bash(rm:*)\"\n    ]\n  }\n}\n",
            Append(&["permissions", "allow"], r#""mcp__zerodb""#),
            Ok("{\n  \"permissions\": {\n    \"deny\": [\n      \"Bash(rm:*)\"\n    ],\n    \"allow\": [\n      \"mcp__zerodb\"\n    ]\n  }\n}\n"),
        ),
        (
            "array with items",
            "{\n  \"allow\": [\n    \"a\",\n    \"b\"\n  ]\n}\n",
            Append(&["allow"], r#""c""#),
            Ok("{\n  \"allow\": [\n    \"a\",\n    \"b\",\n    \"c\"\n  ]\n}\n"),
        ),
        (
            "empty array on 1 line",
            "{\n  \"allow\": []\n}\n",
            Append(&["allow"], r#""a""#),
            Ok("{\n  \"allow\": [\n    \"a\"\n  ]\n}\n"),
        ),
        (
            "empty array on 2 lines",
            "{\n  \"allow\": [\n  ]\n}\n",
            Append(&["allow"], r#""a""#),
            Ok("{\n  \"allow\": [\n    \"a\"\n  ]\n}\n"),
        ),
        (
            "empty root array, object value",
            "[\n]\n",
            Append(&[], r#"{"b": [1, 2], "a": {}}"#),
            Ok("[\n  {\n    \"a\": {},\n    \"b\": [\n      1,\n      2\n    ]\n  }\n]\n"),
        ),
        (
            "remove the last element",
            "{\n  \"allow\": [\n    \"a\",\n    \"b\",\n    \"c\"\n  ]\n}\n",
            Remove(&["allow"], &[r#""c""#], 1),
            Ok("{\n  \"allow\": [\n    \"a\",\n    \"b\"\n  ]\n}\n"),
        ),
        (
            "remove the only element",
            "{\n  \"allow\": [\n    \"a\"\n  ],\n  \"x\": 1\n}\n",
            Remove(&["allow"], &[r#""a""#], 1),
            Ok("{\n  \"allow\": [],\n  \"x\": 1\n}\n"),
        ),
        (
            "remove a middle element",
            "{\n  \"allow\": [\n    \"a\",\n    \"b\",\n    \"c\"\n  ]\n}\n",
            Remove(&["allow"], &[r#""b""#], 1),
            Ok("{\n  \"allow\": [\n    \"a\",\n    \"c\"\n  ]\n}\n"),
        ),
        (
            "remove the first and third of 4",
            "{\n  \"allow\": [\n    \"a\",\n    \"b\",\n    \"c\",\n    \"d\"\n  ]\n}\n",
            Remove(&["allow"], &[r#""a""#, r#""c""#], 2),
            Ok("{\n  \"allow\": [\n    \"b\",\n    \"d\"\n  ]\n}\n"),
        ),
        (
            "remove the second and fourth of 4",
            "{\n  \"allow\": [\n    \"a\",\n    \"b\",\n    \"c\",\n    \"d\"\n  ]\n}\n",
            Remove(&["allow"], &[r#""b""#, r#""d""#], 2),
            Ok("{\n  \"allow\": [\n    \"a\",\n    \"c\"\n  ]\n}\n"),
        ),
        (
            "remove 2 adjacent objects",
            "[\n  {\"id\": 1},\n  {\"id\": 2},\n  {\"id\": 3},\n  {\"id\": 4}\n]",
            Remove(&[], &[r#"{"id": 2}"#, r#"{"id": 3}"#], 2),
            Ok("[\n  {\"id\": 1},\n  {\"id\": 4}\n]"),
        ),
        (
            "CRLF append",
            "{\r\n  \"allow\": [\r\n    \"a\"\r\n  ]\r\n}\r\n",
            Append(&["allow"], r#""b""#),
            Ok("{\r\n  \"allow\": [\r\n    \"a\",\r\n    \"b\"\r\n  ]\r\n}\r\n"),
        ),
        (
            "CRLF nested object, no final newline",
            "{\r\n  \"x\": 1\r\n}",
            Append(&["hooks", "Stop"], r#"{"a": 1}"#),
            Ok("{\r\n  \"x\": 1,\r\n  \"hooks\": {\r\n    \"Stop\": [\r\n      {\r\n        \"a\": 1\r\n      }\r\n    ]\r\n  }\r\n}"),
        ),
        (
            "CRLF remove",
            "{\r\n  \"allow\": [\r\n    \"a\",\r\n    \"b\"\r\n  ]\r\n}\r\n",
            Remove(&["allow"], &[r#""a""#], 1),
            Ok("{\r\n  \"allow\": [\r\n    \"b\"\r\n  ]\r\n}\r\n"),
        ),
        (
            "tab indentation",
            "{\n\t\"hooks\": {\n\t\t\"Stop\": []\n\t}\n}\n",
            Append(&["hooks", "Stop"], r#"{"type": "command", "command": "zerodb hook"}"#),
            Ok("{\n\t\"hooks\": {\n\t\t\"Stop\": [\n\t\t\t{\n\t\t\t\t\"command\": \"zerodb hook\",\n\t\t\t\t\"type\": \"command\"\n\t\t\t}\n\t\t]\n\t}\n}\n"),
        ),
        (
            "4-space indentation, into an empty object",
            "{\n    \"allow\": [\n        \"a\"\n    ],\n    \"x\": {}\n}\n",
            Append(&["x", "y"], r#""z""#),
            Ok("{\n    \"allow\": [\n        \"a\"\n    ],\n    \"x\": {\n        \"y\": [\n            \"z\"\n        ]\n    }\n}\n"),
        ),
        (
            "inline array append",
            "{\n  \"allow\": [\"a\", \"b\"]\n}\n",
            Append(&["allow"], r#""c""#),
            Ok("{\n  \"allow\": [\"a\", \"b\", \"c\"]\n}\n"),
        ),
        (
            "inline array keeps a compact gap",
            "{\"allow\":[\"a\",\"b\"]}",
            Append(&["allow"], r#""c""#),
            Ok("{\"allow\":[\"a\",\"b\",\"c\"]}"),
        ),
        (
            "inline array remove middle",
            "{\"allow\": [\"a\", \"b\", \"c\"]}",
            Remove(&["allow"], &[r#""b""#], 1),
            Ok("{\"allow\": [\"a\", \"c\"]}"),
        ),
        (
            "inline array remove last",
            "{\"allow\": [\"a\", \"b\", \"c\"]}",
            Remove(&["allow"], &[r#""c""#], 1),
            Ok("{\"allow\": [\"a\", \"b\"]}"),
        ),
        (
            "inline object gets an escaped key",
            "{\"a\": 1}",
            Append(&["q\"k"], "1"),
            Ok("{\"a\": 1, \"q\\\"k\": [1]}"),
        ),
        (
            "escaped key on the path, escaped values",
            r#"{"a\u0062": ["say \"hi\"", "\u00e9"]}"#,
            Append(&["ab"], r#""x\"y""#),
            Ok(r#"{"a\u0062": ["say \"hi\"", "\u00e9", "x\"y"]}"#),
        ),
        (
            "remove by decoded value",
            r#"{"a\u0062": ["say \"hi\"", "\u00e9", "\ud83d\ude00"]}"#,
            Remove(&["ab"], &[r#""é""#, r#""😀""#], 2),
            Ok(r#"{"a\u0062": ["say \"hi\""]}"#),
        ),
        (
            "slashes inside a string",
            r#"{"urls": ["http://example.com/*"]}"#,
            Append(&["urls"], r#""https://a/*b*/""#),
            Ok(r#"{"urls": ["http://example.com/*", "https://a/*b*/"]}"#),
        ),
        (
            "nothing matches",
            "{\n  \"allow\": [\n    \"a\"\n  ]\n}",
            Remove(&["allow"], &[r#""b""#], 0),
            Ok("{\n  \"allow\": [\n    \"a\"\n  ]\n}"),
        ),
        (
            "missing path removes nothing",
            "{\"hooks\": {}}",
            Remove(&["hooks", "SessionStart"], &[r#""a""#], 0),
            Ok("{\"hooks\": {}}"),
        ),
        (
            "BOM",
            "\u{feff}{\"allow\": []}",
            Append(&["allow"], r#""a""#),
            Err("the file starts with a UTF-8 byte order mark"),
        ),
        (
            "line comment",
            "{\n  // note\n  \"allow\": []\n}",
            Append(&["allow"], r#""a""#),
            Err("invalid JSON at byte 4: comments are not JSON"),
        ),
        (
            "block comment",
            "{\"allow\": [] /* x */}",
            Remove(&["allow"], &[r#""a""#], 0),
            Err("invalid JSON at byte 13: comments are not JSON"),
        ),
        (
            "trailing comma in an array",
            "{\"allow\": [\"x\",]}",
            Append(&["allow"], r#""a""#),
            Err("invalid JSON at byte 14: trailing comma"),
        ),
        (
            "trailing comma in an object",
            "{\"allow\": [],\n}",
            Append(&["allow"], r#""a""#),
            Err("invalid JSON at byte 12: trailing comma"),
        ),
        (
            "duplicate key",
            "{\"a\": [], \"a\": []}",
            Append(&["a"], "1"),
            Err("duplicate key \"a\" at byte 10"),
        ),
        (
            "duplicate key off the path",
            "{\"x\": {\"k\": 1, \"k\": 2}, \"a\": []}",
            Remove(&["a"], &["1"], 0),
            Err("duplicate key \"k\" at byte 15"),
        ),
        (
            "duplicate key by escape",
            r#"{"ab": [], "a\u0062": []}"#,
            Append(&["ab"], "1"),
            Err("duplicate key \"ab\" at byte 11"),
        ),
        (
            "long duplicate key is cut",
            r#"{"0123456789012345678901234567890123456789XX": 1, "0123456789012345678901234567890123456789XX": 2}"#,
            Append(&["a"], "1"),
            Err("duplicate key \"0123456789012345678901234567890123456789\" at byte 50"),
        ),
        (
            "object where an array should be",
            "{\"permissions\": {\"allow\": {}}}",
            Append(&["permissions", "allow"], r#""a""#),
            Err("[\"permissions\", \"allow\"] should be an array but is an object"),
        ),
        (
            "string where an object should be",
            "{\"permissions\": \"x\"}",
            Append(&["permissions", "allow"], r#""a""#),
            Err("[\"permissions\"] should be an object but is a string"),
        ),
        (
            "remove through a string",
            "{\"permissions\": \"x\"}",
            Remove(&["permissions", "allow"], &[r#""a""#], 0),
            Err("[\"permissions\"] should be an object but is a string"),
        ),
        (
            "root is not an object",
            "[1]",
            Append(&["allow"], r#""a""#),
            Err("[] should be an object but is an array"),
        ),
        (
            "text after the root value",
            "{\"allow\": []} x",
            Append(&["allow"], r#""a""#),
            Err("invalid JSON at byte 14: unexpected text after the root value"),
        ),
        (
            "second root value",
            "{}{}",
            Append(&["allow"], r#""a""#),
            Err("invalid JSON at byte 2: unexpected text after the root value"),
        ),
        (
            "lone surrogate",
            r#"{"a": ["\ud800"]}"#,
            Append(&["a"], "1"),
            Err("invalid JSON at byte 8: lone surrogate in \\u escape"),
        ),
        (
            "number too large",
            "{\"a\": [1e400]}",
            Append(&["a"], "1"),
            Err("invalid JSON at byte 7: number out of range"),
        ),
        ("empty text", "", Append(&["a"], "1"), Err("invalid JSON at byte 0: unexpected end of text")),
    ];

    #[test]
    fn edits_change_only_their_bytes() {
        for (name, text, edit, want) in CASES {
            let got = match edit {
                Append(path, value) => {
                    append(text, path, &serde_json::from_str(value).unwrap()).map(|out| (out, 1))
                }
                Remove(path, drop, _) => {
                    let drop: Vec<Value> =
                        drop.iter().map(|value| serde_json::from_str(value).unwrap()).collect();
                    remove(text, path, |value| drop.contains(value))
                }
            };
            match (got, want) {
                (Ok((out, count)), Ok(want)) => {
                    assert_eq!(out, *want, "{name}: output text");
                    if let Remove(_, _, removed) = edit {
                        assert_eq!(count, *removed, "{name}: removed count");
                    }
                }
                (Err(err), Err(want)) => assert_eq!(err.to_string(), *want, "{name}: error"),
                (got, _) => panic!("{name}: got {got:?}, want {want:?}"),
            }
        }

        let mut deep = json!(1);
        for _ in 0..130 {
            deep = json!([deep]);
        }
        let err = append(r#"{"a":[]}"#, &["a"], &deep).unwrap_err().to_string();
        let want = "refused the edit at [\"a\"], as the result did not verify: the edited text \
                    is not strict JSON: invalid JSON at byte 131: nesting is too deep";
        assert_eq!(err, want, "too deep: error");
    }

    /// Shaped like a Claude Code `settings.json`, with 16 top-level keys and 10 hook events.
    const SETTINGS: &str = r##"{
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "model": "opus",
  "theme": "dark",
  "cleanupPeriodDays": 30,
  "includeCoAuthoredBy": false,
  "autoUpdates": false,
  "alwaysThinkingEnabled": true,
  "outputStyle": "Explanatory",
  "env": {
    "BASH_DEFAULT_TIMEOUT_MS": "120000",
    "DISABLE_TELEMETRY": "1"
  },
  "permissions": {
    "allow": [
      "Bash(git status:*)",
      "Bash(git diff:*)",
      "Bash(git log:*)",
      "Bash(cargo test:*)",
      "Bash(cargo clippy:*)",
      "Bash(cargo fmt:*)",
      "Bash(npm run lint)",
      "Read(~/notes/**)",
      "WebFetch(domain:docs.rs)",
      "WebSearch",
      "mcp__github__get_issue",
      "mcp__github__list_pull_requests"
    ],
    "deny": [
      "Read(./.env)",
      "Bash(curl:*)"
    ],
    "defaultMode": "acceptEdits"
  },
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "~/bin/guard.sh \"$CLAUDE_PROJECT_DIR\"",
            "timeout": 10
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [{"type": "command", "command": "cargo fmt --all"}]
      }
    ],
    "Notification": [
      {
        "hooks": [{"type": "command", "command": "osascript -e 'display notification \"Claude\"'"}]
      }
    ],
    "UserPromptSubmit": [],
    "Stop": [
      {
        "hooks": [{"type": "command", "command": "afplay /System/Library/Sounds/Glass.aiff"}]
      }
    ],
    "SubagentStop": [],
    "PreCompact": [
      {
        "matcher": "auto",
        "hooks": [{"type": "command", "command": "~/bin/save-transcript.sh"}]
      }
    ],
    "SessionStart": [
      {
        "matcher": "startup",
        "hooks": [{"type": "command", "command": "echo \"branch: $(git branch --show-current)\""}]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [{"type": "command", "command": "~/bin/log-session.sh"}]
      }
    ],
    "PermissionRequest": []
  },
  "statusLine": {
    "type": "command",
    "command": "~/.claude/statusline.sh",
    "padding": 0
  },
  "enabledPlugins": {
    "rust-analyzer@claude-plugins": true
  },
  "spinnerTipsEnabled": false,
  "enableAllProjectMcpServers": false,
  "forceLoginMethod": "claudeai"
}
"##;

    #[test]
    fn append_then_remove_restores_a_settings_file() {
        let group = json!({"hooks": [{
            "type": "command",
            "command": "'/opt/zerodb/bin/zerodb' hook session-start --harness claude-code",
            "timeout": 5,
        }]});
        let tool = json!("mcp__zerodb__memory_search");
        let hooked = append(SETTINGS, &["hooks", "SessionStart"], &group).unwrap();
        let block = r#"
      },
      {
        "hooks": [
          {
            "command": "'/opt/zerodb/bin/zerodb' hook session-start --harness claude-code",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ],
    "SessionEnd""#;
        assert!(hooked.contains(block), "settings: the group follows its sibling's layout");
        let both = append(&hooked, &["permissions", "allow"], &tool).unwrap();
        let line = "\"mcp__github__list_pull_requests\",\n      \"mcp__zerodb__memory_search\"\n";
        assert!(both.contains(line), "settings: the tool is the last allow line");
        let lines = |text: &str| text.lines().count();
        assert_eq!(lines(&both), lines(SETTINGS) + 10, "settings: only the new lines are added");

        let (unhooked, removed) =
            remove(&both, &["hooks", "SessionStart"], |v| *v == group).unwrap();
        assert_eq!(removed, 1, "settings: 1 group removed");
        let (restored, removed) =
            remove(&unhooked, &["permissions", "allow"], |v| *v == tool).unwrap();
        assert_eq!(removed, 1, "settings: 1 tool removed");
        assert_eq!(restored, SETTINGS, "settings: byte-identical after removing both");
    }
}
