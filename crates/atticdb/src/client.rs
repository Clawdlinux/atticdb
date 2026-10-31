//! The client side of the server's HTTP memory API. It runs 8 operations:
//! `memory_append`, `memory_recent`, `memory_search`, `fact_set`, `fact_get`, `fact_list`,
//! and the file refs `file_ref` and `file_get`, which are facts under `file<path>`.
//! Each one checks its arguments, hides secrets in writes, sends the request with retries,
//! and shapes the result. The MCP adapter, the shell verbs and the session-start hook use it.
//! The studio uses it only to find the store and token, then sends its own reads.

use crate::config::{read_token_file, Config, Token};
use crate::launch::{Launch, Url};
use crate::outcome::{self, Attempt, Merge, Outcome};
use crate::redact::{Counts, Redactor};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The environment variable that may hold the token.
pub const TOKEN_ENV: &str = "ATTICDB_TOKEN";
/// The agent of `atticdb mcp` without `--agent`, and of the shell verbs. The session-start
/// hook reads its notes too.
pub const DEFAULT_AGENT: &str = "mcp";
const FIRST_BACKOFF: Duration = Duration::from_millis(50);
const MAX_BACKOFF: Duration = Duration::from_secs(2);
/// Time kept for the request itself: no attempt connects or sends with less than this
/// left before the deadline.
const MIN_ATTEMPT: Duration = Duration::from_secs(2);

/// A store connection as 1 agent, with a default session.
pub(crate) struct Client {
    agent: String,
    /// The session of calls that name none. The MCP adapter sets it from the client's roots.
    pub(crate) session: String,
    token: Token,
    retry_deadline: Duration,
    launch: Launch,
}

/// Runs one tool from a shell, with arguments from `--<name>` flags. Prints the tool's
/// `structuredContent` as one JSON line. Exits 0, 1 when nothing was written or a read
/// failed, 2 for a bad flag, or 3 when a write's outcome is unknown.
pub fn command(tool: &str, mut flags: HashMap<String, String>) -> u8 {
    let mut args = json!({});
    for name in ["content", "role", "key", "prefix", "after", "query", "path", "about"] {
        if let Some(value) = flags.remove(&format!("--{name}")) {
            args[name] = json!(value);
        }
    }
    let numbers =
        [("--n", "n"), ("--limit", "limit"), ("--max-bytes", "max_bytes"), ("--before", "before")];
    for (flag, name) in numbers {
        match flags.remove(flag).map(|text| text.parse::<u64>()) {
            Some(Ok(number)) => args[name] = json!(number),
            Some(Err(_)) => {
                eprintln!("atticdb: {flag} must be a whole number");
                return 2;
            }
            None => {}
        }
    }
    if flags.remove("--delete").is_some() {
        args["delete"] = json!(true);
    }
    if flags.remove("--versions").is_some() {
        args["versions"] = json!(true);
    }
    let result = match Client::new(flags) {
        Ok(mut client) => client.call(tool, &args).unwrap_or_default(),
        Err(message) => tool_error(message),
    };
    if result["isError"] != true {
        println!("{}", result["structuredContent"]);
        return 0;
    }
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    eprintln!("atticdb: {text}");
    if text.starts_with("unknown:") {
        3
    } else {
        1
    }
}

/// Reads from the per-user home for code in this crate, such as the session-start hook.
pub(crate) struct HomeReader {
    client: Client,
    deadline: Instant,
}

impl HomeReader {
    /// Resolves the home `lookup` finds as `atticdb mcp` with no connection flags does,
    /// quietly ignoring `ATTICDB_TOKEN`. All reads together, connects and server starts
    /// included, end by `deadline`.
    pub(crate) fn in_home(
        session: &str,
        deadline: Instant,
        lookup: &crate::home::Lookup,
    ) -> Result<Self, String> {
        let flags = HashMap::from([("--session".into(), session.into())]);
        let client = Client::open(flags, false, lookup)?;
        Ok(Self { client, deadline })
    }

    /// Runs the read tool `tool` as `agent`. Returns its `structuredContent`, or its error.
    /// With under 2 s left it sends nothing and fails at once.
    pub(crate) fn read(&mut self, agent: &str, tool: &str, args: &Value) -> Result<Value, String> {
        self.client.agent = agent.to_owned();
        // `retry` and every exchange in it end at now + `retry_deadline`.
        self.client.retry_deadline = self.deadline.saturating_duration_since(Instant::now());
        let result = self.client.call(tool, args).ok_or_else(|| format!("unknown tool {tool}"))?;
        match result["content"][0]["text"].as_str() {
            Some(text) if result["isError"] == true => Err(text.to_owned()),
            _ => Ok(result["structuredContent"].clone()),
        }
    }
}

