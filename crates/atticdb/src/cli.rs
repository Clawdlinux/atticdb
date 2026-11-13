//! The command line's help screen, error messages and logo. These are pure functions, so main
//! only picks the stream and prints.

/// Every command and flag. `atticdb help --all` prints it.
pub const USAGE: &str = "usage:
  atticdb --version | -V
  atticdb init --config <path> --tenant <name>
  atticdb serve --config <path> [--data-dir <dir>] [--listen <addr>|none] [--unix-socket <path>]
               [--unix-socket-group <gid or name>] [--exit-on-poison] [--log-requests]
  atticdb recover --truncate-at <lsn> --data-dir <dir>
  atticdb stats --url unix:///path | --data-dir <dir>
  atticdb load --url unix:///path|http://127.0.0.1:<port> [--token-file <path>] [--writers <n>]
              [--turns <n>] [--batch <n>] [--size <bytes>] [--think <ms>]
  atticdb verify --data-dir <dir> [--scratch <dir>]
  atticdb dump --data-dir <dir> [--scratch <dir>] [--from <lsn>] [--limit <n>]
  atticdb mcp <connect>
  atticdb setup [--yes] [--harness <list>] [--no-hook] [--notes ask|auto] [--dry-run]
  atticdb doctor [--harness <list>]
  atticdb upgrade [--yes] [--version <v>]
  atticdb connect <vscode|copilot> [--force] [--dry-run]
  atticdb connect <antigravity|claude-desktop> [--force] [--dry-run]
  atticdb connect <claude-code|codex|gemini> [--force] [--no-hook] [--notes ask|auto] [--dry-run]
  atticdb disconnect <vscode|copilot|claude-code|codex|gemini|antigravity|claude-desktop>
  atticdb hook <session-start|post-compact> [--harness <name>] [--home <dir>]
  atticdb studio [--url unix:///path|http://127.0.0.1:<port>] [--data-dir <dir>] [--config <path>]
                [--tenant <name>] [--token-file <path>] [--no-open] [--allow-forget]
  atticdb append <connect> --content <text> [--role user|assistant|tool]
  atticdb recent <connect> [--n <n>] [--max-bytes <n>]
  atticdb search <connect> --query <words> [--order relevance|newest] [--limit <n>]
                [--before <lsn>] [--max-bytes <n>]
  atticdb fact set <connect> --key <key> (--content <text> | --delete)
  atticdb fact get <connect> --key <key>
  atticdb fact history <connect> --key <key> [--before <lsn>] [--max-bytes <n>]
  atticdb fact list <connect> --prefix <prefix> [--after <key>] [--limit <n>] [--max-bytes <n>]
  atticdb tree [<prefix>] <connect> [--depth <n>] [--content] [--json]
  atticdb file save <connect> --path <file> --about <text>
  atticdb file get <connect> --path <file or folder> [--versions]
  atticdb forget session <connect> --agent <a> --session <s> [--up-to-lsn <n>]
  atticdb forget key <connect> --key <key> [--up-to-lsn <n>]
  atticdb forget file <connect> --path <file> [--up-to-lsn <n>]
  atticdb forget lsns <connect> --lsns <n,n,...> [--up-to-lsn <n>]
  atticdb export <dir> [--url <url>] [--data-dir <dir>] [--config <path>] [--tenant <name>]
                 [--token-file <path>] [--max-bytes <n>]
  <connect>: [--url unix:///path|http://127.0.0.1:<port>] [--data-dir <dir>] [--config <path>]
             [--tenant <name>] [--token-file <path>] [--agent <name>] [--session <name>]
             [--retry-deadline <n>s|<n>ms]
  The token comes from --token-file, ATTICDB_TOKEN, or the config entry for --tenant.
  With no --url, --data-dir, --config, --tenant or --token-file, the home is used and
  ATTICDB_TOKEN is ignored. ATTICDB_HOME, when set, is the home. It must be an absolute path.
  --agent defaults to mcp. --session defaults to the repo of the current directory, named
  after the folder holding .git. A worktree takes its main repo's name.
  append, recent, search, fact and file print one JSON line. They exit 1 when nothing was written,
  3 when a write may or may not be saved.
  search ranks by relevance. --order newest needs every word and pages with --before.
  tree prints fact keys as a tree of / segments, with counts and the newest change date.
  With no <prefix>, --depth defaults to 2. --content adds each value's first line. --json
  prints 1 JSON line. Deleted and forgotten keys never show.
  forget hides records up to --up-to-lsn, or up to the forget itself. It exits like append.
  Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159).
  What stays visible: https://github.com/Clawdlinux/atticdb#what-a-forget-leaves-visible
  forget session needs --agent and --session. The other forget verbs take neither.
  export writes sessions/<agent>/<session>.md at 1 LSN into <dir>, which must not exist.
  It writes facts.md only when at least 1 fact is left. Forgotten turns show as placeholders.
  Forgotten facts are left out.
  studio serves a read-only page on 127.0.0.1 and opens it. With --allow-forget it can also forget
  sessions and fact keys. It runs until Ctrl-C.
  setup finds VS Code, Copilot CLI, Claude Code, Codex, Gemini CLI, Antigravity and Claude
  Desktop. It asks which to connect and shows the plan. Then it connects them and checks each.
  See atticdb setup --help.
  doctor checks the install. It writes nothing but the daily update check's cache. See
  atticdb doctor --help.
  upgrade installs the latest release, or --version, over this binary with the release's
  install.sh. It keeps the old binary as atticdb.prev. Then it stops the server and runs
  doctor. It asks first, unless --yes. Without a terminal on stdin, it needs --yes.";

/// How much color a stream gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// No escape bytes at all.
    Plain,
    /// 256-color escapes.
    Ansi256,
    /// 24-bit color escapes.
    TrueColor,
}

