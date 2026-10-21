//! Server configuration. One JSON file, checked on the open handle before it is read.
//!
//! Errors name the path and the mode, never file content, so a token cannot leak
//! through an error message.

use serde_json::{json, Value};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Shortest accepted token, in bytes.
pub const MIN_TOKEN_BYTES: usize = 32;
/// Longest tenant name, in UTF-8 bytes. The engine's limit.
pub const MAX_TENANT_BYTES: usize = 255;
const MAX_FILE_BYTES: u64 = 1 << 20;
const FIELDS: &str = "data_dir, listen, unix_socket, unix_socket_group, tenants";

/// A tenant's bearer token. `Debug` never shows it.
#[derive(Clone, Eq, PartialEq)]
pub struct Token(String);

impl Token {
    /// Accepts at least 32 visible ASCII bytes, which fit an `Authorization` header as is.
    pub fn new(text: String) -> Result<Self, &'static str> {
        if text.len() < MIN_TOKEN_BYTES {
            return Err("token must be at least 32 bytes");
        }
        if !text.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("token must be visible ASCII without spaces");
        }
        Ok(Self(text))
    }

    /// The secret itself. Send it only in an `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// One tenant and its token.
#[derive(Clone, Debug)]
pub struct Tenant {
    /// Tenant name, 1 to 255 UTF-8 bytes.
    pub name: String,
    /// The token that selects this tenant.
    pub token: Token,
}

/// The TCP listen setting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Listen {
    /// Bind no TCP listener.
    None,
    /// Bind this address.
    Tcp(SocketAddr),
}

/// A checked configuration file. Flags override its fields.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Data directory.
    pub data_dir: Option<PathBuf>,
    /// TCP listen address, or none.
    pub listen: Option<Listen>,
    /// Unix socket path.
    pub unix_socket: Option<PathBuf>,
    /// Group for a 0660 Unix socket, as a name or a numeric gid.
    pub unix_socket_group: Option<String>,
    /// At least one tenant. Tokens are unique.
    pub tenants: Vec<Tenant>,
}

/// A configuration or token file failed to load. The message never holds file content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl Config {
    /// Opens, checks and parses a configuration file.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = read_private(path, "config")?;
        parse(&text).map_err(|message| Error(format!("config {}: {message}", path.display())))
    }

    /// Finds a tenant by name.
    pub fn tenant(&self, name: &str) -> Option<&Tenant> {
        self.tenants.iter().find(|tenant| tenant.name == name)
    }
}

/// Reads a token file with the same checks as the configuration file.
/// Surrounding whitespace, such as a trailing newline, is ignored.
pub fn read_token_file(path: &Path) -> Result<Token, Error> {
    let text = read_private(path, "token file")?;
    Token::new(text.trim().to_owned())
        .map_err(|message| Error(format!("token file {}: {message}", path.display())))
}

/// Writes a new mode 0600 configuration with one tenant and a random token.
/// Never overwrites an existing file. Returns the token so the caller can print it once.
pub fn init(path: &Path, tenant: &str) -> Result<Token, Error> {
    let fail = |message: String| Error(format!("config {}: {message}", path.display()));
    check_tenant_name(tenant).map_err(|message| fail(message.into()))?;
    let token = crate::random_hex(MIN_TOKEN_BYTES)
        .map_err(|error| fail(format!("cannot read /dev/urandom: {error}")))
        .and_then(|hex| Token::new(hex).map_err(|message| fail(message.into())))?;
    let tenants = json!([{"name": tenant, "token": token.expose()}]);
    let mut text = serde_json::to_string_pretty(&json!({ "tenants": tenants }))
        .map_err(|_| fail("cannot encode the configuration".into()))?;
    text.push('\n');
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| fail(format!("cannot create: {error}")))?;
    if let Err(error) = file.write_all(text.as_bytes()).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(fail(format!("cannot write: {error}")));
    }
    Ok(token)
}

