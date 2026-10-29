//! `zerodb setup`: finds the harnesses on this machine, asks which to connect, shows the plan,
//! connects them, then checks each one. The plan is [`connect::connect`] in [`Mode::Plan`], the same
//! code that writes, so the preview and `--dry-run` show what Apply does.

use crate::connect::{self, Harness, Line, Mode, Notes, Options, Report};
use serde_json::{json, Value};
use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The flags of `zerodb setup`. Each one replaces a question.
#[derive(Clone, Debug)]
pub struct Flags {
    /// Take every default, and don't ask `Apply?`.
    pub yes: bool,
    /// The apps to connect, from `--harness`.
    pub harnesses: Option<Vec<Harness>>,
    /// `--no-hook`: no SessionStart hook for Claude Code and Codex.
    pub no_hook: bool,
    /// Whether Claude Code and Codex save notes without asking, from `--notes`.
    pub notes: Option<Notes>,
    /// Print the plan and write nothing.
    pub dry_run: bool,
}

/// What setup says when it can't ask.
const NEEDS_TERMINAL: &str = "zerodb setup needs a terminal, or --yes. See zerodb setup --help.";

/// A testing aid: when it is `1`, stdin counts as a terminal, so tests can pipe the answers.
const INTERACTIVE_ENV: &str = "ZERODB_SETUP_INTERACTIVE";

/// How many answers a question takes before setup gives up.
const TRIES: usize = 3;

/// How long the hook may take to print its line.
const HOOK_TIMEOUT: Duration = Duration::from_secs(4);

/// How long the store may take to answer `fact_list`, a server start included.
const STORE_TIMEOUT: Duration = Duration::from_secs(5);

/// The order apps are listed and connected in: the Copilot ones, then Claude Code and Codex.
const ORDER: [Harness; 4] =
    [Harness::Copilot, Harness::Vscode, Harness::ClaudeCode, Harness::Codex];

/// The apps in a comma list of `vscode`, `copilot`, `claude-code` and `codex`, in [`ORDER`].
/// `None` when a name isn't one of those.
pub fn apps(list: &str) -> Option<Vec<Harness>> {
    let mut chosen = Vec::new();
    for name in list.split(',').map(str::trim) {
        chosen.push(Harness::parse(name)?);
    }
    Some(ORDER.into_iter().filter(|harness| chosen.contains(harness)).collect())
}

/// The app's name in sentences.
fn title(harness: Harness) -> &'static str {
    match harness {
        Harness::Vscode => "VS Code",
        Harness::Copilot => "Copilot CLI",
        Harness::ClaudeCode => "Claude Code",
        Harness::Codex => "Codex",
    }
}

/// Runs `zerodb setup`. Returns the exit code: 0 when every chosen app connected with no
/// skipped part and passed its checks, 1 when not, 2 for a usage error or no answer.
pub fn run(flags: Flags) -> u8 {
    let terminal = std::io::stdin().is_terminal()
        || std::env::var_os(INTERACTIVE_ENV).is_some_and(|value| value == "1");
    if !terminal && !flags.yes && !flags.dry_run {
        eprintln!("{NEEDS_TERMINAL}");
        return 2;
    }
    // Without a terminal, `--dry-run` takes the defaults, so it never waits on stdin.
    let ask = terminal && !flags.yes;
    let stdin = std::io::stdin();
    let mut asker = Asker { input: stdin.lock() };
    let chosen = match choose(&flags, ask, &mut asker) {
        Ok(Some(chosen)) => chosen,
        Ok(None) => return 1,
        Err(error) => {
            eprintln!("zerodb: {error}");
            return 2;
        }
    };
    let (harnesses, options) = chosen;
    show_plan(&harnesses, options);
    if flags.dry_run {
        println!("Dry run: nothing was written.");
        return 0;
    }
    if ask {
        match asker.line("Apply? [y/N] ") {
            None => {
                eprintln!("zerodb: no answer on stdin. {NEEDS_TERMINAL}");
                return 2;
            }
            Some(answer) if yes_no(&answer) == Some(true) => {}
            Some(_) => {
                println!("Nothing changed.");
                return 0;
            }
        }
    }
    apply(&harnesses, options)
}

/// Asks on stdout and reads the answers from stdin.
struct Asker<R> {
    input: R,
}

impl<R: BufRead> Asker<R> {
    /// Prints `prompt` and reads 1 answer, trimmed. `None` at the end of stdin.
    fn line(&mut self, prompt: &str) -> Option<String> {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        match self.input.read_line(&mut answer) {
            Ok(0) | Err(_) => {
                println!();
                None
            }
            Ok(_) => Some(answer.trim().to_owned()),
        }
    }

