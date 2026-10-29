//! Hides secrets in what the model writes, before `memory_append` and `fact_set` send it.
//!
//! First come the values of env vars named like secrets, longest first. Then come known token
//! formats, matched by hand. Each secret becomes a `[secret:<type>]` placeholder, which never
//! matches again. It is best effort: a secret that was changed, split or encoded gets through.

use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;

/// How many secrets of each type were hidden, such as `github-token` or `env:GITHUB_TOKEN`.
pub(crate) type Counts = BTreeMap<String, u64>;

/// Env values shorter than this are kept, as short ones match ordinary words.
const MIN_ENV: usize = 12;

/// Env names that hold secrets end in one of these, compared in upper case.
const ENV_ENDS: [&str; 4] = ["_KEY", "_TOKEN", "_SECRET", "_PAT"];

/// The types of the known formats.
const TYPES: [&str; 9] = [
    "github-token",
    "aws-key-id",
    "slack-token",
    "stripe-key",
    "anthropic-key",
    "openai-key",
    "private-key",
    "bearer-token",
    "url-password",
];

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
            text = scan(&text, |bytes, at| {
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

/// Copies `text`, replacing what `find` matches. `find` gets the bytes and a position, and
/// returns where the match ends and its replacement. Placeholders are copied as they are.
fn scan(text: &str, mut find: impl FnMut(&[u8], usize) -> Option<(usize, String)>) -> String {
    let bytes = text.as_bytes();
    let (mut out, mut at) = (Vec::with_capacity(bytes.len()), 0);
    while at < bytes.len() {
        if let Some(length) = placeholder(&bytes[at..]) {
            out.extend_from_slice(&bytes[at..at + length]);
            at += length;
        } else if let Some((end, with)) = find(bytes, at) {
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

/// The length of the placeholder `rest` starts with: a known type, or `env:` and a name of
/// `[A-Za-z0-9_]`.
fn placeholder(rest: &[u8]) -> Option<usize> {
    let inner = rest.strip_prefix(b"[secret:")?;
    let close = inner.iter().position(|byte| *byte == b']')?;
    let kind = &inner[..close];
    let env = kind
        .strip_prefix(b"env:")
        .filter(|name| !name.is_empty() && name.iter().all(|byte| alnum(*byte) || *byte == b'_'));
    let known = TYPES.iter().any(|known| known.as_bytes() == kind);
    (known || env.is_some()).then_some(b"[secret:".len() + close + 1)
}

/// `text` with the known formats hidden.
fn formats(text: &str, counts: &mut Counts) -> String {
    scan(text, |bytes, at| {
        let (end, kind, with) = format_at(bytes, at, counts)?;
        *counts.entry(kind.into()).or_default() += 1;
        Some((end, with))
    })
}

/// The format at `at`: where it ends, its type and its replacement.
fn format_at(
    bytes: &[u8],
    at: usize,
    counts: &mut Counts,
) -> Option<(usize, &'static str, String)> {
    let rest = &bytes[at..];
    if rest.starts_with(b"://") {
        if let Some((end, with)) = url_password(bytes, at + 3, counts) {
            return Some((end, "url-password", with));
        }
    }
    if at > 0 && word(bytes[at - 1]) {
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
    if let Some(length) = private_key(rest) {
        return Some(placed(length, "private-key"));
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

/// A `-----BEGIN ` line holding `PRIVATE KEY-----`, through the next `-----END ` line that
/// holds it too, or to the end. Only the first [`KEY_LINE`] bytes of those lines are read.
fn private_key(rest: &[u8]) -> Option<usize> {
    let after = rest.strip_prefix(BEGIN)?;
    find(&after[..line(after)], PRIVATE_KEY)?;
    let mut from = BEGIN.len() + find(after, b"\n").unwrap_or(after.len());
    while let Some(found) = find(&rest[from..], END) {
        let start = from + found;
        if let Some(key) = find(&rest[start..start + line(&rest[start..])], PRIVATE_KEY) {
            return Some(start + key + PRIVATE_KEY.len());
        }
        from = start + END.len();
    }
    Some(rest.len())
}

/// A key's BEGIN and END lines are short. Reading at most this much of each keeps a long
/// text full of `-----BEGIN ` linear.
const KEY_LINE: usize = 200;

/// The length of the line `text` starts with, up to [`KEY_LINE`] bytes.
fn line(text: &[u8]) -> usize {
    let text = &text[..text.len().min(KEY_LINE)];
    find(text, b"\n").unwrap_or(text.len())
}

/// The userinfo that starts at `start`, after `://`, when it holds a password: `<user>:` and
/// at least 1 byte, then `@`, with no `/`, `?`, `#` or whitespace before it. Returns where the
/// `@` ends and the replacement from `://`. The user part is checked for formats too.
fn url_password(bytes: &[u8], start: usize, counts: &mut Counts) -> Option<(usize, String)> {
    let stop = |byte: &u8| b"@/?#".contains(byte) || byte.is_ascii_whitespace();
    let length = bytes[start..].iter().position(stop)?;
    let userinfo = &bytes[start..start + length];
    if bytes[start + length] != b'@' {
        return None;
    }
    // The first `:` outside a placeholder.
    let mut colon = 0;
    while colon < userinfo.len() && userinfo[colon] != b':' {
        colon += placeholder(&userinfo[colon..]).unwrap_or(1);
    }
    let password = userinfo.get(colon + 1..).filter(|password| !password.is_empty())?;
    if placeholder(password) == Some(password.len()) {
        return None;
    }
    // Both ends are ASCII bytes, so the user part is whole UTF-8.
    let user = String::from_utf8_lossy(&userinfo[..colon]);
    let user = formats(&user, counts);
    Some((start + length + 1, format!("://{user}:[secret:url-password]@")))
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

    /// Every format with its near misses, env values, and placeholders, which stay. Tokens are
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
                "https://[secret:github-token]:[secret:url-password]@host/x",
                "github-token=1 url-password=1",
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
                "https://bob:[secret:env:MY_TOKEN]@host/x",
                "env:MY_TOKEN=1",
            ),
            ("placeholders", "[secret:github-token] [secret:env:X_KEY]".into(), "", ""),
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