/// Opens once, checks the open handle with `fstat`, then reads from the same handle.
/// `O_NONBLOCK` keeps a FIFO from blocking the open. It is then refused as not regular.
fn read_private(path: &Path, what: &str) -> Result<String, Error> {
    let fail = |message: String| Error(format!("{what} {}: {message}", path.display()));
    let nonblock = rustix::fs::OFlags::NONBLOCK.bits() as i32;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nonblock)
        .open(path)
        .map_err(|error| fail(format!("cannot open: {error}")))?;
    let meta = file.metadata().map_err(|error| fail(format!("cannot stat: {error}")))?;
    let euid = rustix::process::geteuid().as_raw();
    let mode = meta.mode() & 0o7777;
    if !meta.file_type().is_file() || meta.uid() != euid || mode & 0o077 != 0 {
        return Err(fail(format!(
            "must be a regular file owned by uid {euid} with mode 0600 or stricter, \
             found mode {mode:04o} owned by uid {}",
            meta.uid()
        )));
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| fail(format!("cannot read: {error}")))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(fail(format!("larger than {MAX_FILE_BYTES} bytes")));
    }
    Ok(text)
}

fn parse(text: &str) -> Result<Config, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        format!("invalid JSON at line {} column {}", error.line(), error.column())
    })?;
    let Value::Object(fields) = value else {
        return Err("expected a JSON object".into());
    };
    let mut config = Config::default();
    for (key, value) in fields {
        match key.as_str() {
            "data_dir" => config.data_dir = Some(non_empty(value, "data_dir")?.into()),
            "unix_socket" => config.unix_socket = Some(non_empty(value, "unix_socket")?.into()),
            "unix_socket_group" => {
                config.unix_socket_group = Some(non_empty(value, "unix_socket_group")?)
            }
            "listen" => {
                let text = non_empty(value, "listen")?;
                config.listen = Some(if text == "none" {
                    Listen::None
                } else {
                    Listen::Tcp(text.parse().map_err(|_| {
                        "listen must be an address such as 127.0.0.1:<port>, or \"none\""
                    })?)
                });
            }
            "tenants" => config.tenants = tenants(value)?,
            _ => return Err(format!("unknown top-level field, expected one of: {FIELDS}")),
        }
    }
    if config.tenants.is_empty() {
        return Err("tenants must name at least one tenant".into());
    }
    Ok(config)
}

fn non_empty(value: Value, field: &str) -> Result<String, String> {
    match value {
        Value::String(text) if !text.is_empty() => Ok(text),
        _ => Err(format!("{field} must be a non-empty string")),
    }
}

fn tenants(value: Value) -> Result<Vec<Tenant>, String> {
    let shape = "tenants must be an array of {\"name\": ..., \"token\": ...} objects";
    let Value::Array(entries) = value else {
        return Err(shape.into());
    };
    let mut tenants: Vec<Tenant> = Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let fail = |message: &str| format!("tenants[{index}]: {message}");
        let Value::Object(mut fields) = entry else {
            return Err(fail(shape));
        };
        let (Some(Value::String(name)), Some(Value::String(token)), true) =
            (fields.remove("name"), fields.remove("token"), fields.is_empty())
        else {
            return Err(fail("needs exactly a string \"name\" and a string \"token\""));
        };
        check_tenant_name(&name).map_err(fail)?;
        let token = Token::new(token).map_err(fail)?;
        if tenants.iter().any(|tenant| tenant.name == name) {
            return Err(fail("two tenants share a name"));
        }
        if tenants.iter().any(|tenant| tenant.token == token) {
            return Err(fail("two tenants share a token"));
        }
        tenants.push(Tenant { name, token });
    }
    Ok(tenants)
}

