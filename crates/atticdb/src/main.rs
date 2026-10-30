//! The `atticdb` command. It parses arguments and calls the library.

#![forbid(unsafe_code)]

use atticdb::config::{Config, Listen};
use atticdb::connect::{Harness, Mode, Notes, Options};
use atticdb::launch::Url;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage:
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
  atticdb migrate [--dry-run]
  atticdb connect <vscode|copilot> [--force] [--dry-run]
  atticdb connect <claude-code|codex> [--force] [--no-hook] [--notes ask|auto] [--dry-run]
  atticdb disconnect <vscode|copilot|claude-code|codex>
  atticdb hook session-start [--harness <name>]
  atticdb studio [--url unix:///path|http://127.0.0.1:<port>] [--data-dir <dir>] [--config <path>]
                [--tenant <name>] [--token-file <path>] [--no-open]
  atticdb append <connect> --content <text> [--role user|assistant|tool]
  atticdb recent <connect> [--n <n>] [--max-bytes <n>]
  atticdb search <connect> --query <words> [--limit <n>] [--before <lsn>] [--max-bytes <n>]
  atticdb fact set <connect> --key <key> (--content <text> | --delete)
  atticdb fact get <connect> --key <key>
  atticdb fact list <connect> --prefix <prefix> [--after <key>] [--limit <n>] [--max-bytes <n>]
  <connect>: [--url unix:///path|http://127.0.0.1:<port>] [--data-dir <dir>] [--config <path>]
             [--tenant <name>] [--token-file <path>] [--agent <name>] [--session <name>]
             [--retry-deadline <n>s|<n>ms]
  The token comes from --token-file, ATTICDB_TOKEN, or the config entry for --tenant.
  With no --url, --data-dir, --config, --tenant or --token-file, the home is used and
  ATTICDB_TOKEN is ignored. ATTICDB_HOME, when set, is the home. It must be an absolute path.
  --agent defaults to mcp. --session defaults to the repo of the current directory, named
  after the folder holding .git. A worktree takes its main repo's name.
  append, recent, search and fact print one JSON line. They exit 1 when nothing was written,
  3 when a write may or may not be saved.
  studio serves a read-only page on 127.0.0.1 and opens it. It runs until Ctrl-C.
  setup finds VS Code, Copilot CLI, Claude Code and Codex, asks which to connect, shows the
  plan, then connects them and checks each. See atticdb setup --help.
  doctor checks the install and writes nothing. See atticdb doctor --help.
  migrate moves an install from before the rename: the home and each app's parts. See the
  README.";

/// Commands that use the home. A relative `ATTICDB_HOME` fails them with exit 2, so none falls
/// back to the default home. The hook prints `{}` for it, as for any error.
const HOME_COMMANDS: [&str; 9] =
    ["mcp", "connect", "setup", "doctor", "studio", "append", "recent", "search", "fact"];

/// Flags of every command that talks to a server as a tenant. The first 5 pick the store.
const CONNECT: [&str; 8] = [
    "--url",
    "--data-dir",
    "--config",
    "--tenant",
    "--token-file",
    "--agent",
    "--session",
    "--retry-deadline",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|command| HOME_COMMANDS.contains(&command.as_str())) {
        if let Err(error) = atticdb::home::env_dir() {
            return ExitCode::from(usage(&error));
        }
        // Doctor reports these itself, and setup's plan shows the home's as a stop line.
        if let Err(error) = atticdb::legacy::guard(&args[0]) {
            return ExitCode::from(usage(&error));
        }
    }
    let code = match args.split_first() {
        Some((command, rest)) => match command.as_str() {
            "--version" | "-V" if rest.is_empty() => {
                println!("atticdb {}", atticdb::VERSION);
                Some(0)
            }
            "init" => flags(rest, &["--config", "--tenant"], &[]).and_then(init),
            "serve" => flags(
                rest,
                &["--config", "--data-dir", "--listen", "--unix-socket", "--unix-socket-group"],
                &["--exit-on-poison", "--log-requests"],
            )
            .and_then(serve),
            "recover" => flags(rest, &["--truncate-at", "--data-dir"], &[]).and_then(recover),
            "stats" => flags(rest, &["--url", "--data-dir"], &[]).and_then(stats),
            "load" => {
                let valued = [
                    "--url",
                    "--token-file",
                    "--writers",
                    "--turns",
                    "--batch",
                    "--size",
                    "--think",
                ];
                flags(rest, &valued, &[]).and_then(load)
            }
            "verify" => flags(rest, &["--data-dir", "--scratch"], &[]).and_then(verify),
            "dump" => {
                flags(rest, &["--data-dir", "--scratch", "--from", "--limit"], &[]).and_then(dump)
            }
            "mcp" => flags(rest, &CONNECT, &[]).map(atticdb::mcp::start),
            "connect" => rest.split_first().and_then(|(name, rest)| {
                connect(
                    Harness::parse(name)?,
                    flags(rest, &["--notes"], &["--force", "--no-hook", "--dry-run"])?,
                )
            }),
            "setup" if rest.iter().any(|arg| arg == "--help") => {
                println!("{}", atticdb::setup::USAGE);
                Some(0)
            }
            "setup" => flags(rest, &["--harness", "--notes"], &["--yes", "--no-hook", "--dry-run"])
                .and_then(setup),
            "doctor" if rest.iter().any(|arg| arg == "--help") => {
                println!("{}", atticdb::doctor::USAGE);
                Some(0)
            }
            "doctor" => flags(rest, &["--harness"], &[]).and_then(doctor),
            "migrate" => flags(rest, &[], &["--dry-run"])
                .map(|flags| atticdb::legacy::migrate(flags.contains_key("--dry-run"))),
            "disconnect" => match rest {
                [name] => Harness::parse(name).map(atticdb::connect::disconnect),
                _ => None,
            },
            "hook" => match rest {
                [event] if event == "session-start" => Some(atticdb::hook::session_start(None)),
                [event, flag, name] if event == "session-start" && flag == "--harness" => {
                    Some(atticdb::hook::session_start(Some(name)))
                }
                // A hook must not fail, so a missing name prints `{}` like an unknown one.
                [event, flag] if event == "session-start" && flag == "--harness" => {
                    Some(atticdb::hook::session_start(Some("")))
                }
                _ => None,
            },
            "studio" => flags(rest, &CONNECT[..5], &["--no-open"]).map(atticdb::studio::start),
            "append" => tool(rest, "memory_append", &["--content", "--role"], &[]),
            "recent" => tool(rest, "memory_recent", &["--n", "--max-bytes"], &[]),
            "search" => {
                tool(rest, "memory_search", &["--query", "--limit", "--before", "--max-bytes"], &[])
            }
            "fact" => match rest.split_first().map(|(verb, rest)| (verb.as_str(), rest)) {
                Some(("set", rest)) => {
                    tool(rest, "fact_set", &["--key", "--content"], &["--delete"])
                }
                Some(("get", rest)) => tool(rest, "fact_get", &["--key"], &[]),
                Some(("list", rest)) => {
                    let valued = ["--prefix", "--after", "--limit", "--max-bytes"];
                    tool(rest, "fact_list", &valued, &[])
                }
                _ => None,
            },
            _ => None,
        },
        None => None,
    };
    ExitCode::from(code.unwrap_or_else(|| {
        eprintln!("{USAGE}");
        2
    }))
}

/// Parses `--name value` pairs and bare switches, each at most once. `None` is a usage error.
fn flags(args: &[String], valued: &[&str], switches: &[&str]) -> Option<HashMap<String, String>> {
    let mut parsed = HashMap::new();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = match flag.as_str() {
            name if switches.contains(&name) => String::new(),
            name if valued.contains(&name) => args.next()?.clone(),
            _ => return None,
        };
        if parsed.insert(flag.clone(), value).is_some() {
            return None;
        }
    }
    Some(parsed)
}