/// Which store the connection flags chose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoreKind {
    /// The per-user home: no connection flags were given.
    Home,
    /// A data directory, from `--data-dir` or the config.
    DataDir,
    /// The server at `--url`.
    Url,
}

/// A store and its token, resolved from the connection flags.
#[derive(Debug)]
pub(crate) struct Store {
    pub(crate) launch: Launch,
    pub(crate) token: Token,
    pub(crate) kind: StoreKind,
    /// The tenant whose config entry gave the token. `None` for a token file or
    /// `ATTICDB_TOKEN`, since then the server alone knows the tenant.
    pub(crate) tenant: Option<String>,
    /// The home directory, when the home is used.
    pub(crate) home: Option<PathBuf>,
}

/// `--url`, `--data-dir`, `--config`, `--token-file` and `--tenant`, taken out of a
/// command's flags. Nothing on disk is touched yet.
#[derive(Debug)]
pub(crate) struct StoreFlags {
    url: Option<Url>,
    data_dir: Option<String>,
    config: Option<String>,
    token_file: Option<String>,
    tenant: Option<String>,
}

impl StoreFlags {
    /// Removes the connection flags from `flags`. Fails on a bad `--url`.
    pub(crate) fn take(flags: &mut HashMap<String, String>) -> Result<Self, String> {
        Ok(Self {
            url: flags.remove("--url").as_deref().map(Url::parse).transpose()?,
            data_dir: flags.remove("--data-dir"),
            config: flags.remove("--config"),
            token_file: flags.remove("--token-file"),
            tenant: flags.remove("--tenant"),
        })
    }

    /// Resolves the store and its token. With no connection flags, it uses the per-user
    /// home, creating it if missing, and ignores `ATTICDB_TOKEN`. With `note`, says so on
    /// stderr when that variable is set.
    pub(crate) fn open(self, note: bool) -> Result<Store, String> {
        self.open_in(note, &crate::home::Lookup::ENV)
    }

    /// [`Self::open`], with the home `lookup` finds.
    fn open_in(self, note: bool, lookup: &crate::home::Lookup) -> Result<Store, String> {
        let Self { url, data_dir, config, token_file, tenant } = self;
        // With no connection flags, use the per-user home and its token, never the
        // environment's.
        let flags = [&data_dir, &config, &token_file, &tenant];
        let given = url.is_some() || flags.iter().any(|flag| flag.is_some());
        let home = (!given).then(|| lookup.ensure()).transpose()?;
        if home.is_some() && std::env::var_os(TOKEN_ENV).is_some() && note {
            eprintln!(
                "atticdb: ignoring {TOKEN_ENV}: no --url, --data-dir or --config given, so the \
                 home's token is used"
            );
        }
        let env_token = std::env::var(TOKEN_ENV).ok().filter(|_| home.is_none());
        // Absolute, as a started server runs in `/`.
        let config_path = config.map(std::path::absolute).transpose();
        let config_path = config_path.map_err(|error| format!("--config: {error}"))?;
        let config_path = config_path.or_else(|| home.as_ref().map(|home| home.config.clone()));
        let config = config_path.as_deref().map(Config::load).transpose()?;
        // Absolute against this folder, as a started server runs in `/`.
        let unix_socket = config.as_ref().and_then(|config| config.unix_socket.as_deref());
        let unix_socket = unix_socket.map(std::path::absolute).transpose();
        let unix_socket = unix_socket.map_err(|error| format!("unix_socket: {error}"))?;
        let tenant = tenant.or_else(|| home.as_ref().map(|home| home.tenant.clone()));
        let (token, tenant) = match (token_file, env_token, tenant) {
            (Some(path), _, _) => (read_token_file(path.as_ref())?, None),
            (None, Some(text), _) => {
                (Token::new(text).map_err(|error| format!("{TOKEN_ENV}: {error}"))?, None)
            }
            (None, None, Some(name)) => {
                let entry = config.as_ref().and_then(|config| config.tenant(&name));
                let token = entry.map(|tenant| tenant.token.clone());
                (token.ok_or_else(|| format!("tenant {name:?} is not in the config"))?, Some(name))
            }
            _ => return Err(format!("set {TOKEN_ENV}, --token-file, or --config with --tenant")),
        };
        let kind = match (&url, &home) {
            (Some(_), _) => StoreKind::Url,
            (None, Some(_)) => StoreKind::Home,
            (None, None) => StoreKind::DataDir,
        };
        let home_dir = home.as_ref().map(|home| home.dir.clone());
        let data_dir = data_dir.map(PathBuf::from);
        let data_dir = data_dir.or(home.map(|home| home.data_dir));
        let data_dir = data_dir.or_else(|| config.and_then(|config| config.data_dir));
        let data_dir = data_dir.map(std::path::absolute).transpose().map_err(|e| e.to_string())?;
        // Someone who can write there could name a server of theirs in `LOCK`.
        #[cfg(windows)]
        if let Some(path) = &data_dir {
            crate::platform::check_in_profile(path, "data dir")?;
        }
        if url.is_none() && data_dir.is_none() {
            return Err("set --url, or --data-dir or a config with data_dir".into());
        }
        let launch = Launch::new(url, data_dir, config_path, unix_socket);
        #[cfg(windows)]
        let launch = launch.with_token(token.clone());
        Ok(Store { launch, token, kind, tenant, home: home_dir })
    }
}

