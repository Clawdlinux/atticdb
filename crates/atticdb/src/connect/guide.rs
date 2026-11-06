//! The guide: AtticDB's block in the instructions file an app reads from its config dir. Codex
//! and Gemini CLI get one, each by its [`Guide`]: [`CODEX`] and [`GEMINI`].
//!
//! The block is the MCP server's instructions between 2 marker lines, as HTML comments. A
//! marker counts only as a whole line outside fenced code. A file that mentions AtticDB
//! anywhere else outside fenced code is the user's own guide, so AtticDB doesn't write to it.

use crate::connect::{add_or_update, planned, read_up_to, write, Harness, Line, Mode, State};
use std::ops::Range;
use std::path::{Path, PathBuf};

/// An app's guide: the file it goes in, its markers, and the size the app may cut it at.
pub(super) struct Guide {
    /// The app. Its name goes in the advice to re-run `atticdb connect`.
    harness: Harness,
    /// The file the app reads in its config dir.
    file: &'static str,
    /// The file the app reads in place of `file` when it holds more than whitespace. Codex
    /// only.
    replaced_by: Option<&'static str>,
    /// The begin line AtticDB writes. It names `atticdb disconnect <app>`.
    begin: &'static str,
    /// The begin line AtticDB writes after adding the line end the file lacked, so `disconnect`
    /// removes that line end too.
    begin_added: &'static str,
    /// The size in bytes past which the app may cut its instructions, so the guide may not
    /// reach the model. `None` when the app documents no limit.
    limit: Option<usize>,
}

/// Codex's guide. Codex reads `AGENTS.override.md` in its config dir when it holds more than
/// whitespace, else `AGENTS.md`, and only that 1 file. Codex 0.147 didn't pass MCP server
/// instructions to the model in `codex exec`, so the guide goes there. The limit is Codex's
/// default `project_doc_max_bytes`.
pub(super) const CODEX: Guide = Guide {
    harness: Harness::Codex,
    file: "AGENTS.md",
    replaced_by: Some("AGENTS.override.md"),
    begin: "<!-- atticdb:begin v1. atticdb disconnect codex removes this block. -->",
    begin_added: "<!-- atticdb:begin v1. atticdb disconnect codex removes this block and the \
                  line end it added above. -->",
    limit: Some(32 * 1024),
};

/// Gemini CLI's guide, in the global `GEMINI.md` of its config dir. Gemini CLI's docs give
/// no size limit for its context files, so none is checked.
pub(super) const GEMINI: Guide = Guide {
    harness: Harness::Gemini,
    file: "GEMINI.md",
    replaced_by: None,
    begin: "<!-- atticdb:begin v1. atticdb disconnect gemini removes this block. -->",
    begin_added: "<!-- atticdb:begin v1. atticdb disconnect gemini removes this block and the \
                  line end it added above. -->",
    limit: None,
};

/// The end line of AtticDB's block.
const END: &str = "<!-- atticdb:end -->";
/// The block's heading.
const TITLE: &str = "## AtticDB memory";
/// The part's name in its line.
const PART: &str = "atticdb guide";
/// The most of a file read.
const MAX_READ: u64 = 1 << 20;

/// The file the app reads in its config dir `dir`. For Codex, `AGENTS.override.md` unless
/// it is missing, holds only whitespace, or isn't UTF-8, as Codex then reads `AGENTS.md`.
/// An override AtticDB can't read, such as a symlink, stays the target, so its part is
/// skipped.
fn target(guide: &Guide, dir: &Path) -> PathBuf {
    let Some(replaced_by) = guide.replaced_by else { return dir.join(guide.file) };
    let empty =
        |bytes: &[u8]| std::str::from_utf8(bytes).map_or(true, |text| text.trim().is_empty());
    match read_up_to(&dir.join(replaced_by), MAX_READ) {
        Ok(None) => dir.join(guide.file),
        Ok(Some((bytes, _))) if empty(&bytes) => dir.join(guide.file),
        _ => dir.join(replaced_by),
    }
}

/// AtticDB's block with `eol` line ends. With `added`, its begin line says `connect` added
/// the line end before it.
fn block(guide: &Guide, added: bool, eol: &str) -> String {
    let begin = if added { guide.begin_added } else { guide.begin };
    [begin, TITLE, crate::mcp::INSTRUCTIONS, END].map(|line| format!("{line}{eol}")).concat()
}

/// A line of a file: its bytes with its line end, and its text without.
struct Row<'a> {
    span: Range<usize>,
    text: &'a str,
}

