//! Server configuration. One JSON file, checked on the open handle before it is read.
//!
//! Errors name the path and the mode, never file content, so a token cannot leak
//! through an error message.

#[cfg(not(unix))]
use crate::platform::OpenOptionsExt;
use serde_json::{json, Value};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Shortest accepted token, in bytes.
pub const MIN_TOKEN_BYTES: usize = 32;
/// Longest accepted token, in bytes. Its `Authorization` header fits the 16 KiB head.
pub const MAX_TOKEN_BYTES: usize = 1024;
/// Longest tenant name, in UTF-8 bytes. The engine's limit.
pub const MAX_TENANT_BYTES: usize = 255;
pub(crate) const MAX_FILE_BYTES: u64 = 1 << 20;
/// Smallest `segment_mib`. A batch must fit in 1 segment, and 16 MiB is twice the 8 MiB body cap.
pub const MIN_SEGMENT_MIB: u64 = 16;
/// Largest `segment_mib`.
pub const MAX_SEGMENT_MIB: u64 = 1024;
const FIELDS: &str = "data_dir, listen, unix_socket, unix_socket_group, compression, \
                      search_index_mib, segment_mib, tenants";

/// A tenant's bearer token. `Debug` never shows it.
#[derive(Clone, Eq, PartialEq)]
pub struct Token(String);

impl Token {
    /// Accepts 32 to 1024 visible ASCII bytes, which fit an `Authorization` header as is.
    pub fn new(text: String) -> Result<Self, &'static str> {
        if !(MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&text.len()) {
            return Err("token must be 32 to 1024 bytes");
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

impl Listen {
    /// Parses `none` or an address such as `127.0.0.1:<port>`.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text == "none" {
            return Ok(Self::None);
        }
        let error = "listen must be an address such as 127.0.0.1:<port>, or \"none\"";
        text.parse().map(Self::Tcp).map_err(|_| error.to_owned())
    }
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
    /// Turn compression: `auto` when absent. `on` migrates an existing store one way.
    pub compression: Option<atticdb_core::CompressionMode>,
    /// Memory budget of the search word index in MiB. 0 turns it off. 512 when absent.
    pub search_index_mib: Option<u64>,
    /// Segment size in MiB for a new store, 16 to 1024. 64 when absent. An existing store
    /// keeps the size it was made with.
    pub segment_mib: Option<u64>,
    /// At least one tenant. Tokens are unique.
    pub tenants: Vec<Tenant>,
}

impl Config {
    /// Opens, checks and parses a configuration file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = read_private(path, "config")?;
        parse(&text).map_err(|message| format!("config {}: {message}", path.display()))
    }

    /// Finds a tenant by name.
    pub fn tenant(&self, name: &str) -> Option<&Tenant> {
        self.tenants.iter().find(|tenant| tenant.name == name)
    }
}

/// Reads a token file with the same checks as the configuration file.
/// Surrounding whitespace, such as a trailing newline, is ignored.
pub fn read_token_file(path: &Path) -> Result<Token, String> {
    let text = read_private(path, "token file")?;
    Token::new(text.trim().to_owned())
        .map_err(|message| format!("token file {}: {message}", path.display()))
}

/// Writes a new mode 0600 configuration with one tenant and a random token, and
/// `segment_mib` when given. Never overwrites an existing file. Returns the token so the
/// caller can print it once.
pub fn init(path: &Path, tenant: &str, segment_mib: Option<u64>) -> Result<Token, String> {
    let fail = |message: String| format!("config {}: {message}", path.display());
    check_tenant_name(tenant).map_err(|message| fail(message.into()))?;
    // Windows ignores the mode, so the new token file gets its folder's access list.
    #[cfg(windows)]
    crate::platform::check_in_profile(path, "config")?;
    let token = crate::random_hex(MIN_TOKEN_BYTES)
        .map_err(|error| fail(crate::platform::no_random(error)))
        .and_then(|hex| Token::new(hex).map_err(|message| fail(message.into())))?;
    let tenants = json!([{"name": tenant, "token": token.expose()}]);
    let mut config = json!({ "tenants": tenants });
    if let Some(mib) = segment_mib {
        config["segment_mib"] = mib.into();
    }
    let mut text = serde_json::to_string_pretty(&config)
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

/// Opens with `O_NONBLOCK`, so a FIFO cannot block the open, then refuses anything but a
/// regular file, checked with `fstat` on the handle. Read or write through that handle.
#[cfg(unix)]
pub fn open_regular(options: &mut OpenOptions, path: &Path) -> std::io::Result<File> {
    let file = options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("must be a regular file"));
    }
    Ok(file)
}

/// Opens once with [`open_regular`] and [`check_private`], then reads the same handle.
fn read_private(path: &Path, what: &str) -> Result<String, String> {
    let fail = |message: String| format!("{what} {}: {message}", path.display());
    let file = open_regular(OpenOptions::new().read(true), path)
        .map_err(|error| fail(format!("cannot open: {error}")))?;
    check_private(&file, path, what)?;
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| fail(format!("cannot read: {error}")))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(fail(format!("larger than {MAX_FILE_BYTES} bytes")));
    }
    Ok(text)
}

/// Checks an open file with `fstat`: a regular file owned by the effective uid, with no
/// group or other bits. The error names the path and the mode, never content.
#[cfg(unix)]
pub fn check_private(file: &File, path: &Path, what: &str) -> Result<(), String> {
    let fail = |message: String| format!("{what} {}: {message}", path.display());
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
    Ok(())
}