impl Client {
    /// Opens the store the connection flags in `flags` name. It takes `--agent`, `--session`
    /// and `--retry-deadline` too.
    pub(crate) fn new(flags: HashMap<String, String>) -> Result<Self, String> {
        Self::open(flags, true, &crate::home::Lookup::ENV)
    }

    /// With `note`, says on stderr when the home is used and `ATTICDB_TOKEN` is ignored.
    fn open(
        mut flags: HashMap<String, String>,
        note: bool,
        lookup: &crate::home::Lookup,
    ) -> Result<Self, String> {
        let store = StoreFlags::take(&mut flags)?;
        let mut duration = |name: &str, default: u64| match flags.remove(name) {
            None => Ok(Duration::from_secs(default)),
            Some(text) => match text.strip_suffix("ms") {
                Some(ms) => ms.parse().map(Duration::from_millis).ok(),
                None => {
                    text.strip_suffix('s').and_then(|s| s.parse().ok()).map(Duration::from_secs)
                }
            }
            .ok_or(format!("{name} must be <n>s or <n>ms")),
        };
        // Checked before the store is opened, which may create the home.
        let retry_deadline = duration("--retry-deadline", 30)?;
        let Store { launch, token, .. } = store.open_in(note, lookup)?;
        let session = flags.remove("--session");
        Ok(Self {
            agent: flags.remove("--agent").unwrap_or_else(|| DEFAULT_AGENT.into()),
            // The repo of the current directory, as the session-start hook reads it, until
            // the MCP client's roots name one.
            session: session.unwrap_or_else(|| {
                let repo = std::env::current_dir().ok().and_then(|dir| crate::home::repo(&dir));
                repo.unwrap_or_else(|| "default".into())
            }),
            token,
            retry_deadline,
            launch,
        })
    }

    /// Runs the operation `name` with the arguments `args`. Returns its tool result:
    /// `content`, and `structuredContent` or `isError`. `None` for an unknown name.
    pub(crate) fn call(&mut self, name: &str, args: &Value) -> Option<Value> {
        Some(match name {
            "memory_append" => self.append(args),
            "memory_recent" => self.recent(args),
            "memory_search" => self.search(args),
            "fact_set" => self.fact_set(args),
            "fact_get" => self.fact_get(args),
            "fact_list" => self.fact_list(args),
            "file_ref" => self.file_ref(args),
            "file_get" => self.file_get(args),
            _ => return None,
        })
    }

    fn append(&mut self, args: &Value) -> Value {
        let turn = (|| {
            let content = string(args, "content")?.ok_or("content is required")?;
            let role = string(args, "role")?.unwrap_or_else(|| "assistant".into());
            let mut turn = json!({"role": role, "content": content});
            match &args["meta"] {
                Value::Null => {}
                meta @ Value::Object(_) => turn["meta"] = meta.clone(),
                _ => return Err("meta must be an object".to_owned()),
            }
            Ok((session_arg(args)?, turn))
        })();
        let (session, mut turn) = match turn {
            Ok(turn) => turn,
            Err(message) => return tool_error(message),
        };
        let hidden = redact(&mut turn);
        let session = session.unwrap_or_else(|| self.session.clone());
        let body = json!({"agent": self.agent, "session": session, "turns": [turn]});
        reported(self.write("/v1/append", body), hidden)
    }