/// The app names `connect` and `disconnect` take, in `Harness::ALL` order.
pub const APPS: [&str; 7] =
    ["vscode", "copilot", "claude-code", "codex", "gemini", "antigravity", "claude-desktop"];

const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

/// The mark from the site's `atticdb-mark.svg`, in half blocks. Rows 0 to 4 are the roof.
const LOGO: [&str; 8] = [
    "       ▄▄",
    "     ▄████▄",
    "   ▄███▀▀███▄",
    "▄████▀    ▀████▄",
    " ▀▀▀        ▀▀▀",
    "     ██████",
    "     ██████",
    "     ▀▀▀▀▀▀",
];
const LOGO_WIDTH: usize = 16;
const ROOF_ROWS: usize = 5;

const TAGLINE: &str = "local memory for coding agents";
const SITE: &str = "https://atticdb.com";
const MORE: &str = "  more:  atticdb --help\n";

/// Color is on only for a terminal, with `NO_COLOR` unset or empty, off Windows. `COLORTERM`
/// set to `truecolor` or `24bit` picks 24-bit color. Anything else gets 256 colors.
pub fn style(
    is_terminal: bool,
    no_color: Option<&str>,
    colorterm: Option<&str>,
    windows: bool,
) -> Style {
    if !is_terminal || windows || no_color.is_some_and(|value| !value.is_empty()) {
        Style::Plain
    } else if matches!(colorterm, Some("truecolor" | "24bit")) {
        Style::TrueColor
    } else {
        Style::Ansi256
    }
}

/// The style for `stream`, from `NO_COLOR`, `COLORTERM` and the OS.
pub fn style_for(stream: &impl std::io::IsTerminal) -> Style {
    let var = |name| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned());
    let (no_color, colorterm) = (var("NO_COLOR"), var("COLORTERM"));
    style(stream.is_terminal(), no_color.as_deref(), colorterm.as_deref(), cfg!(windows))
}

/// The foreground escape for a color. It's empty when `style` is plain.
fn fg(style: Style, rgb: (u8, u8, u8), index: u8) -> String {
    match style {
        Style::Plain => String::new(),
        Style::Ansi256 => format!("\x1b[38;5;{index}m"),
        Style::TrueColor => format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2),
    }
}

