//! The `zerodb` command. It parses arguments and calls the library.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use zerodb::config::{Config, Listen};
use zerodb::connect::{Harness, Notes};
use zerodb::launch::Url;

const USAGE: &str = "usage:
  zerodb init --config <path> --tenant <name>
  zerodb serve --config <path> [--data-dir <dir>] [--listen <addr>|none] [--unix-socket <path>]
               [--unix-socket-group <gid or name>] [--exit-on-poison] [--log-requests]
  zerodb recover --truncate-at <lsn> --data-dir <dir>
  zerodb stats --url unix:///path | --data-dir <dir>
  zerodb load --url unix:///path|http://127.0.0.1:<port> [--token-file <path>] [--writers <n>]
              [--turns <n>] [--batch <n>] [--size <bytes>] [--think <ms>]
  zerodb verify --data-dir <dir> [--scratch <dir>]
  zerodb dump --data-dir <dir> [--scratch <dir>] [--from <lsn>] [--limit <n>]
  zerodb mcp <connect>
  zerodb connect <vscode|copilot> [--force]
  zerodb connect <claude-code|codex> [--force] [--no-hook] [--notes ask|auto]
  zerodb disconnect <vscode|copilot|claude-code|codex>
  zerodb hook session-start [--harness <name>]
  zerodb studio [--url unix:///path|http://127.0.0.1:<port>] [--data-dir <dir>] [--config <path>]
                [--tenant <name>] [--token-file <path>] [--no-open]
  zerodb append <connect> --content <text> [--role user|assistant|tool]
  zerodb recent <connect> [--n <n>] [--max-bytes <n>]
  zerodb search <connect> --query <words> [--limit <n>] [--before <lsn>] [--max-bytes <n>]
  zerodb fact set <connect> --key <key> (--content <text> | --delete)
  zerodb fact get <connect> --key <key>
  zerodb fact list <connect> --prefix <prefix> [--after <key>] [--limit <n>] [--max-bytes <n>]
  <connect>: [--url unix:///path|http://127.0.0.1:<port>] [--data-dir <dir>] [--config <path>]
             [--tenant <name>] [--token-file <path>] [--agent <name>] [--session <name>]
             [--retry-deadline <n>s|<n>ms]
  The token comes from --token-file, ZERODB_TOKEN, or the config entry for --tenant.
  With no --url, --data-dir, --config, --tenant or --token-file, the home is used and
  ZERODB_TOKEN is ignored. ZERODB_HOME, when set, is the home. It must be an absolute path.
  --agent defaults to mcp. --session defaults to the repo of the current directory, named
  after the folder holding .git. A worktree takes its main repo's name.
  append, recent, search and fact print one JSON line. They exit 1 when nothing was written,
  3 when a write may or may not be saved.
  studio serves a read-only page on 127.0.0.1 and opens it. It runs until Ctrl-C.";

/// Commands that use the home. A relative `ZERODB_HOME` fails them with exit 2, so none falls
/// back to the default home. The hook prints `{}` for it, as for any error.
const HOME_COMMANDS: [&str; 7] = ["mcp", "connect", "studio", "append", "recent", "search", "fact"];

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
        if let Err(error) = zerodb::home::env_dir() {
            return ExitCode::from(usage(&error));
        }
    }
    let code = match args.split_first() {
        Some((command, rest)) => match command.as_str() {
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
            "mcp" => flags(rest, &CONNECT, &[]).map(zerodb::mcp::start),
            "connect" => rest.split_first().and_then(|(name, rest)| {
                connect(
                    Harness::parse(name)?,
                    flags(rest, &["--notes"], &["--force", "--no-hook"])?,
                )
            }),
            "disconnect" => match rest {
                [name] => Harness::parse(name).map(zerodb::connect::disconnect),
                _ => None,
            },
            "hook" => match rest {
                [event] if event == "session-start" => Some(zerodb::hook::session_start(None)),
                [event, flag, name] if event == "session-start" && flag == "--harness" => {
                    Some(zerodb::hook::session_start(Some(name)))
                }
                // A hook must not fail, so a missing name prints `{}` like an unknown one.
                [event, flag] if event == "session-start" && flag == "--harness" => {
                    Some(zerodb::hook::session_start(Some("")))
                }
                _ => None,
            },
            "studio" => flags(rest, &CONNECT[..5], &["--no-open"]).map(zerodb::studio::start),
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
        .map(|flags| zerodb::mcp::command(name, flags))
}

/// `zerodb connect`. `--no-hook` and `--notes` are for Claude Code and Codex only. Codex
/// takes `--force`, but never edits a `zerodb` server it didn't write.
fn connect(harness: Harness, mut flags: HashMap<String, String>) -> Option<u8> {
    let force = flags.remove("--force").is_some();
    if ![Harness::ClaudeCode, Harness::Codex].contains(&harness) {
        return flags.is_empty().then(|| zerodb::connect::connect(harness, force));
    }
    let notes = match flags.remove("--notes").as_deref() {
        None | Some("auto") => Notes::Auto,
        Some("ask") => Notes::Ask,
        Some(_) => return None,
    };
    let hook = flags.remove("--no-hook").is_none();
    Some(match harness {
        Harness::Codex => zerodb::connect::connect_codex(hook, notes),
        _ => zerodb::connect::connect_claude_code(force, hook, notes),
    })
}

fn report<T>(result: Result<T, impl std::fmt::Display>) -> Result<T, u8> {
    result.map_err(|error| {
        eprintln!("zerodb: {error}");
        1
    })
}

fn init(mut flags: HashMap<String, String>) -> Option<u8> {
    let (config, tenant) = (flags.remove("--config")?, flags.remove("--tenant")?);
    Some(match report(zerodb::config::init(Path::new(&config), &tenant)) {
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
    Some(zerodb::serve::start(config, poison, requests))
}

fn recover(mut flags: HashMap<String, String>) -> Option<u8> {
    let lsn = flags.remove("--truncate-at")?.parse().ok()?;
    Some(zerodb::serve::recover(Path::new(&flags.remove("--data-dir")?), lsn))
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
    Some(zerodb::ops::stats(socket.as_deref(), dir.as_deref().map(Path::new)))
}

fn verify(mut flags: HashMap<String, String>) -> Option<u8> {
    let scratch = flags.remove("--scratch").map(PathBuf::from);
    Some(zerodb::ops::verify(Path::new(&flags.remove("--data-dir")?), scratch.as_deref()))
}

fn dump(mut flags: HashMap<String, String>) -> Option<u8> {
    let from = flags.remove("--from").map_or(Ok(1), |text| text.parse()).ok()?;
    let limit = flags.remove("--limit").map_or(Ok(usize::MAX), |text| text.parse()).ok()?;
    let scratch = flags.remove("--scratch").map(PathBuf::from);
    let dir = flags.remove("--data-dir")?;
    Some(zerodb::ops::dump(Path::new(&dir), scratch.as_deref(), from, limit))
}

fn usage(message: &str) -> u8 {
    eprintln!("zerodb: {message}");
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
    if writers > zerodb::serve::MAX_CONNECTIONS || batch > 1024 || split.is_none() || think > 4_000
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
        Some(path) => zerodb::config::read_token_file(Path::new(&path)),
        None => std::env::var(zerodb::mcp::TOKEN_ENV)
            .map_err(|_| format!("set {} or --token-file", zerodb::mcp::TOKEN_ENV))
            .and_then(|text| zerodb::config::Token::new(text).map_err(str::to_owned)),
    };
    match token {
        Ok(token) => Some(zerodb::ops::load(
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