/// Opens `path`, then refuses anything but a regular file, checked on the handle. Windows
/// has no FIFO that could block the open.
#[cfg(not(unix))]
pub fn open_regular(options: &mut OpenOptions, path: &Path) -> std::io::Result<File> {
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("must be a regular file"));
    }
    Ok(file)
}

/// Checks that an open file is a regular file inside the user profile. Windows has no uid or
/// mode bits to check. A file in the user's profile folder is private by its inherited access
/// list. So every config, token file and server log must be there, by
/// [`crate::platform::check_in_profile`].
#[cfg(not(unix))]
pub fn check_private(file: &File, path: &Path, what: &str) -> Result<(), String> {
    let fail = |message: String| format!("{what} {}: {message}", path.display());
    let meta = file.metadata().map_err(|error| fail(format!("cannot stat: {error}")))?;
    if !meta.file_type().is_file() {
        return Err(fail("must be a regular file".into()));
    }
    #[cfg(windows)]
    crate::platform::check_in_profile(path, what)?;
    Ok(())
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
            "compression" => {
                use atticdb_core::CompressionMode::{Auto, Off, On};
                let mode = match non_empty(value, "compression")?.as_str() {
                    "auto" => Auto,
                    "on" => On,
                    "off" => Off,
                    _ => return Err("compression must be \"auto\", \"on\" or \"off\"".into()),
                };
                config.compression = Some(mode);
            }
            "search_index_mib" => match value.as_u64().filter(|mib| *mib <= 1 << 20) {
                Some(mib) => config.search_index_mib = Some(mib),
                None => return Err("search_index_mib must be a whole number of MiB".into()),
            },
            "segment_mib" => {
                let range = MIN_SEGMENT_MIB..=MAX_SEGMENT_MIB;
                match value.as_u64().filter(|mib| range.contains(mib)) {
                    Some(mib) => config.segment_mib = Some(mib),
                    None => {
                        return Err(format!(
                            "segment_mib must be a whole number of MiB from {MIN_SEGMENT_MIB} \
                             to {MAX_SEGMENT_MIB}"
                        ))
                    }
                }
            }
            "unix_socket" => config.unix_socket = Some(non_empty(value, "unix_socket")?.into()),
            "unix_socket_group" => {
                config.unix_socket_group = Some(non_empty(value, "unix_socket_group")?)
            }
            "listen" => config.listen = Some(Listen::parse(&non_empty(value, "listen")?)?),
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

    #[test]
    fn debug_never_shows_tokens() {
        let parse = |token: &str| {
            super::parse(&format!(r#"{{"tenants": [{{"name": "a", "token": "{token}"}}]}}"#))
        };
        let secret = "secret-token-0123456789abcdefghijkl";
        assert!(!format!("{:?}", parse(secret).unwrap()).contains(secret), "Debug redacts tokens");
        for (bytes, ok) in [(31, false), (32, true), (1024, true), (1025, false)] {
            let token = "t".repeat(bytes);
            let error = parse(&token).err().unwrap_or_default();
            assert_eq!(error.is_empty(), ok, "{bytes}-byte token: {error}");
            let named = error.contains("32 to 1024 bytes") && !error.contains(&token);
            assert!(ok || named, "{bytes}: the error names the rule, not the token: {error}");
        }
    }

    #[test]
    fn compression_is_auto_on_or_off() {
        use atticdb_core::CompressionMode::{Auto, Off, On};
        let tenants = r#""tenants": [{"name": "a", "token": "tttttttttttttttttttttttttttttttt"}]"#;
        let parse = |field: &str| super::parse(&format!("{{{field}{tenants}}}"));
        assert_eq!(parse("").unwrap().compression, None, "absent means the engine default, auto");
        for (value, mode) in [("auto", Auto), ("on", On), ("off", Off)] {
            let field = format!(r#""compression": "{value}", "#);
            assert_eq!(parse(&field).unwrap().compression, Some(mode), "{value}");
        }
        for value in [r#""yes""#, r#""ON""#, "true", r#""""#] {
            let error = parse(&format!(r#""compression": {value}, "#)).unwrap_err();
            assert!(error.contains("compression"), "{value}: {error}");
        }
        for (value, mib) in [("0", Some(0)), ("64", Some(64))] {
            let config = parse(&format!(r#""search_index_mib": {value}, "#)).unwrap();
            assert_eq!(config.search_index_mib, mib, "{value}");
        }
        for value in ["-1", "1.5", r#""64""#, "1048577"] {
            let error = parse(&format!(r#""search_index_mib": {value}, "#)).unwrap_err();
            assert!(error.contains("search_index_mib"), "{value}: {error}");
        }
    }

    #[test]
    fn segment_mib_range() {
        let tenants = r#""tenants": [{"name": "a", "token": "tttttttttttttttttttttttttttttttt"}]"#;
        let parse = |field: &str| super::parse(&format!("{{{field}{tenants}}}"));
        assert_eq!(parse("").unwrap().segment_mib, None, "absent keeps the 64 MiB default");
        for mib in [16, 64, 1024] {
            let config = parse(&format!(r#""segment_mib": {mib}, "#)).unwrap();
            assert_eq!(config.segment_mib, Some(mib), "{mib}");
        }
        for value in ["15", "1025", "0", r#""16""#, "-1", "16.5", "null"] {
            let error = parse(&format!(r#""segment_mib": {value}, "#)).unwrap_err();
            let named = error.contains("segment_mib") && error.contains("16 to 1024");
            assert!(named, "{value}: the error names the key and the range: {error}");
        }
    }
}