/// The lines of `text`.
fn rows(text: &str) -> Vec<Row<'_>> {
    let (mut rows, mut at) = (Vec::new(), 0);
    while at < text.len() {
        let end = text[at..].find('\n').map_or(text.len(), |n| at + n + 1);
        let line = &text[at..end];
        // Only a `\r\n` pair is a line end. A bare `\r` stays in the text.
        let line = line.strip_suffix("\r\n").or_else(|| line.strip_suffix('\n')).unwrap_or(line);
        rows.push(Row { span: at..end, text: line });
        at = end;
    }
    rows
}

/// A code fence line: up to 3 spaces, then 3 or more backticks or tildes. Returns the fence
/// character, how many, and the text after them. Only `' '` counts as indent, as in
/// CommonMark, where a tab is 4 columns and so too much.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let mark = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let after = rest.trim_start_matches(mark);
    let len = rest.len() - after.len();
    (len >= 3).then_some((mark, len, after))
}

/// The lines of `text` outside fenced code, without the fence lines, and whether a fence is
/// still open at the end. As in CommonMark, a fence that never closes runs to the end, and
/// only spaces and tabs may follow a closing fence, not other Unicode whitespace.
fn plain(text: &str) -> (Vec<Row<'_>>, bool) {
    let (mut open, mut plain) = (None, Vec::new());
    for row in rows(text) {
        match (open, fence(row.text)) {
            // A backtick fence's info string can't hold a backtick.
            (None, Some((mark, len, after))) if mark == '~' || !after.contains('`') => {
                open = Some((mark, len));
            }
            (None, _) => plain.push(row),
            (Some((mark, len)), Some((closer, n, after)))
                if closer == mark && n >= len && after.chars().all(|c| matches!(c, ' ' | '\t')) =>
            {
                open = None;
            }
            (Some(_), _) => {}
        }
    }
    (plain, open.is_some())
}

/// What every AtticDB begin line starts with, this version's or another's.
const BEGIN_START: &str = "<!-- atticdb:begin";

/// AtticDB's block among the `rows` outside fenced code: from the start of its begin line to
/// the end of its end line. Markers are lines that are exactly a begin line AtticDB writes for
/// `guide`'s app, or [`END`]. A line that only looks like one, such as an indented marker, one
/// with trailing spaces or another version's or app's begin line, is an error, as is a pair
/// that doesn't match up.
fn find_block(guide: &Guide, rows: &[Row<'_>]) -> Result<Option<Range<usize>>, String> {
    let (mut begins, mut ends) = (Vec::new(), Vec::new());
    for row in rows {
        match row.text {
            text if text == guide.begin || text == guide.begin_added => {
                begins.push(row.span.clone());
            }
            END => ends.push(row.span.clone()),
            text if text.trim().starts_with(BEGIN_START) || text.trim() == END => {
                return Err(
                    "it has a line that looks like an atticdb marker but isn't exactly one".into(),
                );
            }
            _ => {}
        }
    }
    match (begins.as_slice(), ends.as_slice()) {
        ([], []) => Ok(None),
        ([begin], [end]) if begin.start < end.start => Ok(Some(begin.start..end.end)),
        ([], _) => Err(format!("it has a line {END} without an atticdb begin line")),
        ([_], []) => Err(format!("it has an atticdb begin line without a line {END}")),
        _ => Err(format!(
            "its atticdb begin lines and lines {END} don't pair up: each must come once, in that \
             order"
        )),
    }
}

/// Whether one of the `rows` outside fenced code, and outside `block`, mentions AtticDB in
/// any case.
fn mentions(rows: &[Row<'_>], block: Option<&Range<usize>>) -> bool {
    let mut outside =
        rows.iter().filter(|row| block.is_none_or(|block| !block.contains(&row.span.start)));
    outside.any(|row| row.text.to_ascii_lowercase().contains("atticdb"))
}

/// The line end of `text`'s first line, `\n` when it has none.
fn eol(text: &str) -> &'static str {
    match text.find('\n') {
        Some(at) if text[..at].ends_with('\r') => "\r\n",
        _ => "\n",
    }
}

/// The line end of `block`'s begin line, which `connect` also put before the block.
fn block_eol(block: &str) -> &'static str {
    match block.split('\n').next() {
        Some(begin) if begin.ends_with('\r') => "\r\n",
        _ => "\n",
    }
}

/// `text` without `block`, and without the blank line `connect` put before it. When nothing
/// follows the block, the line end `connect` added before it goes too, so the file is back
/// as it was. Only the bytes `connect` inserted go: the block's own line end, after a line
/// end of the file's.
fn without(guide: &Guide, text: &str, block: &Range<usize>) -> String {
    let (head, tail, ours) = (&text[..block.start], &text[block.end..], &text[block.clone()]);
    let eol = block_eol(ours);
    let mut head = head.strip_suffix(eol).filter(|shorter| shorter.ends_with('\n')).unwrap_or(head);
    if ours.starts_with(guide.begin_added) && tail.is_empty() {
        head = head.strip_suffix(eol).unwrap_or(head);
    }
    format!("{head}{tail}")
}