fn cream(style: Style) -> String {
    fg(style, (237, 230, 216), 254)
}

fn lime(style: Style) -> String {
    fg(style, (200, 240, 60), 191)
}

/// Wraps `text` in `start` and a reset. Plain text stays as it is.
fn paint(style: Style, start: &str, text: &str) -> String {
    match style {
        Style::Plain => text.to_string(),
        _ => format!("{start}{text}{RESET}"),
    }
}

/// The logo with the name, tagline and site beside it. Plain output skips the logo for 1 line.
/// It ends in a newline.
pub fn header(style: Style, version: &str) -> String {
    if let Style::Plain = style {
        return format!("atticdb {version}, {TAGLINE}\n");
    }
    let name = paint(style, &format!("{BOLD}{}", lime(style)), "atticdb");
    let beside = [format!("{name} {version}"), TAGLINE.to_string(), SITE.to_string()];
    let mut out = String::new();
    for (index, row) in LOGO.iter().enumerate() {
        let color = if index < ROOF_ROWS { cream(style) } else { lime(style) };
        out += &paint(style, &color, row);
        if let Some(text) = index.checked_sub(1).and_then(|line| beside.get(line)) {
            let pad = LOGO_WIDTH - row.chars().count() + 3;
            out += &format!("{:pad$}{text}", "");
        }
        out.push('\n');
    }
    out
}

/// The help screen: the header, the 3 command groups and a pointer to every flag. It ends in a
/// newline.
pub fn help(style: Style, version: &str) -> String {
    let heading = |text: &str| paint(style, BOLD, text);
    let command = |text: &str| paint(style, &lime(style), text);
    let started = [
        (command("setup"), 5, "find your agents and connect them"),
        (format!("{} <app>", command("connect")), 13, "connect 1 app"),
        (command("doctor"), 6, "check the install"),
    ];
    let row =
        |names: &[&str]| names.iter().map(|name| command(name)).collect::<Vec<_>>().join("  ");
    let mut out = header(style, version);
    out += &format!("\n  {}\n", heading("Get started"));
    for (name, width, about) in started {
        out += &format!("    {name}{:pad$}{about}\n", "", pad = 17 - width);
    }
    out += &format!("\n  {}\n", heading("Memory"));
    let memory =
        ["search", "recent", "append", "fact", "tree", "file", "forget", "export", "studio"];
    out += &format!("    {}\n", row(&memory));
    out += &format!("\n  {}\n", heading("Server"));
    let server = ["serve", "init", "stats", "verify", "dump", "recover", "load", "upgrade"];
    out += &format!("    {}\n", row(&server));
    out += "\n  Every flag: atticdb help --all\n";
    out
}

/// The `USAGE` lines starting with `prefix`, plus their more indented continuation lines.
/// The prefix is `atticdb <command>`, or `<connect>:` for the shared flags block. A line ending
/// in `.` is prose, so it's skipped.
fn usage_lines(prefix: &str) -> Vec<&'static str> {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let mut lines = Vec::new();
    let mut inside = false;
    for line in USAGE.lines() {
        let text = line.trim_start();
        if inside && indent(line) > 2 && !text.starts_with("atticdb ") {
            lines.push(line);
            continue;
        }
        inside = !text.ends_with('.')
            && text
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '));
        if inside {
            lines.push(line);
        }
    }
    lines
}

/// The `USAGE` lines of `command`, as `--version`, `-V` or a command name. `lead` starts the
/// first line and indents the rest. `None` when `command` isn't one.
fn command_lines(command: &str, lead: &str) -> Option<String> {
    let command = if command == "-V" { "--version" } else { command };
    if command.contains(char::is_whitespace) {
        return None;
    }
    let lines = usage_lines(&format!("atticdb {command}"));
    let indent = " ".repeat(lead.len());
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        out += &format!("{}{}\n", if index == 0 { lead } else { &indent }, &line[2..]);
    }
    (!out.is_empty()).then_some(out)
}

