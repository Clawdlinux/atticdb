//! A small scanner for the structure of a TOML file: its table headers, keys and comment
//! lines, with their bytes. It tracks strings, arrays and inline tables, so text inside them
//! never looks like a header, a key or a comment. It decodes keys, but doesn't check values,
//! so a file it reads may still not be valid TOML. Its errors give a line number, never the
//! file's text, which may hold secrets.

use std::ops::Range;

/// What a statement of a TOML file is.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Item {
    /// A table header, `[a.b]` or `[[a.b]]`, with its decoded key path.
    Table(Vec<String>),
    /// A key/value pair, with the key's decoded path from the root, and the value's text
    /// without a comment after it.
    Key(Vec<String>, String),
    /// A comment on its own line, from its `#` to the line end.
    Comment(String),
}

/// An item, and its bytes: from the start of its first line to after its last line end.
pub(super) struct Entry {
    /// The item.
    pub(super) item: Item,
    /// Its bytes.
    pub(super) span: Range<usize>,
}

/// The headers, keys and comment lines of `text`, in order. Blank lines aren't listed.
pub(super) fn scan(text: &str) -> Result<Vec<Entry>, String> {
    let mut at = Cursor { text, at: 0 };
    let (mut table, mut found) = (Vec::new(), Vec::new());
    while at.at < text.len() {
        let start = at.at;
        at.blanks();
        let item = match at.peek() {
            None | Some(b'\n' | b'\r') => {
                at.line_end()?;
                continue;
            }
            Some(b'#') => {
                let from = at.at;
                at.comment();
                let comment = text[from..at.at].to_owned();
                at.line_end()?;
                Item::Comment(comment)
            }
            Some(b'[') => {
                let array = at.eat("[[");
                if !array {
                    at.at += 1;
                }
                table = at.key()?;
                if !at.eat(if array { "]]" } else { "]" }) {
                    return Err(at.error("a table header isn't closed"));
                }
                at.rest()?;
                Item::Table(table.clone())
            }
            Some(_) => {
                let key = at.key()?;
                if !at.eat("=") {
                    return Err(at.error("a key has no ="));
                }
                at.blanks();
                let from = at.at;
                let end = at.value()?;
                at.rest()?;
                let path = table.iter().cloned().chain(key).collect();
                Item::Key(path, text[from..end].to_owned())
            }
        };
        found.push(Entry { item, span: start..at.at });
    }
    Ok(found)
}