    fn fact_set(&mut self, args: &Value) -> Value {
        let body = (|| {
            let key = string(args, "key")?.ok_or("key is required")?;
            let mut body = json!({"key": key, "agent": self.agent});
            let delete = args["delete"] == true;
            match (string(args, "content")?, delete) {
                (Some(content), false) => body["content"] = json!(content),
                (None, true) => {}
                _ => return Err("pass exactly one of content or delete: true".to_owned()),
            }
            match &args["meta"] {
                Value::Null => {}
                meta @ Value::Object(_) if !delete => body["meta"] = meta.clone(),
                _ => return Err("meta must be an object, and only with content".into()),
            }
            Ok((body, delete))
        })();
        match body {
            Ok((body, true)) => self.write("/v1/facts/delete", body),
            Ok((mut body, false)) => {
                let hidden = redact(&mut body);
                reported(self.write("/v1/facts/put", body), hidden)
            }
            Err(message) => tool_error(message),
        }
    }

    fn fact_get(&mut self, args: &Value) -> Value {
        match string(args, "key").and_then(|key| key.ok_or("key is required".into())) {
            Ok(key) => {
                self.read("fact_get", &format!("/v1/facts/get?key={}", encode(&key)), "fact", fact)
            }
            Err(message) => tool_error(message),
        }
    }

    fn fact_list(&mut self, args: &Value) -> Value {
        let target = (|| {
            let prefix = string(args, "prefix")?.ok_or("prefix is required")?;
            let (limit, max_bytes) =
                (number(args, "limit", 50)?.min(200), number(args, "max_bytes", 64 << 10)?);
            let mut target = format!(
                "/v1/facts/list?prefix={}&limit={limit}&max_bytes={max_bytes}",
                encode(&prefix)
            );
            if let Some(after) = string(args, "after")? {
                target += &format!("&after={}", encode(&after));
            }
            Ok::<_, String>(target)
        })();
        match target {
            Ok(target) => self.read("fact_list", &target, "facts", facts),
            Err(message) => tool_error(message),
        }
    }

    /// Saves a reference to a local file as the fact `file<absolute path>`: `about` as content,
    /// and size, modified time, blake3 and MIME type as meta. Each revision is a version. The
    /// same bytes and about as the current ref write nothing.
    fn file_ref(&mut self, args: &Value) -> Value {
        let checked = (|| {
            let path = string(args, "path")?.ok_or("path is required")?;
            let about = string(args, "about")?.map(|about| about.trim().to_owned());
            let about = about.filter(|about| !about.is_empty()).ok_or("about is required")?;
            let path =
                std::fs::canonicalize(&path).map_err(|_| format!("path not found: {path}"))?;
            let (bytes, modified_ns) = stat(&path).ok_or("path must be a file")?;
            let key = file_key(&path)?;
            let blake3 = hash_file(&path)?;
            let meta = json!({"file": {"bytes": bytes, "modified_ns": modified_ns,
                                       "blake3": blake3, "mime": mime(&path)}});
            Ok::<_, String>((key, about, blake3, meta))
        })();
        let (key, about, blake3, meta) = match checked {
            Ok(checked) => checked,
            Err(message) => return tool_error(message),
        };
        let current = self.fact_get(&json!({"key": key}));
        if current["isError"] == true {
            return current;
        }
        let fact = &current["structuredContent"]["fact"];
        // The stored about is redacted, so compare it with the redacted one.
        let stored = Redactor::from_env().text(&about, &mut Counts::new());
        if fact["content"] == stored.as_str() && fact["meta"]["file"]["blake3"] == blake3.as_str() {
            let structured =
                json!({"unchanged": true, "key": key, "lsn": fact["lsn"], "blake3": blake3});
            return answer(format!("unchanged {key}"), structured);
        }
        let mut result = self.fact_set(&json!({"key": key, "content": about, "meta": meta}));
        if let Some(structured) = result["structuredContent"].as_object_mut() {
            structured.insert("key".into(), json!(key));
            structured.insert("blake3".into(), json!(blake3));
            structured.insert("unchanged".into(), json!(false));
        }
        result
    }

