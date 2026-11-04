//! `atticdb tree [prefix]`: the fact keys as a tree of `/` path segments. Each folder shows
//! how many current facts are under it and the date of the newest change. Values show only
//! with `--content`. It reads with `fact_list`, so deleted and forgotten keys never show.

use crate::client::Client;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// Most keys read. Past it the tree says it shows only the first ones.
const MAX_KEYS: usize = 10_000;
/// Most `fact_list` pages read. A page may skip deleted keys and hold none, so this bounds a
/// store with many of them.
const MAX_PAGES: usize = 1_000;
/// Keys per page: `fact_list`'s most.
const PAGE_KEYS: usize = 200;
/// Bytes per page, values included.
const PAGE_BYTES: u64 = 1 << 20;
/// Longest first line `--content` shows, in characters.
const LINE: usize = 80;
/// The depth with no prefix and no `--depth`.
const TOP_DEPTH: usize = 2;
/// What the text tree says last when `--depth` hid levels.
const COLLAPSED: &str = "Deeper levels show as counts. Pass --depth <n> to see more.";

/// A folder or a key. A folder's name ends in `/`. A key's never does.
#[derive(Default)]
struct Node {
    facts: u64,
    newest_ns: u64,
    /// The fact, for a key.
    fact: Option<Value>,
    /// By name, in key order. `a` and `a/` can both be there.
    children: BTreeMap<String, Node>,
}

impl Node {
    fn add(&mut self, segments: &[&str], fact: &Value) {
        self.facts += 1;
        self.newest_ns = self.newest_ns.max(fact["commit_time_ns"].as_u64().unwrap_or(0));
        match segments {
            [] => self.fact = Some(fact.clone()),
            [leaf] => self.children.entry((*leaf).to_owned()).or_default().add(&[], fact),
            [folder, rest @ ..] => {
                self.children.entry(format!("{folder}/")).or_default().add(rest, fact);
            }
        }
    }
}

/// Runs `atticdb tree`. `flags` holds `atticdb mcp`'s connection flags, `--depth`, `--content`
/// and `--json`. Prints the tree as text, or as 1 JSON line with `--json`. With no facts it
/// prints a hint that says how to save one. Exits 0, 1 when a read failed, or 2 for a bad flag.
pub fn command(prefix: Option<String>, mut flags: HashMap<String, String>) -> u8 {
    let prefix = prefix.filter(|prefix| !prefix.is_empty());
    let depth = match flags.remove("--depth").map(|text| text.parse::<usize>()) {
        Some(Ok(depth)) => Some(depth),
        Some(Err(_)) => {
            eprintln!("atticdb: --depth must be a whole number");
            return 2;
        }
        None => prefix.is_none().then_some(TOP_DEPTH),
    };
    let (content, as_json) =
        (flags.remove("--content").is_some(), flags.remove("--json").is_some());
    let prefix = prefix.unwrap_or_default();
    let (facts, truncated) = match Client::new(flags).and_then(|client| read(client, &prefix)) {
        Ok(read) => read,
        Err(message) => {
            eprintln!("atticdb: {message}");
            return 1;
        }
    };
    // Paths below the prefix up to its last `/`, so `repo/at` shows `atticdb/`.
    let base = &prefix[..prefix.rfind('/').map_or(0, |end| end + 1)];
    let mut root = Node::default();
    for fact in &facts {
        let key = fact["key"].as_str().unwrap_or_default();
        let rest = key.strip_prefix(base).unwrap_or(key);
        root.add(&rest.split('/').collect::<Vec<_>>(), fact);
    }
    let hint = (root.facts == 0).then(|| empty_hint(&prefix));
    if as_json {
        let nodes = json_nodes(&root, base, 1, depth, content);
        let newest = (root.facts > 0).then_some(root.newest_ns);
        let tree = json!({"prefix": prefix, "facts": root.facts, "newest_ns": newest,
                          "truncated": truncated, "nodes": nodes});
        println!("{tree}");
        if let Some(hint) = hint {
            eprintln!("{hint}");
        }
    } else if let Some(hint) = hint {
        println!("{hint}");
    } else {
        let label = if prefix.is_empty() { "(all keys)" } else { &prefix };
        let mut lines = vec![format!("{}  {}", shown(label), folder_stats(&root))];
        let mut collapsed = false;
        text_lines(&root, "", 1, depth, content, &mut lines, &mut collapsed);
        if collapsed {
            lines.push(COLLAPSED.into());
        }
        println!("{}", lines.join("\n"));
    }
    if truncated {
        let shown = thousands(facts.len());
        eprintln!("atticdb: showing the first {shown} keys. Pass a longer prefix to see the rest.");
    }
    0
}