/// How to remove AtticDB's block from `shown` by hand.
fn remove_by_hand(shown: impl std::fmt::Display) -> String {
    format!("Remove the atticdb marker lines, and the lines between them, from {shown} by hand")
}

/// Puts AtticDB's block in the file the app reads in its config dir `dir`, after 1 blank
/// line, or updates it in place. A file with CRLF line ends gets CRLF. A file without a final
/// line end gets one, and the block says so. A missing file is created at mode 0600. A file
/// that mentions AtticDB outside the block is kept as it is. In Plan mode, it writes nothing.
/// Returns the part's line, and a warning when the file ends up over the app's limit.
pub(super) fn add(guide: &Guide, dir: &Path, mode: Mode) -> (Line, Option<String>) {
    let path = target(guide, dir);
    let app = guide.harness.name();
    let shown = path.display();
    let skipped =
        |why: String| (Line::new(State::Skipped, format!("{PART}: skipped, {why}")), None);
    let by_hand = format!("Add this to the end of {shown} by hand:\n{}", block(guide, false, "\n"));
    let by_hand = by_hand.trim_end();
    let old = match read_up_to(&path, MAX_READ) {
        Ok(old) => old,
        Err(why) => return skipped(format!("{shown} {why}. {by_hand}")),
    };
    let text = match &old {
        Some((bytes, _)) => match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return skipped(format!("{shown} is not UTF-8. {by_hand}")),
        },
        None => "",
    };
    let (rows, open) = plain(text);
    let found = match find_block(guide, &rows) {
        Ok(found) => found,
        Err(why) => {
            return skipped(format!(
                "{shown}: {why}. Nothing changed. {}, then re-run atticdb connect {app}",
                remove_by_hand(&shown)
            ))
        }
    };
    if mentions(&rows, found.as_ref()) {
        let said = match mode {
            Mode::Plan => format!("{shown}: keep the user's guide, which mentions atticdb"),
            Mode::Apply => format!("{PART}: kept, {shown} already mentions atticdb"),
        };
        return (Line::new(State::Kept, said), None);
    }
    let (new, state) = match &found {
        Some(span) => {
            // The block keeps its own line ends, which may not be the file's first line's.
            let ours = &text[span.clone()];
            let wanted = block(guide, ours.starts_with(guide.begin_added), block_eol(ours));
            if text[span.clone()] == wanted {
                (text.to_owned(), State::Unchanged)
            } else {
                let (head, tail) = (&text[..span.start], &text[span.end..]);
                (format!("{head}{wanted}{tail}"), State::Updated)
            }
        }
        None if open => {
            return skipped(format!(
                "{shown} has a code fence that never closes, so a block after it would be code. \
                 Nothing changed. Close the fence, then re-run atticdb connect {app}"
            ))
        }
        None => {
            let eol = eol(text);
            let added = !text.is_empty() && !text.ends_with('\n');
            let mut new = text.to_owned();
            if !text.is_empty() {
                new.push_str(if added { eol } else { "" });
                new.push_str(eol);
            }
            new.push_str(&block(guide, added, eol));
            (new, add_or_update(false))
        }
    };
    // AtticDB must be able to read the file again, to update or remove its block.
    if state != State::Unchanged && new.len() as u64 > MAX_READ {
        return skipped(format!(
            "{shown} would be over 1 MiB with atticdb's block, and atticdb reads at most 1 MiB. \
             Nothing changed. {by_hand}"
        ));
    }
    let over = guide.limit.filter(|limit| new.len() > *limit);
    let warning = over.map(|limit| {
        format!(
            "{shown} is {} bytes, over {} KiB. {} may cut its instructions there, so atticdb's \
             guide may not reach the model",
            new.len(),
            limit / 1024,
            guide.harness.title()
        )
    });
    let line = match (state, mode) {
        (_, Mode::Plan) => planned(&path, PART, state),
        (State::Unchanged, Mode::Apply) => Line::new(state, format!("{PART}: unchanged ({shown})")),
        (_, Mode::Apply) => match write(&path, old.as_ref(), new.as_bytes()) {
            Ok(backup) => {
                let saved = backup.map(|backup| format!(", backup: {}", backup.display()));
                let verb = if state == State::Added { "added" } else { "updated" };
                let saved = saved.unwrap_or_default();
                Line::new(state, format!("{PART}: {verb} ({shown}{saved})"))
            }
            Err(why) => Line::new(State::Failed, format!("{PART}: failed, {why}. {by_hand}")),
        },
    };
    (line, warning)
}