    /// Reads the ref of a file, with its status now: `ok`, `changed` or `missing`. With
    /// `versions`, every kept revision too, newest first, and `on_disk`: the one whose bytes
    /// are on disk now. A folder lists the refs under it.
    fn file_get(&mut self, args: &Value) -> Value {
        let path = match string(args, "path").and_then(|p| p.ok_or("path is required".into())) {
            Ok(path) => path,
            Err(message) => return tool_error(message),
        };
        let path = resolve(&path);
        let key = match file_key(&path) {
            Ok(key) => key,
            Err(message) => return tool_error(message),
        };
        if path.is_dir() {
            return self.file_list(&key);
        }
        let current = self.fact_get(&json!({"key": key}));
        if current["isError"] == true {
            return current;
        }
        let fact = &current["structuredContent"]["fact"];
        if fact.is_null() {
            return answer(format!("no ref for {key}"), json!({"ref": null}));
        }
        let mut hash = None;
        let status = file_status(&path, &fact["meta"]["file"], &mut hash);
        let mut text =
            vec![format!("{key} [{status}]: {}", fact["content"].as_str().unwrap_or(""))];
        let mut structured = json!({"ref": file_ref_json(fact, status)});
        if args["versions"] != true {
            return answer(text.join("\n"), structured);
        }
        let target = format!("/v1/facts/history?key={}&max_bytes={}", encode(&key), 1 << 20);
        let history = self
            .read("file_get", &target, "revisions", |reply| answer(String::new(), reply.clone()));
        if history["isError"] == true {
            return history;
        }
        let revisions = history["structuredContent"]["revisions"].as_array().cloned();
        let versions: Vec<Value> = revisions
            .unwrap_or_default()
            .iter()
            .filter(|revision| revision["deleted"] != true)
            .map(|revision| {
                let file = &revision["meta"]["file"];
                json!({"lsn": revision["lsn"], "commit_time_ns": revision["commit_time_ns"],
                       "about": revision["content"], "bytes": file["bytes"],
                       "blake3": file["blake3"], "mime": file["mime"]})
            })
            .collect();
        let on_disk = match status {
            "missing" => None,
            _ => hash.or_else(|| fact["meta"]["file"]["blake3"].as_str().map(str::to_owned)),
        };
        let on_disk = on_disk.and_then(|hash| {
            versions
                .iter()
                .find(|version| version["blake3"] == hash.as_str())
                .map(|v| v["lsn"].clone())
        });
        for version in &versions {
            let short: String = version["blake3"].as_str().unwrap_or("").chars().take(12).collect();
            let about = version["about"].as_str().unwrap_or("");
            text.push(format!(
                "lsn {} {} bytes {short}: {about}",
                version["lsn"], version["bytes"]
            ));
        }
        text.push(match &on_disk {
            Some(lsn) => format!("on disk: lsn {lsn}"),
            None => "on disk: no saved version".into(),
        });
        structured["versions"] = json!(versions);
        structured["on_disk"] = on_disk.unwrap_or(Value::Null);
        answer(text.join("\n"), structured)
    }

    /// The refs under the folder whose key is `key`, up to 200, each with its status.
    fn file_list(&mut self, key: &str) -> Value {
        let prefix = if key.ends_with('/') { key.to_owned() } else { format!("{key}/") };
        let listed = self.fact_list(&json!({"prefix": prefix, "limit": 200}));
        if listed["isError"] == true {
            return listed;
        }
        let facts = listed["structuredContent"]["facts"].as_array().cloned().unwrap_or_default();
        let mut text = Vec::new();
        let refs: Vec<Value> = facts
            .iter()
            .map(|fact| {
                let path = file_path(fact["key"].as_str().unwrap_or(""));
                let status = file_status(&path, &fact["meta"]["file"], &mut None);
                let about = fact["content"].as_str().unwrap_or("");
                text.push(format!("{} [{status}]: {about}", fact["key"].as_str().unwrap_or("")));
                file_ref_json(fact, status)
            })
            .collect();
        if text.is_empty() {
            text.push(format!("no refs under {prefix}"));
        }
        let next = listed["structuredContent"]["next"].clone();
        if let Some(next) = next.as_str() {
            text.push(format!("more: list a narrower folder, or fact_list with after={next}"));
        }
        answer(text.join("\n"), json!({"refs": refs, "next": next}))
    }

    /// Posts a write under one new retry key, judged by the outcome rule across attempts.
    fn write(&mut self, target: &str, mut body: Value) -> Value {
        match crate::random_hex(16) {
            Ok(key) => body["idem_key"] = json!(key),
            Err(error) => return tool_error(crate::platform::no_random(error)),
        }
        let (mut merge, mut reasons) = (Merge::default(), Vec::new());
        self.retry("POST", target, body.to_string().as_bytes(), |reply| {
            let (attempt, detail) = match &reply {
                Ok(reply) => match reply["outcome"].as_str().and_then(outcome::parse) {
                    Some(outcome) => (Attempt::Answered(outcome), describe(reply)),
                    None => (Attempt::TransportError, "malformed response".into()),
                },
                Err((attempt, error)) => (*attempt, error.clone()),
            };
            let detail = note(&mut reasons, detail);
            match (merge.record(attempt), reply) {
                (Some(Outcome::Committed), Ok(reply)) => Ok(committed(&reply, target)),
                (Some(outcome), _) => Ok(failed(outcome, &detail)),
                (None, _) => Err(failed(merge.finish(), &detail)),
            }
        })
    }

