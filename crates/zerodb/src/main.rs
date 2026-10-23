//! The `zerodb` command. It parses arguments and calls the library.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use zerodb::config::{Config, Listen};
use zerodb::launch::Url;

const USAGE: &str = "usage:
  zerodb init --config <path> --tenant <name>
  zerodb serve --config <path> [--data-dir <dir>] [--listen <addr>|none] [--unix-socket <path>]
               [--unix-socket-group <gid or name>] [--exit-on-poison]
  zerodb recover --truncate-at <lsn> --data-dir <dir>
  zerodb stats --url unix:///path | --data-dir <dir>
  zerodb load --url unix:///path|http://127.0.0.1:<port> [--token-file <path>] [--writers <n>]
              [--turns <n>] [--batch <n>] [--size <bytes>] [--think <ms>]
  zerodb verify --data-dir <dir> [--scratch <dir>]
  zerodb dump --data-dir <dir> [--scratch <dir>] [--from <lsn>] [--limit <n>]
  zerodb mcp [--url unix:///path|http://127.0.0.1:<port>] [--data-dir <dir>] [--config <path>]
             [--tenant <name>] [--token-file <path>] [--agent <name>] [--session <name>]
             [--retry-deadline <n>s|<n>ms]
  The mcp token comes from --token-file, ZERODB_TOKEN, or the config entry for --tenant.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.split_first() {
        Some((command, rest)) => match command.as_str() {
            "init" => flags(rest, &["--config", "--tenant"], &[]).and_then(init),
            "serve" => flags(
                rest,
                &["--config", "--data-dir", "--listen", "--unix-socket", "--unix-socket-group"],
                &["--exit-on-poison"],
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
            "mcp" => {
                let valued = ["--url", "--data-dir", "--config", "--tenant", "--token-file"];
                let more = ["--agent", "--session", "--retry-deadline"];
                flags(rest, &[&valued[..], &more].concat(), &[]).map(zerodb::mcp::start)
            }
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
    Some(zerodb::serve::start(config, flags.contains_key("--exit-on-poison")))
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