fn check_tenant_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > MAX_TENANT_BYTES {
        return Err("tenant names must be 1 to 255 UTF-8 bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    mod fs {
        // Cargo doesn't define CARGO_TARGET_TMPDIR for unit tests.
        const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/tmp");
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/support.rs"));
    }

    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::time::Duration;

    const SECRET: &str = "secret-token-0123456789abcdefghijkl";

    enum Kind {
        File(u32),
        Fifo,
    }

    struct Case {
        name: &'static str,
        kind: Kind,
        body: &'static str,
        /// Tenant names on success, or a fragment of the error.
        expect: Result<&'static [&'static str], &'static str>,
    }

    const CASES: &[Case] = &[
        Case {
            name: "two tenants",
            kind: Kind::File(0o600),
            body: r#"{"listen": "none", "tenants": [
                {"name": "a", "token": "secret-token-0123456789abcdefghijkl"},
                {"name": "b", "token": "secret-token-0123456789abcdefghijkX"}]}"#,
            expect: Ok(&["a", "b"]),
        },
        Case {
            name: "fifo is refused without blocking",
            kind: Kind::Fifo,
            body: "",
            expect: Err("must be a regular file"),
        },
        Case {
            name: "group-readable file",
            kind: Kind::File(0o640),
            body: r#"{"tenants": [{"name": "a", "token": "secret-token-0123456789abcdefghijkl"}]}"#,
            expect: Err("found mode 0640"),
        },
        Case {
            name: "duplicate tenant name",
            kind: Kind::File(0o600),
            body: r#"{"tenants": [
                {"name": "a", "token": "secret-token-0123456789abcdefghijkl"},
                {"name": "a", "token": "secret-token-0123456789abcdefghijkX"}]}"#,
            expect: Err("tenants[1]: two tenants share a name"),
        },
        Case {
            name: "duplicate token",
            kind: Kind::File(0o600),
            body: r#"{"tenants": [
                {"name": "a", "token": "secret-token-0123456789abcdefghijkl"},
                {"name": "b", "token": "secret-token-0123456789abcdefghijkl"}]}"#,
            expect: Err("tenants[1]: two tenants share a token"),
        },
        Case {
            name: "short token",
            kind: Kind::File(0o600),
            body: r#"{"tenants": [{"name": "a", "token": "secret-token"}]}"#,
            expect: Err("at least 32 bytes"),
        },
        Case {
            name: "name to token map",
            kind: Kind::File(0o600),
            body: r#"{"tenants": {"a": "secret-token-0123456789abcdefghijkl"}}"#,
            expect: Err("must be an array"),
        },
        Case {
            name: "extra tenant field",
            kind: Kind::File(0o600),
            body: r#"{"tenants": [{"name": "a", "token": "secret-token-0123456789abcdefghijkl", "x": 1}]}"#,
            expect: Err("exactly a string"),
        },
    ];

    #[test]
    fn config_files_are_checked_before_use() {
        let dir = fs::TestDir::new();
        for (index, case) in CASES.iter().enumerate() {
            let path = dir.0.join(format!("config-{index}.json"));
            match case.kind {
                Kind::File(mode) => {
                    std::fs::write(&path, case.body).unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
                }
                Kind::Fifo => {
                    let status = std::process::Command::new("mkfifo")
                        .args(["-m", "600"])
                        .arg(&path)
                        .status()
                        .unwrap();
                    assert!(status.success(), "{}: mkfifo", case.name);
                }
            }
            let (send, receive) = mpsc::channel();
            let load = path.clone();
            std::thread::spawn(move || {
                let _ = send.send(Config::load(&load));
            });
            let loaded = receive
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("{}: load blocked", case.name));
            match (loaded, case.expect) {
                (Ok(config), Ok(names)) => {
                    let got: Vec<&str> = config.tenants.iter().map(|t| t.name.as_str()).collect();
                    assert_eq!(got, names, "{}: tenants", case.name);
                    assert!(!format!("{config:?}").contains(SECRET), "{}: Debug", case.name);
                }
                (Err(error), Err(fragment)) => {
                    let text = error.to_string();
                    assert!(text.contains(fragment), "{}: error {text:?}", case.name);
                    assert!(text.contains(&*path.to_string_lossy()), "{}: path", case.name);
                    assert!(!text.contains("secret-token"), "{}: content echoed", case.name);
                }
                (result, _) => panic!("{}: unexpected {result:?}", case.name),
            }
        }
        let path = dir.0.join("init.json");
        let token = init(&path, "alice").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.tenant("alice").map(|t| &t.token), Some(&token), "init round trip");
        assert!(init(&path, "alice").is_err(), "init never overwrites");
    }
}