    fn recent(&mut self, args: &Value) -> Value {
        let query = (|| {
            let session = session_arg(args)?.unwrap_or_else(|| self.session.clone());
            let max_bytes = number(args, "max_bytes", 64 << 10)?;
            Ok::<_, String>((number(args, "n", 20)?.min(200), session, max_bytes))
        })();
        let (n, session, max_bytes) = match query {
            Ok(query) => query,
            Err(message) => return tool_error(message),
        };
        let (agent, session) = (encode(&self.agent), encode(&session));
        let target =
            format!("/v1/window?agent={agent}&session={session}&last={n}&max_bytes={max_bytes}");
        self.read("memory_recent", &target, "turns", turns)
    }

    fn search(&mut self, args: &Value) -> Value {
        let target = (|| {
            let query = string(args, "query")?.ok_or("query is required")?;
            let (limit, max_bytes) =
                (number(args, "limit", 10)?.clamp(1, 50), number(args, "max_bytes", 64 << 10)?);
            let mut target =
                format!("/v1/search?q={}&limit={limit}&max_bytes={max_bytes}", encode(&query));
            if !args["before"].is_null() {
                target += &format!("&before={}", number(args, "before", 0)?);
            }
            Ok::<_, String>(target)
        })();
        match target {
            Ok(target) => self.read("memory_search", &target, "hits", hits),
            Err(message) => tool_error(message),
        }
    }

    /// Sends a read until it is answered. The reply field `field` marks success.
    fn read(
        &mut self,
        tool: &str,
        target: &str,
        field: &str,
        render: fn(&Value) -> Value,
    ) -> Value {
        let mut reasons = Vec::new();
        self.retry("GET", target, &[], |reply| match reply {
            Ok(reply) if reply.get(field).is_some() => Ok(render(&reply)),
            Ok(reply) => {
                let detail = note(&mut reasons, describe(&reply));
                Ok(tool_error(format!("{tool} failed: {detail}")))
            }
            Err((_, error)) => {
                Err(tool_error(format!("{tool} failed: {}", note(&mut reasons, error))))
            }
        })
    }

