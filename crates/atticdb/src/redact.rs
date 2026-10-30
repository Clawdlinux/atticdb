//! Hides secrets in what the model writes, before `memory_append` and `fact_set` send it.
//!
//! First come the values of env vars named like secrets, longest first. Then come known token
//! formats, matched by hand. Each secret becomes a `[secret:<type>]` placeholder, which never
//! matches again. Text shaped like a placeholder is scanned like any other, so it can't hide a
//! secret. It is best effort: a secret that was changed, split or encoded gets through.

use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;

/// How many secrets of each type were hidden, such as `github-token` or `env:GITHUB_TOKEN`.
pub(crate) type Counts = BTreeMap<String, u64>;

/// Env values shorter than this are kept, as short ones match ordinary words.
const MIN_ENV: usize = 12;

/// Env names that hold secrets end in one of these, compared in upper case.
const ENV_ENDS: [&str; 4] = ["_KEY", "_TOKEN", "_SECRET", "_PAT"];

/// Prefixes, the bytes that may follow, the fewest of them, and the type. The first match
/// wins, so `sk-ant-` comes before `sk-`.
type Token = (&'static [&'static str], fn(u8) -> bool, usize, &'static str);
const TOKENS: [Token; 6] = [
    (&["ghp_", "gho_", "ghu_", "ghs_", "ghr_"], alnum, 36, "github-token"),
    (&["github_pat_"], |b| alnum(b) || b == b'_', 22, "github-token"),
    (&["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"], |b| alnum(b) || b == b'-', 10, "slack-token"),
    (&["sk_live_", "rk_live_"], alnum, 16, "stripe-key"),
    (&["sk-ant-"], word, 20, "anthropic-key"),
    (&["sk-"], word, 32, "openai-key"),
];

const BEGIN: &[u8] = b"-----BEGIN ";
const END: &[u8] = b"-----END ";
const PRIVATE_KEY: &[u8] = b"PRIVATE KEY-----";
const BEARER: &[u8] = b"bearer ";

/// The secrets to look for.
pub(crate) struct Redactor {
    /// Env names and values, longest value first.
    known: Vec<(String, String)>,
}

impl Redactor {
    /// Reads this process's environment.
    pub(crate) fn from_env() -> Self {
        Self::new(std::env::vars_os())
    }

    /// Keeps the vars whose names end in [`ENV_ENDS`] or contain `PASSWORD`, in any case, with
    /// UTF-8 values of [`MIN_ENV`] bytes or more.
    fn new(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        let mut known: Vec<(String, String)> = vars
            .into_iter()
            .filter_map(|(name, value)| {
                let (name, value) = (name.into_string().ok()?, value.into_string().ok()?);
                let upper = name.to_ascii_uppercase();
                let named = ENV_ENDS.iter().any(|end| upper.ends_with(end));
                let secret = named || upper.contains("PASSWORD");
                (secret && value.len() >= MIN_ENV).then_some((name, value))
            })
            .collect();
        known.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        Self { known }
    }

    /// `text` with its secrets hidden. Adds what it hid to `counts`.
    pub(crate) fn text(&self, text: &str, counts: &mut Counts) -> String {
        let mut text = text.to_owned();
        for (name, value) in &self.known {
            let kind = format!("env:{name}");
            let with = format!("[secret:{kind}]");
            text = scan(&text, |bytes, at, _| {
                bytes[at..].starts_with(value.as_bytes()).then(|| {
                    *counts.entry(kind.clone()).or_default() += 1;
                    (at + value.len(), with.clone())
                })
            });
        }
        formats(&text, counts)
    }

    /// Hides secrets in every string in `value`, nested ones included. Keys are kept.
    pub(crate) fn json(&self, value: &mut Value, counts: &mut Counts) {
        match value {
            Value::String(text) => *text = self.text(text, counts),
            Value::Array(items) => items.iter_mut().for_each(|item| self.json(item, counts)),
            Value::Object(fields) => fields.values_mut().for_each(|field| self.json(field, counts)),
            _ => {}
        }
    }
}