/// A place in a TOML file. Outside strings, it only stops on a character boundary.
struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    /// Steps over `word` when the bytes go on with it.
    fn eat(&mut self, word: &str) -> bool {
        let rest = self.text.as_bytes().get(self.at..).unwrap_or_default();
        let starts = rest.starts_with(word.as_bytes());
        if starts {
            self.at += word.len();
        }
        starts
    }

    fn error(&self, what: &str) -> String {
        let before = &self.text.as_bytes()[..self.at.min(self.text.len())];
        format!("{what} on line {}", before.iter().filter(|byte| **byte == b'\n').count() + 1)
    }

    /// Steps over spaces and tabs.
    fn blanks(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.at += 1;
        }
    }

    /// Steps to the end of a comment, before its line end, `\r\n` or `\n`.
    fn comment(&mut self) {
        let rest = &self.text[self.at..];
        let end = rest.find('\n').unwrap_or(rest.len());
        self.at +=
            rest[..end].strip_suffix('\r').filter(|_| end < rest.len()).map_or(end, str::len);
    }

    /// Steps over a line end. The end of the file counts as one.
    fn line_end(&mut self) -> Result<(), String> {
        if self.at == self.text.len() || self.eat("\n") || self.eat("\r\n") {
            Ok(())
        } else {
            Err(self.error("a line has more after its end"))
        }
    }

    /// Steps over blanks, a comment and the line end after a header or a value.
    fn rest(&mut self) -> Result<(), String> {
        self.blanks();
        if self.peek() == Some(b'#') {
            self.comment();
        }
        self.line_end()
    }

    /// Reads a dotted key, with blanks around its dots, and decodes each part.
    fn key(&mut self) -> Result<Vec<String>, String> {
        let mut path = Vec::new();
        loop {
            self.blanks();
            let rest = &self.text[self.at..];
            let part = match self.peek() {
                Some(b'"') => self.basic_key()?,
                Some(b'\'') => {
                    let end = rest[1..].find(['\'', '\n']).map(|end| end + 1);
                    let end = end.filter(|end| rest.as_bytes()[*end] == b'\'');
                    let end = end.ok_or_else(|| self.error("a key isn't closed"))?;
                    self.at += end + 1;
                    rest[1..end].to_owned()
                }
                _ => {
                    let bare = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
                    let len = rest.find(|c: char| !bare(c)).unwrap_or(rest.len());
                    if len == 0 {
                        return Err(self.error("a key is missing"));
                    }
                    self.at += len;
                    rest[..len].to_owned()
                }
            };
            path.push(part);
            self.blanks();
            if !self.eat(".") {
                return Ok(path);
            }
        }
    }

    /// Reads a `"quoted"` key and decodes its escapes.
    fn basic_key(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut key = String::new();
        loop {
            let c = self.text[self.at..].chars().next();
            let c = c.filter(|c| *c != '\n').ok_or_else(|| self.error("a key isn't closed"))?;
            self.at += c.len_utf8();
            match c {
                '"' => return Ok(key),
                '\\' => {
                    let escape = self.peek().ok_or_else(|| self.error("a key isn't closed"))?;
                    self.at += 1;
                    let digits = match escape {
                        b'u' => 4,
                        b'U' => 8,
                        b'x' => 2,
                        _ => 0,
                    };
                    let decoded = match escape {
                        b'b' => Some('\u{8}'),
                        b't' => Some('\t'),
                        b'n' => Some('\n'),
                        b'f' => Some('\u{c}'),
                        b'r' => Some('\r'),
                        b'e' => Some('\u{1b}'),
                        b'"' => Some('"'),
                        b'\\' => Some('\\'),
                        _ if digits > 0 => {
                            let hex = self.text.get(self.at..self.at + digits).unwrap_or_default();
                            self.at += hex.len();
                            let hex = Some(hex).filter(|hex| {
                                hex.len() == digits && hex.bytes().all(|b| b.is_ascii_hexdigit())
                            });
                            let code = hex.and_then(|hex| u32::from_str_radix(hex, 16).ok());
                            code.and_then(char::from_u32)
                        }
                        _ => None,
                    };
                    key.push(decoded.ok_or_else(|| self.error("a key has a bad escape"))?);
                }
                c => key.push(c),
            }
        }
    }

    /// Steps over a value: up to its comment or line end, at the same depth it started.
    /// Returns where its text ends.
    fn value(&mut self) -> Result<usize, String> {
        let (start, mut depth, mut end) = (self.at, 0_usize, self.at);
        loop {
            match self.peek() {
                None if depth == 0 => return Ok(end),
                None => {
                    self.at = start;
                    return Err(self.error("an array or inline table isn't closed"));
                }
                Some(b'\n' | b'#') if depth == 0 => return Ok(end),
                Some(b'\r') if depth == 0 && self.text[self.at..].starts_with("\r\n") => {
                    return Ok(end);
                }
                Some(b'#') => self.comment(),
                Some(b'"' | b'\'') => {
                    self.string()?;
                    end = self.at;
                }
                Some(b'[' | b'{') => {
                    depth += 1;
                    self.at += 1;
                    end = self.at;
                }
                Some(b']' | b'}') => {
                    let closes = depth.checked_sub(1);
                    depth = closes.ok_or_else(|| self.error("a bracket closes nothing"))?;
                    self.at += 1;
                    end = self.at;
                }
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.at += 1,
                Some(_) => {
                    self.at += self.text[self.at..].chars().next().map_or(1, char::len_utf8);
                    end = self.at;
                }
            }
        }
    }

    /// Steps over a string value of any of the 4 kinds. Inside it, the cursor walks bytes.
    fn string(&mut self) -> Result<(), String> {
        let quote = if self.peek() == Some(b'"') { "\"" } else { "'" };
        let (multi, basic, start) = (quote.repeat(3), quote == "\"", self.at);
        let closed = if self.eat(&multi) {
            loop {
                match self.peek() {
                    None => break false,
                    Some(b'\\') if basic => self.at += 2,
                    _ if self.eat(&multi) => {
                        // Up to 2 more quotes belong to the string.
                        let _ = self.eat(quote) && self.eat(quote);
                        break true;
                    }
                    Some(_) => self.at += 1,
                }
            }
        } else {
            self.at += 1;
            loop {
                match self.peek() {
                    None | Some(b'\n') => break false,
                    Some(b'\\') if basic => self.at += 2,
                    Some(byte) => {
                        self.at += 1;
                        if byte == quote.as_bytes()[0] {
                            break true;
                        }
                    }
                }
            }
        };
        if closed {
            return Ok(());
        }
        self.at = start;
        let kind = if self.eat(&multi) { "a multi-line string" } else { "a string" };
        Err(self.error(&format!("{kind} isn't closed")))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{scan, Item};

    fn key(path: &[&str], value: &str) -> Item {
        Item::Key(path.iter().map(|part| part.to_string()).collect(), value.into())
    }

    fn table(path: &[&str]) -> Item {
        Item::Table(path.iter().map(|part| part.to_string()).collect())
    }

    #[test]
    fn scan_decodes_keys_and_skips_strings() {
        let rows: Vec<(&str, &str, Vec<Item>)> = vec![
            ("bare dotted", "a.b-c_1 = 1\n", vec![key(&["a", "b-c_1"], "1")]),
            ("blanks at dots", " a . \"b.c\" .'d' = 1", vec![key(&["a", "b.c", "d"], "1")]),
            ("\\u escape", "\"zero\\u0064b\" = 1\n", vec![key(&["zerodb"], "1")]),
            ("\\U escape", "\"\\U0001F600\\t\\\"\" = 1\n", vec![key(&["😀\t\""], "1")]),
            ("literal key", "'a\\u0064' = 1\n", vec![key(&["a\\u0064"], "1")]),
            ("header", "[ x . \"y.z\" ]  # t\n", vec![table(&["x", "y.z"])]),
            (
                "array header",
                "[[x.y]]\nk = 1\n",
                vec![table(&["x", "y"]), key(&["x", "y", "k"], "1")],
            ),
            ("CRLF comment", "  # zerodb:end \r\n", vec![Item::Comment("# zerodb:end ".into())]),
            ("hash in a string", "s = \"a # b\" # c\n", vec![key(&["s"], "\"a # b\"")]),
            (
                "non-ASCII",
                "s = \"é\\é\" # ü\nt = é\n",
                vec![key(&["s"], "\"é\\é\""), key(&["t"], "é")],
            ),
            (
                "multi-line basic",
                "s = \"\"\"\n# zerodb:end\n[t]\nx = \\\"\"\" é \"\"\"\"\n",
                vec![key(&["s"], "\"\"\"\n# zerodb:end\n[t]\nx = \\\"\"\" é \"\"\"\"")],
            ),
            (
                "multi-line literal",
                "s = '''\n[t]\n'''\n# c",
                vec![key(&["s"], "'''\n[t]\n'''"), Item::Comment("# c".into())],
            ),
            (
                "multi-line array",
                "a = [\n  [\"m.z\"], # ]\n  { x = \"]\" },\n]\n[t]\n",
                vec![key(&["a"], "[\n  [\"m.z\"], # ]\n  { x = \"]\" },\n]"), table(&["t"])],
            ),
            ("inline table", "m = { z = { c = 'x' } }\n", vec![key(&["m"], "{ z = { c = 'x' } }")]),
        ];
        for (case, text, wanted) in rows {
            let found: Vec<Item> =
                scan(text).unwrap().into_iter().map(|entry| entry.item).collect();
            assert_eq!(found, wanted, "{case}");
        }
        let spans: Vec<_> =
            scan("a = 1\n\n# c\r\n[t]").unwrap().into_iter().map(|entry| entry.span).collect();
        assert_eq!(spans, [0..6, 7..12, 12..15]);
        let errors = [
            ("s = \"abc\n", "a string isn't closed on line 1"),
            ("\ns = '''abc\n", "a multi-line string isn't closed on line 2"),
            ("\n\na = [1,\n", "an array or inline table isn't closed on line 3"),
            ("[a.b\n", "a table header isn't closed on line 1"),
            ("\"a\\q\" = 1", "a key has a bad escape on line 1"),
            ("a 1", "a key has no = on line 1"),
            ("a = 1 2 ]", "a bracket closes nothing on line 1"),
            ("[a] b = 1", "a line has more after its end on line 1"),
        ];
        for (text, wanted) in errors {
            assert_eq!(scan(text).err().as_deref(), Some(wanted), "{text:?}");
        }
    }
}