    /// Sends one request, reconnecting before each attempt, until `judge` returns `Ok`.
    /// `Err` holds the result to report if the deadline passes first.
    fn retry(
        &mut self,
        method: &str,
        target: &str,
        body: &[u8],
        mut judge: impl FnMut(Result<Value, (Attempt, String)>) -> Result<Value, Value>,
    ) -> Value {
        let deadline = Instant::now() + self.retry_deadline;
        let mut backoff = FIRST_BACKOFF;
        loop {
            let reply = self.request(method, target, body, deadline);
            match judge(reply) {
                Ok(result) => return result,
                Err(result) if self.launch.stuck() => return result,
                Err(result) if Instant::now() + backoff + MIN_ATTEMPT > deadline => return result,
                Err(_) => std::thread::sleep(jitter(backoff)),
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// One attempt, connect included, bounded by the call's deadline.
    fn request(
        &mut self,
        method: &str,
        target: &str,
        body: &[u8],
        deadline: Instant,
    ) -> Result<Value, (Attempt, String)> {
        let connect_by = deadline.checked_sub(MIN_ATTEMPT).unwrap_or(deadline);
        // Checked before connecting and before sending. Nothing sent merges as
        // not_committed, which never clears an earlier unknown.
        let reserve = || {
            let late = (Attempt::NotSent, "less than 2 s left before the deadline".to_owned());
            (Instant::now() < connect_by).then_some(()).ok_or(late)
        };
        reserve()?;
        let (mut stream, host) =
            self.launch.connect(connect_by).map_err(|e| (Attempt::NotSent, e.to_string()))?;
        reserve()?;
        let auth = format!("Bearer {}", self.token.expose());
        let headers = [("Host", host.as_str()), ("Authorization", auth.as_str())];
        let lost = |error: String| (Attempt::TransportError, error);
        let max = crate::api::MAX_RESPONSE_BYTES;
        let response =
            crate::http::exchange(&mut *stream, method, target, &headers, body, deadline, max);
        let response = response.map_err(|error| lost(error.to_string()))?;
        serde_json::from_slice(&response.body).map_err(|_| lost("response is not JSON".into()))
    }
}

fn number(args: &Value, name: &str, default: u64) -> Result<u64, String> {
    match &args[name] {
        Value::Null => Ok(default),
        value => value.as_u64().ok_or(format!("{name} must be a whole number")),
    }
}

/// A tool result with one text block.
fn answer(text: String, structured: Value) -> Value {
    json!({"content": [{"type": "text", "text": text}], "structuredContent": structured})
}

fn string(args: &Value, name: &str) -> Result<Option<String>, String> {
    match &args[name] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(format!("{name} must be a string")),
    }
}

/// The `session` argument. A model may fill an optional field with "", so an empty one is
/// absent too: a note in session "" would never reach the session-start block.
pub(crate) fn session_arg(args: &Value) -> Result<Option<String>, String> {
    Ok(string(args, "session")?.filter(|session| !session.is_empty()))
}

/// Adds a reason once and returns every reason so far, in order. The last alone would
/// depend on timing.
fn note(reasons: &mut Vec<String>, reason: String) -> String {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
    reasons.join("; ")
}

/// The server's error kind and message, for the model.
fn describe(reply: &Value) -> String {
    let error = reply["error"].as_str().unwrap_or("no error kind");
    match reply["message"].as_str() {
        Some(message) => format!("{error}: {message}"),
        None => error.to_owned(),
    }
}

/// Hides secrets in a write's `content` and `meta` strings. Returns what it hid.
fn redact(object: &mut Value) -> Counts {
    let (redactor, mut hidden) = (Redactor::from_env(), Counts::new());
    for field in ["content", "meta"] {
        if let Some(value) = object.get_mut(field) {
            redactor.json(value, &mut hidden);
        }
    }
    hidden
}

/// Adds `"redacted": {"<type>": <count>}` to a committed write's result, when anything was
/// hidden.
fn reported(mut result: Value, hidden: Counts) -> Value {
    if hidden.is_empty() || result["structuredContent"].is_null() {
        return result;
    }
    let counts = json!(hidden);
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    result["content"][0]["text"] = json!(format!("{text}; redacted {counts}"));
    result["structuredContent"]["redacted"] = counts;
    result
}

/// One `structuredContent` shape per tool, `seq` for appends only. Receipt fields are null
/// when the server had no receipt.
fn committed(reply: &Value, target: &str) -> Value {
    let append = target == "/v1/append";
    let lsn = &reply[if append { "last_lsn" } else { "lsn" }];
    let mut structured =
        json!({"committed": true, "lsn": lsn, "deduplicated": reply["deduplicated"]});
    let (name, number) = if append { ("seq", &reply["seqs"][0]) } else { ("lsn", lsn) };
    if append {
        structured["seq"] = number.clone();
    }
    let text = match number {
        Value::Null => "committed, receipt unavailable".into(),
        number => format!("committed {name}={number}"),
    };
    answer(text, structured)
}

fn fact(reply: &Value) -> Value {
    let fact = &reply["fact"];
    let text = match fact["content"].as_str() {
        Some("") => "(empty)".into(),
        Some(content) => content.to_owned(),
        None => "no current value".into(),
    };
    answer(text, json!({"fact": fact}))
}

fn facts(reply: &Value) -> Value {
    let facts = reply["facts"].as_array().map(Vec::as_slice).unwrap_or_default();
    let field = |fact: &Value, name: &str| fact[name].as_str().unwrap_or_default().to_owned();
    let mut text: Vec<String> = facts
        .iter()
        .map(|fact| format!("{}: {}", field(fact, "key"), field(fact, "content")))
        .collect();
    let next = reply["next"].as_str();
    if text.is_empty() {
        text.push("no current facts".into());
    }
    if let Some(next) = next {
        text.push(format!("more: call again with after={next}"));
    }
    answer(text.join("\n"), json!({"facts": facts, "next": reply["next"]}))
}

fn hits(reply: &Value) -> Value {
    let hits = reply["hits"].as_array().map(Vec::as_slice).unwrap_or_default();
    let field = |hit: &Value, name: &str| hit[name].as_str().unwrap_or_default().to_owned();
    let mut text: Vec<String> = hits
        .iter()
        .map(|hit| match hit["kind"].as_str() {
            Some("fact") => format!("[fact {}] {}", field(hit, "key"), field(hit, "content")),
            _ => format!(
                "[{}/{} #{} {}] {}",
                field(hit, "agent"),
                field(hit, "session"),
                hit["seq"],
                field(hit, "role"),
                field(hit, "content")
            ),
        })
        .collect();
    if text.is_empty() {
        text.push("no matches".into());
    }
    if let Some(next) = reply["next"].as_u64() {
        text.push(format!("more: call again with before={next}"));
    }
    answer(text.join("\n"), json!({"hits": hits, "next": reply["next"]}))
}

fn failed(outcome: Outcome, detail: &str) -> Value {
    tool_error(match outcome {
        Outcome::NotCommitted => {
            format!("not_committed: nothing was written ({detail}); calling again is safe")
        }
        _ => format!(
            "unknown: the write may be saved, and repeating the call may duplicate it ({detail})"
        ),
    })
}

fn turns(reply: &Value) -> Value {
    let turns = reply["turns"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut text: Vec<String> = turns
        .iter()
        .map(|turn| {
            let content = turn["content"].as_str().unwrap_or_default();
            format!("#{} {}: {content}", turn["seq"], turn["role"].as_str().unwrap_or_default())
        })
        .collect();
    if reply["truncated"] == true {
        text.push("(older turns left out to fit max_bytes)".into());
    }
    let text = if text.is_empty() { "no turns".into() } else { text.join("\n") };
    answer(text, json!({"turns": turns, "truncated": reply["truncated"]}))
}

/// The facts key of a file ref: `file` and the absolute path with `/` separators, so a folder
/// is a key prefix. Keys are at most 1024 bytes.
fn file_key(path: &std::path::Path) -> Result<String, String> {
    let text = path.to_str().ok_or("path must be UTF-8")?;
    // Windows `canonicalize` returns `\\?\C:\...`, and `\\?\UNC\server\share\...` for shares and
    // mapped drives. A share's key is `file//server/share/...`.
    let text = match text.strip_prefix(r"\\?\UNC\") {
        Some(share) => format!(r"\\{share}"),
        None => text.strip_prefix(r"\\?\").unwrap_or(text).to_owned(),
    };
    let text = text.replace('\\', "/");
    let key = if text.starts_with('/') { format!("file{text}") } else { format!("file/{text}") };
    if key.len() > 1024 {
        return Err("path is too long: its key is over 1024 bytes".into());
    }
    Ok(key)
}

/// The path of a file ref key.
fn file_path(key: &str) -> PathBuf {
    let rest = key.strip_prefix("file").unwrap_or(key);
    if cfg!(windows) {
        if rest.starts_with("//") {
            return PathBuf::from(rest.replace('/', "\\"));
        }
        if let Some(drive) = rest.strip_prefix('/').filter(|d| d.as_bytes().get(1) == Some(&b':')) {
            return PathBuf::from(drive);
        }
    }
    PathBuf::from(rest)
}

/// `path` as `canonicalize` gives it. A path that no longer exists keeps its nearest existing
/// ancestor's real path, so a deleted file finds the key it was saved under.
fn resolve(path: &str) -> PathBuf {
    use std::path::Component;
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let Ok(absolute) = std::path::absolute(path) else { return PathBuf::from(path) };
    let parts: Vec<Component> = absolute.components().collect();
    for end in (1..parts.len()).rev() {
        let Ok(mut real) = std::fs::canonicalize(parts[..end].iter().collect::<PathBuf>()) else {
            continue;
        };
        for part in &parts[end..] {
            match part {
                Component::ParentDir => {
                    real.pop();
                }
                Component::CurDir => {}
                part => real.push(part),
            }
        }
        return real;
    }
    absolute
}

/// Size and modified time in ns of a regular file. `None` for anything else.
fn stat(path: &std::path::Path) -> Option<(u64, u64)> {
    let metadata = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file)?;
    let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((metadata.len(), u64::try_from(modified.as_nanos()).unwrap_or(u64::MAX)))
}

fn hash_file(path: &std::path::Path) -> Result<String, String> {
    let mut hasher = blake3::Hasher::new();
    let file = std::fs::File::open(path).map_err(|error| format!("cannot read file: {error}"))?;
    hasher.update_reader(file).map_err(|error| format!("cannot read file: {error}"))?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// `missing`, `ok` or `changed`. When size or modified time differ, the hash decides, and
/// `hash` keeps it.
fn file_status(path: &std::path::Path, saved: &Value, hash: &mut Option<String>) -> &'static str {
    let Some((bytes, modified_ns)) = stat(path) else { return "missing" };
    if saved["bytes"] == bytes && saved["modified_ns"] == modified_ns {
        return "ok";
    }
    *hash = hash_file(path).ok();
    match hash {
        Some(hash) if saved["blake3"] == hash.as_str() => "ok",
        _ => "changed",
    }
}

fn file_ref_json(fact: &Value, status: &str) -> Value {
    json!({"key": fact["key"], "about": fact["content"], "lsn": fact["lsn"],
           "commit_time_ns": fact["commit_time_ns"], "file": fact["meta"]["file"],
           "status": status})
}

fn mime(path: &std::path::Path) -> &'static str {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn tool_error(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

/// Percent-encodes a query value.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// A random wait between half and all of `backoff`, so retrying adapters spread out.
fn jitter(backoff: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let random = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let half = backoff / 2;
    half + Duration::from_nanos(random % (half.as_nanos() as u64 + 1))
}
