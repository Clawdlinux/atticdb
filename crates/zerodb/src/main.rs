//! The `zerodb` command. It parses arguments and calls the library.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: zerodb init --config <path> --tenant <name>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        Some((command, rest)) if command == "init" => init(rest),
        _ => usage(),
    }
}

fn init(args: &[String]) -> ExitCode {
    let mut config = None;
    let mut tenant = None;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--config" => &mut config,
            "--tenant" => &mut tenant,
            _ => return usage(),
        };
        match args.next() {
            Some(value) if slot.is_none() => *slot = Some(value.clone()),
            _ => return usage(),
        }
    }
    let (Some(config), Some(tenant)) = (config, tenant) else {
        return usage();
    };
    match zerodb::config::init(&PathBuf::from(config), &tenant) {
        Ok(token) => {
            println!("{}", token.expose());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("zerodb: {error}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