fn tool(args: &[String], name: &str, valued: &[&str], switches: &[&str]) -> Option<u8> {
    flags(args, &[&CONNECT[..], valued].concat(), switches)
        .map(|flags| atticdb::mcp::command(name, flags))
}

/// `atticdb connect`. `--no-hook` and `--notes` are for Claude Code and Codex only. Codex
/// takes `--force`, but never edits an `atticdb` server it didn't write. `--dry-run` prints
/// what would change and writes nothing.
fn connect(harness: Harness, mut flags: HashMap<String, String>) -> Option<u8> {
    let force = flags.remove("--force").is_some();
    let mode = if flags.remove("--dry-run").is_some() { Mode::Plan } else { Mode::Apply };
    if ![Harness::ClaudeCode, Harness::Codex].contains(&harness) && !flags.is_empty() {
        return None;
    }
    let notes = notes(flags.remove("--notes"))?.unwrap_or(Notes::Auto);
    let hook = flags.remove("--no-hook").is_none();
    Some(atticdb::connect::connect(harness, Options { force, hook, notes }, mode).print())
}

/// The value of `--notes`: `Some(None)` when it is not given, `None` when it is not valid.
fn notes(value: Option<String>) -> Option<Option<Notes>> {
    match value.as_deref() {
        None => Some(None),
        Some("auto") => Some(Some(Notes::Auto)),
        Some("ask") => Some(Some(Notes::Ask)),
        Some(_) => None,
    }
}

/// `atticdb setup`.
fn setup(mut flags: HashMap<String, String>) -> Option<u8> {
    let harnesses = match flags.remove("--harness") {
        Some(list) => Some(atticdb::setup::apps(&list)?),
        None => None,
    };
    Some(atticdb::setup::run(atticdb::setup::Flags {
        yes: flags.contains_key("--yes"),
        harnesses,
        no_hook: flags.contains_key("--no-hook"),
        notes: notes(flags.remove("--notes"))?,
        dry_run: flags.contains_key("--dry-run"),
    }))
}

/// `atticdb doctor`.
fn doctor(mut flags: HashMap<String, String>) -> Option<u8> {
    let harnesses = match flags.remove("--harness") {
        Some(list) => Some(atticdb::setup::apps(&list)?),
        None => None,
    };
    Some(atticdb::doctor::run(harnesses))
}

