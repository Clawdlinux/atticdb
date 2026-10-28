//! `zerodb hook session-start`: the Copilot `sessionStart` hook. It prints a small block of
//! the user's rules, this repo's facts and its recent notes, which the harness adds to the
//! chat. The block's text depends only on memory, so the prompt prefix stays the same.

use crate::connect::Harness;
use crate::mcp::HomeReader;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Most stdin bytes read.
const MAX_INPUT: u64 = 1 << 20;
/// Time for all reads together, under the hook's 5 s timeout. A read needs 2 s left to
/// start, so the first one has 1 s to connect or start the server.
const BUDGET: Duration = Duration::from_secs(3);
const FACTS: u64 = 50;
const TURNS: usize = 5;
const MAX_CONTENT: usize = 300;
const MAX_BLOCK: usize = 6_000;
const HEADER: &str =
    "zerodb memory (snapshot at chat start; call memory_search or fact_list for more)";
const MORE: &str = "(more in zerodb)";

/// Reads the hook's JSON on stdin and prints `{"additionalContext": "<block>"}`, or `{}`
/// when there is nothing to show or on any error. Errors go to stderr. Returns exit code 0.
pub fn session_start() -> u8 {
    let output = match block(std::io::stdin().lock().take(MAX_INPUT)) {
        Ok(Some(block)) => json!({"additionalContext": block}),
        Ok(None) => json!({}),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "zerodb: hook session-start: {error}");
            json!({})
        }
    };
    let _ = writeln!(std::io::stdout(), "{output}");
    0
}

/// The block for the `cwd` in `input`, or `None` when memory has nothing for it.
fn block(input: impl Read) -> Result<Option<String>, String> {
    // Parsing stops at the end of the first value, so an open stdin cannot hang the hook.
    let first = serde_json::Deserializer::from_reader(input).into_iter::<Value>().next();
    let input = first.ok_or("stdin is empty")?;
    let input = input.map_err(|error| format!("stdin is not JSON of at most 1 MiB: {error}"))?;
    let cwd = input["cwd"].as_str().ok_or("stdin has no cwd string")?;
    let repo = repo(Path::new(cwd))?;
    let mut home = HomeReader::new(&repo, BUDGET)?;
    let mut facts = |prefix: String| {
        let list = home.read("hook", "fact_list", &json!({"prefix": prefix, "limit": FACTS}))?;
        let list = list["facts"].as_array().cloned().unwrap_or_default();
        Ok::<Vec<_>, String>(list.iter().map(fact).collect())
    };
    let (rules, this_repo) = (facts("user/".into())?, facts(format!("repo/{repo}/"))?);
    // Turns are kept per agent, and a hook can't tell which harness ran it.
    let mut turns = Vec::new();
    for agent in Harness::ALL.map(Harness::agent) {
        let recent = home.read(agent, "memory_recent", &json!({"n": TURNS}))?;
        turns.extend(recent["turns"].as_array().cloned().unwrap_or_default());
    }
    turns.sort_by_key(|turn| (turn["commit_time_ns"].as_u64(), turn["lsn"].as_u64()));
    let notes = turns[turns.len().saturating_sub(TURNS)..].iter();
    let notes = notes.map(|turn| format!("- {}", line(&turn["content"], MAX_CONTENT))).collect();
    let sections = [
        ("Rules:".to_owned(), rules),
        (format!("This repo ({repo}):"), this_repo),
        (format!("Recent notes ({repo}):"), notes),
    ];
    let mut lines = vec![];
    for (title, entries) in sections.into_iter().filter(|(_, entries)| !entries.is_empty()) {
        lines.push(title);
        lines.extend(entries);
    }
    Ok((!lines.is_empty()).then(|| cap([HEADER.to_owned()].into_iter().chain(lines))))
}

/// The name of the first directory from `cwd` up that holds a `.git` entry, else of `cwd`.
fn repo(cwd: &Path) -> Result<String, String> {
    if !cwd.is_absolute() {
        return Err(format!("cwd {cwd:?} is not absolute"));
    }
    let root = cwd.ancestors().find(|dir| dir.join(".git").symlink_metadata().is_ok());
    let name = root.unwrap_or(cwd).file_name().and_then(|name| name.to_str());
    name.map(str::to_owned).ok_or_else(|| format!("cwd {cwd:?} has no directory name"))
}

fn fact(fact: &Value) -> String {
    format!("- {}: {}", line(&fact["key"], usize::MAX), line(&fact["content"], MAX_CONTENT))
}

/// `text` cut to `max` characters, with newlines turned into spaces.
fn line(text: &Value, max: usize) -> String {
    let text = text.as_str().unwrap_or_default().chars().take(max);
    text.map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

/// Joins `lines`. Past [`MAX_BLOCK`] characters, stops after the last whole line that
/// fits with [`MORE`].
fn cap(lines: impl Iterator<Item = String>) -> String {
    let lines: Vec<String> = lines.collect();
    let all = lines.join("\n");
    if all.chars().count() <= MAX_BLOCK {
        return all;
    }
    let (mut block, budget) = (String::new(), MAX_BLOCK - MORE.len() - 1);
    let mut used = 0;
    for line in lines {
        used += line.chars().count() + usize::from(!block.is_empty());
        if used > budget {
            break;
        }
        if !block.is_empty() {
            block.push('\n');
        }
        block.push_str(&line);
    }
    block + "\n" + MORE
}