fn alnum(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

/// `[A-Za-z0-9_-]`. A format must start the text or follow a byte outside it.
fn word(byte: u8) -> bool {
    alnum(byte) || byte == b'_' || byte == b'-'
}

/// Copies `text`, replacing what `find` matches. `find` gets the bytes, a position and the
/// last byte written, and returns where the match ends and its replacement. A format's
/// boundary is checked on the byte written, so a second run sees the same boundaries.
fn scan(
    text: &str,
    mut find: impl FnMut(&[u8], usize, Option<u8>) -> Option<(usize, String)>,
) -> String {
    let bytes = text.as_bytes();
    let (mut out, mut at) = (Vec::with_capacity(bytes.len()), 0);
    while at < bytes.len() {
        if let Some((end, with)) = find(bytes, at, out.last().copied()) {
            out.extend_from_slice(with.as_bytes());
            at = end;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    // Every match starts and ends at an ASCII byte or a whole UTF-8 value, so this holds.
    String::from_utf8(out).unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into())
}

/// `text` with the known formats hidden.
fn formats(text: &str, counts: &mut Counts) -> String {
    let mut line = KeyLine::default();
    scan(text, |bytes, at, before| {
        let (end, kind, with) = format_at(bytes, at, before, &mut line)?;
        *counts.entry(kind.into()).or_default() += 1;
        Some((end, with))
    })
}

/// The format at `at`, after the byte `before`: where it ends, its type and its replacement.
fn format_at(
    bytes: &[u8],
    at: usize,
    before: Option<u8>,
    line: &mut KeyLine,
) -> Option<(usize, &'static str, String)> {
    let rest = &bytes[at..];
    if rest.starts_with(b"://") {
        if let Some((end, with)) = url_password(bytes, at + 3) {
            return Some((end, "url-password", with));
        }
    }
    if before.is_some_and(word) {
        return None;
    }
    let placed =
        |length: usize, kind: &'static str| (at + length, kind, format!("[secret:{kind}]"));
    for (prefixes, class, min, kind) in TOKENS {
        let Some(prefix) = prefixes.iter().find(|prefix| rest.starts_with(prefix.as_bytes()))
        else {
            continue;
        };
        let body = &rest[prefix.len()..];
        let length = body.iter().take_while(|byte| class(**byte)).count();
        let digit = kind != "openai-key" || body[..length].iter().any(u8::is_ascii_digit);
        if length >= min && digit {
            return Some(placed(prefix.len() + length, kind));
        }
    }
    if let Some(length) = aws_key_id(rest) {
        return Some(placed(length, "aws-key-id"));
    }
    if let Some(end) = private_key(bytes, at, line) {
        return Some((end, "private-key", "[secret:private-key]".into()));
    }
    let bearer = rest.get(..BEARER.len()).filter(|word| word.eq_ignore_ascii_case(BEARER))?;
    let token = |byte: &u8| alnum(*byte) || b"._~+/=-".contains(byte);
    let length = rest[BEARER.len()..].iter().take_while(|byte| token(byte)).count();
    // The word is ASCII, as it matched `bearer `.
    let with = format!("{}[secret:bearer-token]", String::from_utf8_lossy(bearer));
    (length >= 20).then_some((at + BEARER.len() + length, "bearer-token", with))
}

/// `AKIA` or `ASIA`, then exactly 16 `[A-Z0-9]`, and no `[A-Za-z0-9]` next.
fn aws_key_id(rest: &[u8]) -> Option<usize> {
    if !(rest.starts_with(b"AKIA") || rest.starts_with(b"ASIA")) {
        return None;
    }
    let body = rest.get(4..20)?;
    let upper = body.iter().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
    (upper && !rest.get(20).is_some_and(|byte| alnum(*byte))).then_some(20)
}

/// The end of the line a `-----BEGIN ` was last seen on, and where `PRIVATE KEY-----` last
/// starts on it. Many `-----BEGIN ` on 1 line then read it once, so the scan stays linear.
#[derive(Default)]
struct KeyLine {
    end: usize,
    key: Option<usize>,
}

/// A `-----BEGIN ` at `at` with `PRIVATE KEY-----` after it on its line, through the next
/// `-----END ` line that holds `PRIVATE KEY-----` after it, or to the end. Returns the end.
fn private_key(bytes: &[u8], at: usize, line: &mut KeyLine) -> Option<usize> {
    if !bytes[at..].starts_with(BEGIN) {
        return None;
    }
    if at >= line.end {
        let end = at + find(&bytes[at..], b"\n").unwrap_or(bytes.len() - at);
        let key = bytes[at..end].windows(PRIVATE_KEY.len()).rposition(|w| w == PRIVATE_KEY);
        *line = KeyLine { end, key: key.map(|key| at + key) };
    }
    line.key.filter(|key| *key >= at + BEGIN.len())?;
    // Line by line. Only the first `-----END ` on a line matters: a key after a later one is
    // after the first too.
    let mut start = line.end;
    while start < bytes.len() {
        let end = start + find(&bytes[start..], b"\n").unwrap_or(bytes.len() - start);
        if let Some(found) = find(&bytes[start..end], END) {
            let after = start + found + END.len();
            if let Some(key) = find(&bytes[after..end], PRIVATE_KEY) {
                return Some(after + key + PRIVATE_KEY.len());
            }
        }
        start = end + 1;
    }
    Some(bytes.len())
}

/// The URL password after the `://` that ends at `start`, as WHATWG splits it. The authority
/// ends at the first `/`, `?`, `#` or whitespace. The userinfo is what comes before its last
/// `@`. Formats in it are hidden first, then the password follows its first `:`. A colon
/// inside a placeholder counts too, so a second run splits the same way. Returns where the
/// `@` ends and the replacement from `://`.
fn url_password(bytes: &[u8], start: usize) -> Option<(usize, String)> {
    let stop = |byte: &u8| b"/?#".contains(byte) || byte.is_ascii_whitespace();
    let authority = bytes[start..].iter().position(stop).unwrap_or(bytes.len() - start);
    let at = bytes[start..start + authority].iter().rposition(|byte| *byte == b'@')?;
    // Both ends are ASCII bytes, so the userinfo is whole UTF-8.
    let userinfo = formats(&String::from_utf8_lossy(&bytes[start..start + at]), &mut Counts::new());
    let colon = userinfo.find(':')?;
    let password = &userinfo[colon + 1..];
    // Already hidden, as this writes it.
    if password.is_empty() || password == "[secret:url-password]" {
        return None;
    }
    // The user part holds no `:`, so no whole placeholder. The start of one goes too.
    let user = &userinfo[..colon];
    let user = user.strip_suffix("[secret").unwrap_or(user);
    Some((start + at + 1, format!("://{user}:[secret:url-password]@")))
}

/// Where `needle` first appears in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{Counts, Redactor};
    use serde_json::json;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    /// `type=n` pairs, space separated.
    fn counts(text: &str) -> Counts {
        let pairs = text.split_whitespace().filter_map(|pair| pair.rsplit_once('='));
        pairs.map(|(kind, n)| (kind.to_owned(), n.parse().unwrap_or(0))).collect()
    }

    /// Every format with its near misses, env values, and placeholders, which stay unless they
    /// hold a secret. Tokens are
    /// built here, as a token-shaped literal would trip push protection.
    #[test]
    fn redact_hides_known_secrets_once() {
        let vars = [
            ("MY_TOKEN", "tok-12345678"),
            ("SHORT_KEY", "tok-1234567"),
            ("LONG_SECRET", "5678-abcdefgh"),
            ("db_password", "correct-horse"),
            ("PLAIN_NAME", "plain-value-1"),
        ];
        let mut vars: Vec<(OsString, OsString)> =
            vars.iter().map(|(name, value)| (name.into(), value.into())).collect();
        let pad = "p".repeat(64);
        vars.push(("PAD_KEY".into(), pad.as_str().into()));
        vars.push(("BAD_TOKEN".into(), OsString::from_vec(vec![0xff; 16])));
        let redactor = Redactor::new(vars);

        let github = format!("ghp_{}", "x1".repeat(18));
        let aws = format!("AKIA{}", "AB12".repeat(4));
        let slack = format!("xoxb-{}", "12345-abcd");
        let openai = "ab_-".repeat(7) + "1234";
        let key = |kind: &str| format!("-----{kind} RSA {}-----", "PRIVATE KEY");
        let bearer = "a.b-c~d+e/f=".repeat(2);
        let cert = format!("-----BEGIN {0}-----\nabc\n-----END {0}-----", "CERTIFICATE");
        let url = |user: &str, password: &str| format!("https://{user}:{password}@host/x");

        // An empty expected text means the input stays as it is.
        let rows = [
            ("github", format!("use {github}."), "use [secret:github-token].", "github-token=1"),
            ("github, 34", format!("ghp_{}", "x1".repeat(17)), "", ""),
            ("github in a word", format!("my{github} x-{github} x_{github}"), "", ""),
            (
                "fine-grained",
                format!("(github_pat_{})", "a1_".repeat(8)),
                "([secret:github-token])",
                "github-token=1",
            ),
            ("fine-grained, 21", format!("github_pat_{}", "a".repeat(21)), "", ""),
            ("aws", format!("id={aws}_x"), "id=[secret:aws-key-id]_x", "aws-key-id=1"),
            ("aws, 17", format!("{aws}Z"), "", ""),
            ("aws, lower case next", format!("{aws}z"), "", ""),
            ("slack", slack.clone(), "[secret:slack-token]", "slack-token=1"),
            ("slack, 9", format!("xoxb-{}", "123456789"), "", ""),
            (
                "stripe",
                format!("sk_live_{}", "a1".repeat(8)),
                "[secret:stripe-key]",
                "stripe-key=1",
            ),
            ("stripe, 15", format!("sk_live_{}", "a".repeat(15)), "", ""),
            (
                "anthropic",
                format!("sk-ant-{}", "a1-_".repeat(5)),
                "[secret:anthropic-key]",
                "anthropic-key=1",
            ),
            ("anthropic, 19", format!("sk-ant-{}", "a".repeat(19)), "", ""),
            ("openai", format!("k: sk-{openai}"), "k: [secret:openai-key]", "openai-key=1"),
            ("openai, no digit", format!("sk-{}", "ab".repeat(16)), "", ""),
            ("openai, 31", format!("sk-{}", &openai[1..]), "", ""),
            ("risk-", format!("risk-{openai}"), "", ""),
            ("task-", format!("task-{openai}"), "", ""),
            (
                "private key",
                format!("a\n{}\nMIIabc\n{}\nb", key("BEGIN"), key("END")),
                "a\n[secret:private-key]\nb",
                "private-key=1",
            ),
            (
                "private key, no END line",
                format!("a\n{}\nMIIabc\n", key("BEGIN")),
                "a\n[secret:private-key]",
                "private-key=1",
            ),
            ("certificate", cert, "", ""),
            (
                "key words on the next line",
                format!("-----BEGIN RSA\n{}-----", "PRIVATE KEY"),
                "",
                "",
            ),
            (
                "bearer",
                format!("Authorization: Bearer {bearer}"),
                "Authorization: Bearer [secret:bearer-token]",
                "bearer-token=1",
            ),
            (
                "bearer, any case",
                format!("bEARER {bearer}"),
                "bEARER [secret:bearer-token]",
                "bearer-token=1",
            ),
            ("bearer, 19", format!("Bearer {}", &bearer[..19]), "", ""),
            (
                "url",
                url("bob", "pw123"),
                "https://bob:[secret:url-password]@host/x",
                "url-password=1",
            ),
            ("url, port", "https://host:8080/path a@b".into(), "", ""),
            ("url, no password", "https://bob@host https://bob:@host".into(), "", ""),
            (
                "url, token as user",
                url(&github, "x"),
                "https://:[secret:url-password]@host/x",
                "url-password=1",
            ),
            ("env", "a tok-12345678 b".into(), "a [secret:env:MY_TOKEN] b", "env:MY_TOKEN=1"),
            ("env, 11 bytes", "tok-1234567 ".into(), "", ""),
            (
                "env, overlapping, longer first",
                "tok-12345678-abcdefgh".into(),
                "tok-1234[secret:env:LONG_SECRET]",
                "env:LONG_SECRET=1",
            ),
            (
                "env, lower case name",
                "correct-horse x2 correct-horse".into(),
                "[secret:env:db_password] x2 [secret:env:db_password]",
                "env:db_password=2",
            ),
            ("env, other name", "plain-value-1".into(), "", ""),
            (
                "env in a url",
                url("bob", "tok-12345678"),
                "https://bob:[secret:url-password]@host/x",
                "env:MY_TOKEN=1 url-password=1",
            ),
            (
                "url, password before a placeholder",
                format!("https://alice:{}:[secret:url-password]@host/x", "cleartext-password"),
                "https://alice:[secret:url-password]@host/x",
                "url-password=1",
            ),
            (
                "url, @ in the password",
                format!("postgres://admin:{}@db:5432/x", "P@ssw0rd"),
                "postgres://admin:[secret:url-password]@db:5432/x",
                "url-password=1",
            ),
            ("url, empty password", "https://user:@host".into(), "", ""),
            (
                "url, placeholder then password",
                format!("https://[secret:github-token]{}@host/x", "cleartext-password"),
                "https://:[secret:url-password]@host/x",
                "url-password=1",
            ),
            (
                "url, token then more as user",
                url(&format!("{github}_abc"), "pw"),
                "https://:[secret:url-password]@host/x",
                "url-password=1",
            ),
            (
                "url, token as user and no password",
                format!("https://{github}_abc@host/x"),
                "https://:[secret:url-password]@host/x",
                "url-password=1",
            ),
            (
                "a format right after a hidden one",
                format!("{aws}{}\nabc", key("BEGIN")),
                "[secret:aws-key-id][secret:private-key]",
                "aws-key-id=1 private-key=1",
            ),
            (
                "private key, env value on a long BEGIN line",
                format!("-----BEGIN {} {pad} {}-----\nMIIabc\n", "x".repeat(150), "PRIVATE KEY"),
                "[secret:private-key]",
                "env:PAD_KEY=1 private-key=1",
            ),
            (
                "private key, token on a long BEGIN line",
                format!("-----BEGIN {} {github} {}-----\nMIIabc", "x".repeat(150), "PRIVATE KEY"),
                "[secret:private-key]",
                "private-key=1",
            ),
            ("placeholders", "[secret:github-token] [secret:env:X_KEY]".into(), "", ""),
            (
                "token in a placeholder",
                format!("[secret:env:{github}]"),
                "[secret:env:[secret:github-token]]",
                "github-token=1",
            ),
            (
                "aws in a placeholder",
                format!("[secret:env:{aws}]"),
                "[secret:env:[secret:aws-key-id]]",
                "aws-key-id=1",
            ),
            (
                "env in a placeholder",
                format!("[secret:env:{}]", "tok-12345678"),
                "[secret:env:[secret:env:MY_TOKEN]]",
                "env:MY_TOKEN=1",
            ),
            (
                "token in a placeholder in a url",
                format!("x://[secret:env:{github}]:[secret:url-password]@h"),
                "x://:[secret:url-password]@h",
                "url-password=1",
            ),
        ];
        for (case, input, expected, counted) in rows {
            let expected = if expected.is_empty() { input.as_str() } else { expected };
            let mut found = Counts::new();
            let output = redactor.text(&input, &mut found);
            assert_eq!(output, expected, "{case}");
            assert_eq!(found, counts(counted), "{case}");
            let mut again = Counts::new();
            assert_eq!(redactor.text(&output, &mut again), output, "{case}: twice");
            assert!(again.is_empty(), "{case}: placeholders never match again");
        }

        // Every string in meta, nested ones included. Keys are kept.
        let mut meta =
            json!({"note": github.clone(), "n": 1, "list": [aws], (github.clone()): {"x": slack}});
        let mut found = Counts::new();
        redactor.json(&mut meta, &mut found);
        let expected = json!({"note": "[secret:github-token]", "n": 1,
            "list": ["[secret:aws-key-id]"], (github): {"x": "[secret:slack-token]"}});
        assert_eq!(meta, expected);
        assert_eq!(found, counts("aws-key-id=1 github-token=1 slack-token=1"), "meta");
    }
}