/// The apps line for `connect` and `disconnect`, after `lead`.
fn apps_line(lead: &str) -> String {
    format!("{lead}apps:  {}\n", APPS.join("  "))
}

/// What `atticdb <command> --help` prints: the command's usage lines. `connect` adds the
/// apps line. `None` when `command` isn't one.
pub fn command_usage(command: &str) -> Option<String> {
    let out = command_lines(command, "usage: ")?;
    Some(if command == "connect" { out + &apps_line("") } else { out })
}

/// Every `--flag` named in `lines`.
fn flags_in(lines: &[&str]) -> Vec<String> {
    let mut flags = Vec::new();
    for line in lines {
        let mut rest = *line;
        while let Some(start) = rest.find("--") {
            let name = &rest[start + 2..];
            let end =
                name.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(name.len());
            flags.push(format!("--{}", &name[..end]));
            rest = &name[end..];
        }
    }
    flags
}

/// The stderr text for `args`, argv without the program name, that failed to parse. The first
/// line says what was wrong. Then come the command's usage lines and a pointer to `--help`.
pub fn error(style: Style, args: &[String]) -> String {
    let Some((typed, rest)) = args.split_first() else {
        return format!(
            "{} missing command\n{MORE}",
            paint(style, &format!("{BOLD}{RED}"), "error:")
        );
    };
    let command = if typed == "-V" { "--version" } else { typed.as_str() };
    let usage = command_lines(command, "  usage: ").unwrap_or_default();
    let lines: Vec<&str> = usage.lines().collect();
    let mut extra = String::new();
    // Debug quoting escapes control bytes, so argv can't send escapes to the terminal.
    let what = if lines.is_empty() {
        format!("unknown command {typed:?}")
    } else if let Some(app) = rest
        .first()
        .filter(|app| matches!(command, "connect" | "disconnect") && !APPS.contains(&app.as_str()))
    {
        extra = apps_line("  ");
        format!("unknown app {app:?}")
    } else {
        let mut known = flags_in(&lines);
        if usage.contains("<connect>") {
            known.extend(flags_in(&usage_lines("<connect>:")));
        }
        match rest.iter().find(|arg| arg.starts_with("--") && !known.contains(arg)) {
            Some(flag) => format!("unknown flag {flag:?}"),
            None => format!("missing or invalid arguments for {typed:?}"),
        }
    };
    let error = paint(style, &format!("{BOLD}{RED}"), "error:");
    format!("{error} {what}\n{extra}{usage}{MORE}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::Harness;

    const LOGO_ROWS: [&str; 8] = [
        "       ▄▄",
        "     ▄████▄",
        "   ▄███▀▀███▄",
        "▄████▀    ▀████▄",
        " ▀▀▀        ▀▀▀",
        "     ██████",
        "     ██████",
        "     ▀▀▀▀▀▀",
    ];

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    /// Drops every `ESC [ ... m` sequence.
    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn style_rules() {
        assert_eq!(style(true, None, None, false), Style::Ansi256);
        assert_eq!(style(true, None, Some("truecolor"), false), Style::TrueColor);
        assert_eq!(style(true, None, Some("24bit"), false), Style::TrueColor);
        assert_eq!(style(true, Some("1"), Some("truecolor"), false), Style::Plain);
        assert_eq!(style(true, Some(""), None, false), Style::Ansi256);
        assert_eq!(style(false, None, None, false), Style::Plain);
        assert_eq!(style(true, None, None, true), Style::Plain);
    }

    #[test]
    fn apps_match_harnesses() {
        let names: Vec<&str> = Harness::ALL.iter().map(|harness| harness.name()).collect();
        assert_eq!(names, APPS);
    }

    #[test]
    fn plain_header_is_1_line() {
        assert_eq!(
            header(Style::Plain, "0.2.0"),
            "atticdb 0.2.0, local memory for coding agents\n"
        );
    }

    #[test]
    fn truecolor_header_has_the_logo_and_text() {
        let text = header(Style::TrueColor, "0.2.0");
        assert!(text.contains("38;2;237;230;216"), "{text:?}");
        assert!(text.contains("38;2;200;240;60"), "{text:?}");
        let plain = strip(&text);
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(lines.len(), 8, "{plain}");
        let beside = ["atticdb 0.2.0", "local memory for coding agents", "https://atticdb.com"];
        for (index, (line, row)) in lines.iter().zip(LOGO_ROWS).enumerate() {
            assert!(line.starts_with(row), "row {index}: {line:?}");
            let rest = line[row.len()..].trim();
            match index {
                1..=3 => assert_eq!(rest, beside[index - 1], "row {index}"),
                _ => assert_eq!(rest, "", "row {index}"),
            }
        }
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn ansi256_header_uses_254_and_191() {
        let text = header(Style::Ansi256, "0.2.0");
        assert!(text.contains("38;5;254"), "{text:?}");
        assert!(text.contains("38;5;191"), "{text:?}");
        assert!(!text.contains("38;2;"), "{text:?}");
    }

    #[test]
    fn plain_help_has_groups_and_no_escapes() {
        let text = help(Style::Plain, "0.2.0");
        for part in
            ["Get started", "connect <app>", "Memory", "Server", "Every flag: atticdb help --all"]
        {
            assert!(text.contains(part), "{part}: {text}");
        }
        assert!(text.starts_with("atticdb 0.2.0, local memory for coding agents\n"));
        assert!(!text.contains('\x1b'), "{text:?}");
    }

    #[test]
    fn colored_help_strips_to_plain_body() {
        let colored = strip(&help(Style::Ansi256, "0.2.0"));
        let plain = help(Style::Plain, "0.2.0");
        let body = plain.split_once('\n').map_or("", |(_, body)| body);
        assert!(colored.ends_with(body), "{colored}");
    }

    #[test]
    fn unknown_command() {
        let text = error(Style::Plain, &args(&["bogus"]));
        assert_eq!(text.lines().next(), Some(r#"error: unknown command "bogus""#));
        assert!(!text.contains("usage:"), "{text}");
        assert!(text.ends_with("  more:  atticdb --help\n"), "{text}");
    }

    #[test]
    fn a_command_with_spaces_is_unknown() {
        let text = error(Style::Plain, &args(&["connect <vscode|copilot>"]));
        assert!(text.starts_with("error: unknown command"), "{text}");
        let text = error(Style::Plain, &args(&[""]));
        assert!(text.starts_with(r#"error: unknown command """#), "{text}");
    }

    #[test]
    fn unknown_app() {
        for command in ["connect", "disconnect"] {
            let text = error(Style::Plain, &args(&[command, "claude-desktp"]));
            assert_eq!(text.lines().next(), Some(r#"error: unknown app "claude-desktp""#));
            let apps =
                "  apps:  vscode  copilot  claude-code  codex  gemini  antigravity  claude-desktop";
            assert!(text.lines().any(|line| line == apps), "{text}");
            assert!(
                text.lines().any(|line| line.starts_with("  usage:")
                    && line.contains(&format!("atticdb {command}"))),
                "{text}"
            );
            assert!(text.ends_with("  more:  atticdb --help\n"), "{text}");
        }
    }

    #[test]
    fn unknown_flag_counts_connect_flags() {
        let text = error(Style::Plain, &args(&["search", "--url-x", "u", "--query", "q"]));
        assert_eq!(text.lines().next(), Some(r#"error: unknown flag "--url-x""#));
        assert!(text.contains("  usage: atticdb search <connect> --query <words>"), "{text}");
        assert!(text.ends_with("  more:  atticdb --help\n"), "{text}");
    }

    #[test]
    fn known_flags_are_invalid_arguments() {
        let text = error(Style::Plain, &args(&["search", "--url", "u"]));
        assert_eq!(
            text.lines().next(),
            Some(r#"error: missing or invalid arguments for "search""#)
        );
        assert!(text.ends_with("  more:  atticdb --help\n"), "{text}");
    }

    #[test]
    fn continuation_lines_keep_their_alignment() {
        let text = error(Style::Plain, &args(&["serve", "--bogus"]));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], r#"error: unknown flag "--bogus""#);
        assert!(lines[1].starts_with("  usage: atticdb serve --config <path>"), "{text}");
        assert_eq!(
            lines[2].trim_start(),
            "[--unix-socket-group <gid or name>] [--exit-on-poison] [--log-requests]"
        );
        assert_eq!(lines[2].len() - lines[2].trim_start().len(), 15 + 7, "{text}");
        assert_eq!(lines[3], "  more:  atticdb --help");
    }

    #[test]
    fn version_with_extra_args() {
        for flag in ["--version", "-V"] {
            let text = error(Style::Plain, &args(&[flag, "extra"]));
            assert!(text.contains("atticdb --version | -V"), "{text}");
            assert!(text.ends_with("  more:  atticdb --help\n"), "{text}");
        }
    }

    #[test]
    fn colored_error_is_bold_red() {
        let text = error(Style::Ansi256, &args(&["bogus"]));
        assert!(text.starts_with("\x1b[1m\x1b[31merror:\x1b[0m"), "{text:?}");
        assert_eq!(strip(&text), error(Style::Plain, &args(&["bogus"])));
    }

    #[test]
    fn control_bytes_in_args_are_escaped() {
        let typed = "x\u{1b}]0;pwned\u{7}";
        for list in [
            vec![typed],
            vec!["connect", typed],
            vec!["search", typed],
            vec!["search", "--query", typed],
        ] {
            let text = error(Style::Plain, &args(&list));
            assert!(!text.contains(['\u{1b}', '\u{7}']), "{list:?}: {text:?}");
        }
        let text = error(Style::Plain, &args(&[typed]));
        assert_eq!(text.lines().next(), Some(r#"error: unknown command "x\u{1b}]0;pwned\u{7}""#));
    }

    #[test]
    fn prose_lines_are_not_usage_lines() {
        let text = error(Style::Plain, &args(&["doctor", "--harnes", "x"]));
        let usage: Vec<&str> = text.lines().filter(|line| line.starts_with("  usage:")).collect();
        assert_eq!(usage, ["  usage: atticdb doctor [--harness <list>]"], "{text}");
        assert!(!text.lines().any(|line| line.ends_with('.')), "{text}");
    }

    #[test]
    fn command_usage_lists_the_command_lines() {
        assert_eq!(
            command_usage("search").as_deref(),
            Some(
                "usage: atticdb search <connect> --query <words> [--order relevance|newest] \
                 [--limit <n>]\n                     [--before <lsn>] [--max-bytes <n>]\n"
            )
        );
        let serve = command_usage("serve").unwrap_or_default();
        let lines: Vec<&str> = serve.lines().collect();
        assert!(lines[0].starts_with("usage: atticdb serve --config <path>"), "{serve}");
        assert_eq!(lines[1].len() - lines[1].trim_start().len(), 15 + 5, "{serve}");
        let connect = command_usage("connect").unwrap_or_default();
        assert!(connect.starts_with("usage: atticdb connect <vscode|copilot>"), "{connect}");
        assert_eq!(connect.lines().filter(|line| line.contains("atticdb connect")).count(), 3);
        let apps =
            "apps:  vscode  copilot  claude-code  codex  gemini  antigravity  claude-desktop";
        assert!(connect.ends_with(&format!("{apps}\n")), "{connect}");
        assert_eq!(command_usage("-V").as_deref(), Some("usage: atticdb --version | -V\n"));
        assert_eq!(command_usage("bogus"), None);
        assert_eq!(command_usage("connect <vscode|copilot>"), None);
    }

    #[test]
    fn no_color_in_a_terminal_has_no_escapes() {
        let style = style(true, Some("1"), Some("truecolor"), false);
        assert!(!header(style, "0.2.0").contains('\x1b'));
        assert!(!help(style, "0.2.0").contains('\x1b'));
        assert!(!error(style, &args(&["connect", "nope"])).contains('\x1b'));
    }
}