fn report<T>(result: Result<T, impl std::fmt::Display>) -> Result<T, u8> {
    result.map_err(|error| {
        eprintln!("atticdb: {error}");
        1
    })
}

fn init(mut flags: HashMap<String, String>) -> Option<u8> {
    let (config, tenant) = (flags.remove("--config")?, flags.remove("--tenant")?);
    Some(match report(atticdb::config::init(Path::new(&config), &tenant)) {
        Ok(token) => {
            println!("{}", token.expose());
            0
        }
        Err(code) => code,
    })
}

fn serve(mut flags: HashMap<String, String>) -> Option<u8> {
    let path = flags.remove("--config")?;
    let listen = flags.remove("--listen").map(|text| Listen::parse(&text)).transpose();
    let Ok(listen) = report(listen) else { return Some(2) };
    let mut config = match report(Config::load(Path::new(&path))) {
        Ok(config) => config,
        Err(code) => return Some(code),
    };
    config.listen = listen.or(config.listen);
    config.data_dir = flags.remove("--data-dir").map(PathBuf::from).or(config.data_dir);
    config.unix_socket = flags.remove("--unix-socket").map(PathBuf::from).or(config.unix_socket);
    config.unix_socket_group = flags.remove("--unix-socket-group").or(config.unix_socket_group);
    let (poison, requests) =
        (flags.contains_key("--exit-on-poison"), flags.contains_key("--log-requests"));
    Some(atticdb::serve::start(config, poison, requests))
}

fn recover(mut flags: HashMap<String, String>) -> Option<u8> {
    let lsn = flags.remove("--truncate-at")?.parse().ok()?;
    Some(atticdb::serve::recover(Path::new(&flags.remove("--data-dir")?), lsn))
}

fn stats(mut flags: HashMap<String, String>) -> Option<u8> {
    let (url, dir) = (flags.remove("--url"), flags.remove("--data-dir"));
    if url.is_some() == dir.is_some() {
        return None;
    }
    let socket = match url.as_deref().map(Url::parse).transpose() {
        Ok(Some(Url::Unix(path))) => Some(path),
        Ok(None) => None,
        Ok(Some(Url::Tcp(_))) => return Some(usage("stats is served on the Unix socket only")),
        Err(error) => return Some(usage(&error)),
    };
    Some(atticdb::ops::stats(socket.as_deref(), dir.as_deref().map(Path::new)))
}

fn verify(mut flags: HashMap<String, String>) -> Option<u8> {
    let scratch = flags.remove("--scratch").map(PathBuf::from);
    Some(atticdb::ops::verify(Path::new(&flags.remove("--data-dir")?), scratch.as_deref()))
}

fn dump(mut flags: HashMap<String, String>) -> Option<u8> {
    let from = flags.remove("--from").map_or(Ok(1), |text| text.parse()).ok()?;
    let limit = flags.remove("--limit").map_or(Ok(usize::MAX), |text| text.parse()).ok()?;
    let scratch = flags.remove("--scratch").map(PathBuf::from);
    let dir = flags.remove("--data-dir")?;
    Some(atticdb::ops::dump(Path::new(&dir), scratch.as_deref(), from, limit))
}

fn usage(message: &str) -> u8 {
    eprintln!("atticdb: {message}");
    2
}

fn load(mut flags: HashMap<String, String>) -> Option<u8> {
    let mut number = |name: &str, default: usize| match flags.remove(name) {
        Some(text) => text.parse().ok().filter(|n| *n > 0),
        None => Some(default),
    };
    let (writers, turns) = (number("--writers", 1)?, number("--turns", 10_000)?);
    let (batch, size) = (number("--batch", 1)?, number("--size", 256)?);
    // Pauses up to 2x think must stay under the server's 10 s head timeout.
    let think = flags.remove("--think").map_or(Some(0), |text| text.parse().ok())?;
    // At most the server's connection cap and the tape's batch cap.
    let split = writers.checked_mul(batch).filter(|n| turns % n == 0);
    if writers > atticdb::serve::MAX_CONNECTIONS || batch > 1024 || split.is_none() || think > 4_000
    {
        let rule = "--turns must divide by --writers * --batch; writers <= 256, batch <= 1024, \
            think <= 4000 ms";
        return Some(usage(rule));
    }
    let url = match Url::parse(&flags.remove("--url")?) {
        Ok(url) => url,
        Err(error) => return Some(usage(&error)),
    };
    let token = match flags.remove("--token-file") {
        Some(path) => atticdb::config::read_token_file(Path::new(&path)),
        None => std::env::var(atticdb::mcp::TOKEN_ENV)
            .map_err(|_| format!("set {} or --token-file", atticdb::mcp::TOKEN_ENV))
            .and_then(|text| atticdb::config::Token::new(text).map_err(str::to_owned)),
    };
    match token {
        Ok(token) => Some(atticdb::ops::load(
            url,
            &token,
            writers,
            turns,
            batch,
            size,
            std::time::Duration::from_millis(think),
        )),
        Err(error) => Some(usage(&error)),
    }
}