/// Reads the current facts under `prefix`, in key order, a page at a time. Stops after
/// [`MAX_KEYS`] keys or [`MAX_PAGES`] pages. Returns the facts and whether more were left.
fn read(mut client: Client, prefix: &str) -> Result<(Vec<Value>, bool), String> {
    let (mut facts, mut args) = (Vec::new(), json!({"prefix": prefix, "max_bytes": PAGE_BYTES}));
    for _ in 0..MAX_PAGES {
        args["limit"] = json!(PAGE_KEYS.min(MAX_KEYS - facts.len()));
        let result = client.call("fact_list", &args).unwrap_or_default();
        if result["isError"] == true {
            return Err(result["content"][0]["text"].as_str().unwrap_or_default().to_owned());
        }
        let page = &result["structuredContent"];
        facts.extend(page["facts"].as_array().cloned().unwrap_or_default());
        match page["next"].as_str() {
            None => return Ok((facts, false)),
            Some(_) if facts.len() >= MAX_KEYS => return Ok((facts, true)),
            Some(next) => args["after"] = json!(next),
        }
    }
    Ok((facts, true))
}

/// What `atticdb tree` prints when no fact is under `prefix`, with a command that saves one.
fn empty_hint(prefix: &str) -> String {
    let atticdb = crate::setup::typed();
    let (start, key) = match prefix {
        "" => ("No facts yet.".to_owned(), "user/style".to_owned()),
        _ if prefix.ends_with('/') => {
            (format!("No facts under {prefix:?}."), format!("{prefix}rule"))
        }
        _ => (format!("No facts under {prefix:?}."), format!("{prefix}/rule")),
    };
    format!("{start} Save one: {atticdb} fact set --key {} --content \"...\"", quoted(&key))
}

/// `text` as typed in a shell: as is when every character is plain, else single-quoted.
fn quoted(text: &str) -> String {
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c)) {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// The text lines of `node`'s children at `depth`, drawn with ASCII. A folder at the last
/// depth shows its count only and sets `collapsed`.
fn text_lines(
    node: &Node,
    indent: &str,
    depth: usize,
    max: Option<usize>,
    content: bool,
    lines: &mut Vec<String>,
    collapsed: &mut bool,
) {
    if max.is_some_and(|max| depth > max) {
        *collapsed |= !node.children.is_empty();
        return;
    }
    let last = node.children.len().saturating_sub(1);
    for (i, (name, child)) in node.children.iter().enumerate() {
        let (branch, more) = if i == last { ("`-- ", "    ") } else { ("|-- ", "|   ") };
        let stats = match &child.fact {
            Some(fact) if content => format!("{}  {}", date(child.newest_ns), first_line(fact)),
            Some(_) => date(child.newest_ns),
            None => folder_stats(child),
        };
        lines.push(format!("{indent}{branch}{}  {stats}", shown(name)));
        if child.fact.is_none() {
            let indent = format!("{indent}{more}");
            text_lines(child, &indent, depth + 1, max, content, lines, collapsed);
        }
    }
}

/// `node`'s children at `depth` as JSON, or null past `max`. A folder has `children`. A key
/// has `lsn` instead, and `content` when `content` is set.
fn json_nodes(node: &Node, path: &str, depth: usize, max: Option<usize>, content: bool) -> Value {
    if max.is_some_and(|max| depth > max) {
        return Value::Null;
    }
    let nodes = node.children.iter().map(|(name, child)| {
        let path = format!("{path}{name}");
        let mut out = json!({"name": name, "path": path, "facts": child.facts,
                             "newest_ns": child.newest_ns});
        match &child.fact {
            Some(fact) => {
                out["lsn"] = fact["lsn"].clone();
                if content {
                    out["content"] = json!(first_line(fact));
                }
            }
            None => out["children"] = json_nodes(child, &path, depth + 1, max, content),
        }
        out
    });
    Value::Array(nodes.collect())
}

/// A folder's count and newest change, as `3 facts, newest 2026-11-04`.
fn folder_stats(node: &Node) -> String {
    let plural = if node.facts == 1 { "" } else { "s" };
    format!("{} fact{plural}, newest {}", thousands(node.facts as usize), date(node.newest_ns))
}

/// The UTC date of a commit time, as `2026-11-04`.
fn date(ns: u64) -> String {
    crate::export::utc(ns).chars().take(10).collect()
}

/// The first line of a fact's value, at most [`LINE`] characters, ending in `...` when cut.
fn first_line(fact: &Value) -> String {
    let content = fact["content"].as_str().unwrap_or_default();
    let line = content.lines().next().unwrap_or_default();
    if line.chars().count() <= LINE {
        return shown(line);
    }
    let cut: String = line.chars().take(LINE - 3).collect();
    format!("{}...", shown(&cut))
}

/// `text` with control characters escaped, so a key or value can't drive the terminal.
fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// `n` with commas between thousands, as `10,000`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_format_counts_lines_and_keys() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(MAX_KEYS), "10,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        let long = json!({"content": format!("{}\nline 2", "é".repeat(LINE + 1))});
        assert_eq!(first_line(&long), format!("{}...", "é".repeat(LINE - 3)));
        assert_eq!(first_line(&json!({"content": "a\u{1b}[2J"})), "a\\u{1b}[2J");
        assert_eq!(quoted("repo/x/rule"), "repo/x/rule");
        assert_eq!(quoted("it's here"), r"'it'\''s here'");
        assert_eq!(date(0), "1970-01-01");
    }
}
