//! The `zerodb` command. It parses arguments and calls the library.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use zerodb::config::{Config, Listen};

const USAGE: &str = "usage:
  zerodb init --config <path> --tenant <name>
  zerodb serve --config <path> [--data-dir <dir>] [--listen <addr>|none] [--unix-socket <path>]
               [--unix-socket-group <gid or name>] [--exit-on-poison]
  zerodb recover --truncate-at <lsn> --data-dir <dir>
  zerodb mcp [--url unix:///path|http://127.0.0.1:<port>] [--data-dir <dir>] [--config <path>]
             [--tenant <name>] [--token-file <path>] [--agent <name>] [--session <name>]
             [--start-timeout <n>s|<n>ms] [--retry-deadline <n>s|<n>ms]
  The mcp token comes from --token-file, ZERODB_TOKEN, or the config entry for --tenant.";

const MCP_FLAGS: &[&str] = &[
    "--url",
    "--data-dir",
    "--config",
    "--tenant",
    "--token-file",
    "--agent",
    "--session",
    "--start-timeout",
    "--retry-deadline",
];

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
            "mcp" => flags(rest, MCP_FLAGS, &[]).and_then(mcp),
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

fn mcp(mut flags: HashMap<String, String>) -> Option<u8> {
    let mut duration = |name: &str, default: Duration| match flags.remove(name) {
        None => Some(default),
        Some(text) => match text.strip_suffix("ms") {
            Some(ms) => ms.parse().ok().map(Duration::from_millis),
            None => text.strip_suffix('s')?.parse().ok().map(Duration::from_secs),
        },
    };
    let start_timeout = duration("--start-timeout", Duration::from_secs(60))?;
    let retry_deadline = duration("--retry-deadline", Duration::from_secs(30))?;
    Some(zerodb::mcp::start(zerodb::mcp::Options {
        url: flags.remove("--url"),
        data_dir: flags.remove("--data-dir").map(PathBuf::from),
        config: flags.remove("--config").map(PathBuf::from),
        tenant: flags.remove("--tenant"),
        token_file: flags.remove("--token-file").map(PathBuf::from),
        agent: flags.remove("--agent").unwrap_or_else(|| "mcp".into()),
        session: flags.remove("--session").unwrap_or_else(|| "default".into()),
        start_timeout,
        retry_deadline,
    }))
}