/// Removes AtticDB's block, and the blank line `connect` put before it, from each file the
/// app may read in its config dir `dir`: both for Codex. Every other byte stays. Returns 1
/// line per file that held AtticDB's markers, or 1 line for the file the app reads when none
/// did.
pub(super) fn remove(guide: &Guide, dir: &Path) -> Vec<Line> {
    let read_now = target(guide, dir);
    let mut lines = Vec::new();
    for path in guide.replaced_by.into_iter().chain([guide.file]).map(|file| dir.join(file)) {
        lines.extend(remove_from(guide, &path, path == read_now));
    }
    if lines.is_empty() {
        lines.push(Line::new(
            State::Unchanged,
            format!("{PART}: not found ({})", read_now.display()),
        ));
    }
    lines
}

/// Removes AtticDB's block from the file at `path`. `None` when it has no AtticDB markers. A
/// file AtticDB can't read gets a line only when the app reads it, as AtticDB never wrote
/// there.
fn remove_from(guide: &Guide, path: &Path, read_now: bool) -> Option<Line> {
    let shown = path.display();
    let unread = |why: String| {
        read_now.then(|| {
            let said = format!(
                "{PART}: left {shown} alone, as it {why}. If it holds atticdb's block, {}",
                remove_by_hand(&shown).replacen("Remove", "remove", 1)
            );
            Line::new(State::Unchanged, said)
        })
    };
    let old = match read_up_to(path, MAX_READ) {
        Ok(old) => old?,
        Err(why) => return unread(why),
    };
    let Ok(text) = std::str::from_utf8(&old.0) else { return unread("is not UTF-8".into()) };
    let block = match find_block(guide, &plain(text).0) {
        Ok(block) => block?,
        Err(why) => {
            let said = format!(
                "{PART}: skipped, {shown}: {why}. Nothing changed. {}",
                remove_by_hand(&shown)
            );
            return Some(Line::new(State::Skipped, said));
        }
    };
    let new = without(guide, text, &block);
    Some(match write(path, Some(&old), new.as_bytes()) {
        Ok(backup) => {
            let saved = backup.map(|backup| format!(", backup: {}", backup.display()));
            let said = format!("{PART}: removed ({shown}{})", saved.unwrap_or_default());
            Line::new(State::Removed, said)
        }
        Err(why) => {
            Line::new(State::Failed, format!("{PART}: failed, {why}. {}", remove_by_hand(&shown)))
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{block, find_block, mentions, plain, without, CODEX, GEMINI};

    #[test]
    fn markers_and_mentions_count_only_outside_fenced_code() {
        let block = format!("{}\nx\n<!-- atticdb:end -->\n", CODEX.begin);
        let rows = [
            (format!("a\n\n{block}"), true, false),
            (format!("```md\n{block}```\n"), false, false),
            (format!("~~~~\n{block}~~~\natticdb\n~~~~\n"), false, false),
            (format!(" ```\n{block}\n   ```  \nAtticDB rules\n"), false, true),
            (format!("    ```\n{block}"), true, false),
            (format!("``` a`b\n{block}"), true, false),
            (format!("x {}\n", CODEX.begin), false, true),
        ];
        for (text, found, mentioned) in rows {
            let (rows, _) = plain(&text);
            let block = find_block(&CODEX, &rows).unwrap();
            assert_eq!(block.is_some(), found, "{text:?}");
            assert_eq!(mentions(&rows, block.as_ref()), mentioned, "{text:?}");
        }
        let (_, open) = plain("```\natticdb\n");
        assert!(open);
    }

    /// Each app's block names its own `atticdb disconnect`. Another app's begin line only looks
    /// like a marker, so it is an error, not a block to replace or remove.
    #[test]
    fn each_app_finds_only_its_own_markers() {
        for (ours, theirs, app) in [(&CODEX, &GEMINI, "codex"), (&GEMINI, &CODEX, "gemini")] {
            let text = format!("a\n\n{}", block(ours, false, "\n"));
            assert!(text.contains(&format!("atticdb disconnect {app} removes this block")));
            let (rows, _) = plain(&text);
            assert_eq!(find_block(ours, &rows).unwrap(), Some(3..text.len()), "{app}");
            assert!(find_block(theirs, &rows).is_err(), "{app}");
            let added = format!("a\n{}", block(ours, true, "\n"));
            let (rows, _) = plain(&added);
            let span = find_block(ours, &rows).unwrap().unwrap();
            assert_eq!(without(ours, &added, &span), "a", "{app}");
        }
    }
}