    /// Asks until `parse` takes the answer, at most [`TRIES`] times, printing `hint` after
    /// each answer it doesn't. An empty answer takes `default`.
    fn ask<T>(
        &mut self,
        prompt: &str,
        default: T,
        parse: impl Fn(&str) -> Option<T>,
        hint: &str,
    ) -> Result<T, String> {
        for _ in 0..TRIES {
            let answer = self.line(prompt);
            let answer = answer.ok_or_else(|| format!("no answer on stdin. {NEEDS_TERMINAL}"))?;
            if answer.is_empty() {
                return Ok(default);
            }
            if let Some(value) = parse(&answer) {
                return Ok(value);
            }
            println!("{hint}");
        }
        Err(format!("no valid answer after {TRIES} tries. {NEEDS_TERMINAL}"))
    }
}

/// `true` for y or yes, `false` for n or no, in any case.
fn yes_no(answer: &str) -> Option<bool> {
    match answer.to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

/// The apps and options, from the flags, else the answers, else the defaults. `None` when
/// no app was named and none was found.
fn choose(
    flags: &Flags,
    ask: bool,
    asker: &mut Asker<impl BufRead>,
) -> Result<Option<(Vec<Harness>, Options)>, String> {
    let harnesses = match &flags.harnesses {
        Some(harnesses) => harnesses.clone(),
        None => {
            let found: Vec<Harness> = ORDER.into_iter().filter(|h| connect::detect(*h)).collect();
            if found.is_empty() {
                println!(
                    "zerodb found no apps: none of VS Code, Copilot CLI, Claude Code or Codex has \
                     a config dir, and none of code, copilot, claude or codex is on PATH. \
                     Install one, or name it with --harness."
                );
                return Ok(None);
            }
            let names: Vec<&str> = found.iter().map(|harness| harness.name()).collect();
            let prompt = format!("Which apps? [{}] ", names.join(","));
            let hint = "Answer with a comma list of copilot, vscode, claude-code and codex.";
            if ask {
                asker.ask(&prompt, found, apps, hint)?
            } else {
                found
            }
        }
    };
    // Only Claude Code and Codex take these 2. VS Code and Copilot CLI always get the hook.
    let own = harnesses.iter().any(|h| matches!(h, Harness::ClaudeCode | Harness::Codex));
    let hint = "Answer y or n.";
    let hook = match flags.no_hook {
        true => false,
        false if ask && own => {
            asker.ask("Load memory at the start of each chat? [Y/n] ", true, yes_no, hint)?
        }
        false => true,
    };
    let notes = match flags.notes {
        Some(notes) => notes,
        None if ask && own => {
            let prompt = "Save notes without asking? (Claude Code and Codex. Saving a rule always \
                          asks.) [Y/n] ";
            if asker.ask(prompt, true, yes_no, hint)? {
                Notes::Auto
            } else {
                Notes::Ask
            }
        }
        None => Notes::Auto,
    };
    Ok(Some((harnesses, Options { force: false, hook, notes })))
}

/// The text of a line.
fn text(line: &Line) -> &str {
    let (Ok(text) | Err(text)) = line;
    text
}

/// Prints `report`'s lines under its app, then its error and warnings. Lines in `shown` are
/// left out, and the printed ones are added.
fn show(harness: Harness, report: &Report, shown: &mut Vec<String>, stopped: &str) {
    println!("{}:", title(harness));
    for line in report.lines.iter().map(text) {
        if !shown.iter().any(|seen| seen == line) {
            println!("  {line}");
            shown.push(line.to_owned());
        }
    }
    if let Some((_, error)) = &report.stopped {
        println!("  {stopped}: {error}");
    }
    for warning in &report.warnings {
        println!("  note: {warning}");
    }
}

/// Prints the plan: 1 line per file and part, for each app. VS Code and Copilot CLI share 2
/// files, so their lines show once.
fn show_plan(harnesses: &[Harness], options: Options) {
    println!("Plan:");
    let mut shown = Vec::new();
    for &harness in harnesses {
        let report = connect::connect(harness, options, Mode::Plan);
        show(harness, &report, &mut shown, "stopped");
        if harness == Harness::Vscode {
            let dir = connect::copilot_home().map(|dir| dir.display().to_string());
            let dir = dir.unwrap_or_else(|_| "~/.copilot".into());
            println!(
                "  VS Code's Copilot chat reads the Copilot hook and instructions in {dir}, so \
                 VS Code gets those too."
            );
        }
    }
    let shared = harnesses.iter().any(|h| matches!(h, Harness::Vscode | Harness::Copilot));
    if !options.hook && shared {
        println!("VS Code and Copilot CLI always get the sessionStart hook.");
    }
}

/// Connects each app in turn, past any that fails, then checks each one and prints what to
/// do next. Returns the exit code.
fn apply(harnesses: &[Harness], options: Options) -> u8 {
    println!("Applying:");
    let mut reports = Vec::new();
    for &harness in harnesses {
        let report = connect::connect(harness, options, Mode::Apply);
        show(harness, &report, &mut Vec::new(), "failed");
        reports.push((harness, report));
    }
    let skipped = reports.iter().filter(|(_, report)| report.skipped()).count();
    println!("{} connected, {skipped} with skipped parts.", reports.len() - skipped);
    let mut code = u8::from(skipped > 0);
    println!("Checks:");
    let cwd = std::env::current_dir().map_err(|error| format!("no current dir: {error}"));
    let mut store = None;
    let mut next = Vec::new();
    let mut codex_hook = false;
    for (harness, report) in reports.iter().filter(|(_, report)| report.stopped.is_none()) {
        let (harness, mut failed) = (*harness, Vec::new());
        // The parts are in place when a plan now finds nothing to change.
        let plan = connect::connect(harness, options, Mode::Plan);
        for line in plan.lines.iter().filter(|line| !in_place(line)) {
            failed.push(format!("not in place: {}", text(line)));
        }
        failed.extend(plan.stopped.map(|(_, error)| error));
        if harness == Harness::Codex {
            failed.extend(connect::verify_codex().err());
            codex_hook = options.hook && plan.lines.iter().any(hook_in_place);
            next.extend(report.next.iter().cloned());
        }
        if options.hook || matches!(harness, Harness::Vscode | Harness::Copilot) {
            let hook = cwd.clone().and_then(|cwd| check_hook(harness, &cwd));
            failed.extend(hook.err().map(|why| format!("hook: {why}")));
        }
        let store = store.get_or_insert_with(|| cwd.clone().and_then(|cwd| check_store(&cwd)));
        failed.extend(store.clone().err().map(|why| format!("store: {why}")));
        if failed.is_empty() {
            println!("  {}: ok", title(harness));
        } else {
            println!("  {}: {} failed", title(harness), failed.len());
            for failed in failed {
                println!("    {failed}");
            }
            code = 1;
        }
    }
    let connected: Vec<Harness> =
        reports.iter().filter(|(_, report)| report.stopped.is_none()).map(|(h, _)| *h).collect();
    if codex_hook && next.is_empty() {
        next.push(connect::CODEX_APPROVE.into());
    }
    if !connected.is_empty() {
        next.push("Start new chat sessions to load memory.".into());
    }
    if connected.contains(&Harness::Vscode) {
        next.push("VS Code: reload the window to load zerodb.".into());
    }
    if !next.is_empty() {
        println!("Next:");
        for step in next {
            println!("  {step}");
        }
    }
    code
}

/// Whether a plan line says its part is unchanged, as [`connect::connect`] words it in Plan mode.
fn in_place(line: &Line) -> bool {
    line.as_ref().is_ok_and(|line| line.ends_with(" unchanged"))
}

/// Whether a plan line says Codex's SessionStart hook is unchanged.
fn hook_in_place(line: &Line) -> bool {
    in_place(line) && text(line).ends_with(" SessionStart hook unchanged")
}

/// Runs `zerodb hook session-start --harness <harness>` with the stdin that harness gives it
/// for a chat in `cwd`. It must exit 0 within [`HOOK_TIMEOUT`] and print JSON. Its output,
/// which holds memory, is never shown.
fn check_hook(harness: Harness, cwd: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| format!("no zerodb binary: {error}"))?;
    let input = match harness {
        Harness::ClaudeCode | Harness::Codex => {
            json!({"cwd": cwd, "hook_event_name": "SessionStart"})
        }
        Harness::Vscode | Harness::Copilot => json!({"cwd": cwd}),
    };
    let mut command = Command::new(exe);
    command.args(["hook", "session-start", "--harness", harness.name()]);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // Its own group, so a timeout kills whatever it started too.
    command.process_group(0);
    let mut child = crate::spawn(&mut command).map_err(|error| format!("cannot start: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.to_string().as_bytes());
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.take(1 << 20).read_to_end(&mut out);
            let _ = sender.send(out);
        });
    }
    let deadline = Instant::now() + HOOK_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                // The child isn't reaped yet, so its pid, the group's id, can't be reused.
                let group = rustix::process::Pid::from_child(&child);
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                let _ = child.kill();
                let _ = child.wait();
                return Err("no answer in 4 s".into());
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    if !status.success() {
        return Err(format!("it failed ({status})"));
    }
    let out = receiver.recv_timeout(Duration::from_secs(1));
    let out = out.map_err(|_| "it did not close its output".to_owned())?;
    serde_json::from_slice::<Value>(&out).map_err(|_| "it did not print JSON".to_owned())?;
    Ok(())
}

/// Reads `fact_list` from the home, as the hook does for a chat in `cwd`.
fn check_store(cwd: &Path) -> Result<(), String> {
    let session = crate::home::repo(cwd).unwrap_or_else(|| "setup".into());
    let mut home = crate::mcp::HomeReader::new(&session, Instant::now() + STORE_TIMEOUT)?;
    home.read("setup", "fact_list", &json!({"prefix": "user/", "limit": 1})).map(drop)
}
