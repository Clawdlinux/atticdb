//! `atticdb studio`: a read-only web page on loopback. A small HTTP bridge serves the page and
//! forwards an allowlist of read routes to the store. The tenant token stays in this process.
//!
//! Each run binds a random port on 127.0.0.1 and makes a 32-hex secret. The page reads the
//! secret from the URL fragment, which browsers never send, and returns it in the
//! `X-Atticdb-Studio` header. A page on another origin cannot send that header without a CORS
//! answer, and there is none. The exact `Host` check blocks DNS rebinding.

use crate::api;
use crate::client::{Store, StoreFlags, StoreKind};
use crate::config::Token;
use crate::http::{self, ClientResponse, Head, Limits, Response, ServerConn};
use crate::launch::Launch;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::hint::black_box;
use std::io::{self, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Most open connections. Past it a new one gets 503 `busy`.
const MAX_CONNECTIONS: usize = 16;
/// Time for a whole request head, from accept.
const HEAD_TIMEOUT: Duration = Duration::from_secs(5);
/// Time for 1 store read, connect included, and for 1 block.
const DEADLINE: Duration = Duration::from_secs(10);

/// On every response, the connection's own errors included.
const HEADERS: &[(&str, &str)] = &[
    (
        "Content-Security-Policy",
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
         connect-src 'self'; worker-src 'none'; base-uri 'none'; form-action 'none'; \
         frame-ancestors 'none'",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("Cache-Control", "no-store"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
];

/// The page files, served without the secret. They hold no data.
const PAGES: [(&str, &str, &str); 3] = [
    ("/", "text/html; charset=utf-8", include_str!("../studio/index.html")),
    ("/app.js", "text/javascript; charset=utf-8", include_str!("../studio/app.js")),
    ("/app.css", "text/css; charset=utf-8", include_str!("../studio/app.css")),
];

/// Read routes forwarded to the store, at the same path without `/api`. Never a write route.
const READS: [&str; 9] = [
    "/api/v1/sessions",
    "/api/v1/records",
    "/api/v1/window",
    "/api/v1/search",
    "/api/v1/facts/list",
    "/api/v1/facts/get",
    "/api/v1/facts/history",
    "/api/v1/stats",
    "/api/metrics",
];

/// Where a request that passed the checks goes.
enum Route {
    /// A page file: its content type and text.
    Page(&'static str, &'static str),
    /// A store read, by its path in [`READS`].
    Read(&'static str),
    /// `/api/block`.
    Block,
    /// `/api/info`.
    Info,
}

struct Studio {
    /// Held only while connecting, which may start the home server.
    launch: Mutex<Launch>,
    token: Token,
    kind: StoreKind,
    tenant: Option<String>,
    home: Option<PathBuf>,
    secret: String,
    /// `127.0.0.1:<port>`, the only `Host` accepted.
    host: String,
    /// `http://127.0.0.1:<port>`, the only `Origin` accepted.
    origin: String,
}

/// Runs `atticdb studio` until it is killed. Returns exit code 1 when it cannot start.
pub fn start(mut flags: HashMap<String, String>) -> u8 {
    let open = flags.remove("--no-open").is_none();
    let Err(message) = run(flags, open);
    let _ = writeln!(io::stderr(), "atticdb: {message}");
    1
}

fn run(mut flags: HashMap<String, String>, open: bool) -> Result<Infallible, String> {
    let Store { launch, token, kind, tenant, home } = StoreFlags::take(&mut flags)?.open(true)?;
    let launch = Mutex::new(launch);
    probe(&launch)?;
    let listener = crate::listen::bind_tcp(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
    let listener = listener.map_err(|error| error.to_string())?;
    let port = listener.local_addr().map_err(|error| error.to_string())?.port();
    let secret = crate::random_hex(16);
    let secret = secret.map_err(crate::platform::no_random)?;
    let url = format!("http://127.0.0.1:{port}/#k={secret}");
    // 1 write, so a reader never sees half of a line.
    let mut stdout = io::stdout();
    let lines = format!("atticdb studio: {url}\nPress Ctrl-C to stop.\n");
    let _ = stdout.write_all(lines.as_bytes()).and_then(|()| stdout.flush());
    if open {
        if let Err(error) = open_page(&url) {
            not_opened(&error);
        }
    }
    let (host, origin) = (format!("127.0.0.1:{port}"), format!("http://127.0.0.1:{port}"));
    serve(listener, Arc::new(Studio { launch, token, kind, tenant, home, secret, host, origin }))
}

/// `GET /healthz` once, without the token. Fails unless the store answers 200.
fn probe(launch: &Mutex<Launch>) -> Result<(), String> {
    let reply = get(launch, "/healthz", None, Instant::now() + DEADLINE);
    let reply = reply.map_err(|error| format!("cannot reach the store: {error}"))?;
    if reply.status == 200 {
        return Ok(());
    }
    let health: Value = serde_json::from_slice(&reply.body).unwrap_or_default();
    let status = health["status"].as_str().map(|text| format!(" {text:?}")).unwrap_or_default();
    Err(format!("the store answered /healthz with {}{status}", reply.status))
}

/// Sends 1 `GET` to the store by `deadline`, connect included.
fn get(
    launch: &Mutex<Launch>,
    target: &str,
    token: Option<&Token>,
    deadline: Instant,
) -> io::Result<ClientResponse> {
    // The lock is held only while connecting. The exchange runs outside it.
    let (mut stream, host) = {
        let mut launch = launch.lock().unwrap_or_else(PoisonError::into_inner);
        launch.connect(deadline)?
    };
    let auth = token.map(|token| format!("Bearer {}", token.expose()));
    let mut headers = vec![("Host", host.as_str())];
    headers.extend(auth.as_deref().map(|auth| ("Authorization", auth)));
    http::exchange(&mut *stream, "GET", target, &headers, &[], deadline, api::MAX_RESPONSE_BYTES)
}

/// Writes a 0600 redirect page to `url` in the home directory and opens it in the browser.
/// The secret is never in a process argument, where `ps` would show it.
#[cfg(unix)]
fn open_page(url: &str) -> Result<(), String> {
    let dir = crate::home::dir()?;
    crate::listen::owned_dir(&dir, 0o077)?;
    let page = dir.join("studio.html");
    let hex = crate::random_hex(8).map_err(crate::platform::no_random)?;
    let tmp = dir.join(format!(".studio.html.{hex}"));
    let html = format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>AtticDB studio</title>\n\
         <meta http-equiv=\"refresh\" content=\"0;url={url}\">\n\
         <a href=\"{url}\">Open AtticDB studio</a>\n"
    );
    let mut options = std::fs::OpenOptions::new();
    let written = options.write(true).create_new(true).mode(0o600).open(&tmp);
    let written = written.and_then(|mut file| file.write_all(html.as_bytes()));
    if let Err(error) = written.and_then(|()| std::fs::rename(&tmp, &page)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {error}", page.display()));
    }
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let mut command = Command::new(program);
    command
        .arg(&page)
        // The browser it starts gets this environment, and a token there may be live.
        .env_remove(crate::client::TOKEN_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group, so Ctrl-C here does not stop a browser it starts.
        .process_group(0);
    // Tests put a fake opener on PATH, which may still be busy from its write.
    let mut child = crate::spawn(&mut command).map_err(|error| format!("{program}: {error}"))?;
    // Snap browsers, Ubuntu's default, cannot read hidden folders such as ~/.local/share.
    // xdg-open still exits 0, so only the browser shows the error.
    if cfg!(target_os = "linux") {
        let hint = "atticdb studio: if the browser shows an error, open the link above.";
        let _ = writeln!(io::stderr(), "{hint}");
    }
    let reap = move || match child.wait() {
        Ok(status) if !status.success() => not_opened(&format!("{program} exited: {status}")),
        _ => {}
    };
    let _ = std::thread::Builder::new().name("atticdb-open".into()).spawn(reap);
    Ok(())
}

/// Writes the redirect page as on Unix, then opens it with `explorer.exe`. No shell parses
/// the path, so a `&` or `^` in it stays literal. `cmd /C start` would parse them.
#[cfg(windows)]
fn open_page(url: &str) -> Result<(), String> {
    let dir = crate::home::dir()?;
    crate::listen::owned_dir(&dir, 0o077)?;
    let page = dir.join("studio.html");
    let hex = crate::random_hex(8).map_err(crate::platform::no_random)?;
    let tmp = dir.join(format!(".studio.html.{hex}"));
    let html = format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>AtticDB studio</title>\n\
         <meta http-equiv=\"refresh\" content=\"0;url={url}\">\n\
         <a href=\"{url}\">Open AtticDB studio</a>\n"
    );
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp);
    let written = written.and_then(|mut file| file.write_all(html.as_bytes()));
    if let Err(error) = written.and_then(|()| std::fs::rename(&tmp, &page)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {error}", page.display()));
    }
    let mut command = Command::new(crate::platform::system_program("explorer.exe"));
    command
        .arg(&page)
        // The browser it starts gets this environment, and a token there may be live.
        .env_remove(crate::client::TOKEN_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::no_window(&mut command);
    let mut child = crate::spawn(&mut command).map_err(|error| format!("explorer: {error}"))?;
    // explorer.exe exits 1 even when it opened the page, so its status tells nothing.
    let reap = move || drop(child.wait());
    let _ = std::thread::Builder::new().name("atticdb-open".into()).spawn(reap);
    Ok(())
}

fn not_opened(why: &str) {
    let _ = writeln!(io::stderr(), "atticdb studio: cannot open a browser ({why}). Open the link.");
}

/// Accepts connections until the process is killed, 1 thread each.
fn serve(listener: TcpListener, studio: Arc<Studio>) -> ! {
    let open = Arc::new(AtomicUsize::new(0));
    loop {
        let Ok((stream, _)) = listener.accept() else {
            // Such as running out of file descriptors. Back off instead of spinning.
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        // Only this thread adds, so the count never passes the cap.
        if open.load(Ordering::Acquire) >= MAX_CONNECTIONS {
            busy(stream);
            continue;
        }
        // A response is written as a head and a body, so do not hold the body back.
        let _ = stream.set_nodelay(true);
        open.fetch_add(1, Ordering::AcqRel);
        let (slot, studio) = (Slot(open.clone()), studio.clone());
        // When the spawn fails, the slot and the stream are dropped with the closure.
        let _ = std::thread::Builder::new().name("atticdb-studio".into()).spawn(move || {
            let _slot = slot;
            studio.connection(stream);
        });
    }
}

/// An open connection, counted until dropped.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Answers 503 `busy` on accept without reading, and closes without lingering.
fn busy(mut stream: TcpStream) {
    let deadline = Instant::now() + Duration::from_secs(1);
    let _ = http::write_response(&mut stream, &error(503, "busy"), true, deadline);
    let _ = stream.shutdown(Shutdown::Both);
}

impl Studio {
    /// Serves 1 request, then closes. The head must arrive within 5 s, and bodies are refused.
    fn connection(&self, stream: TcpStream) {
        let limits = Limits {
            head_timeout: HEAD_TIMEOUT,
            body_bytes: 0,
            max_requests: 1,
            headers: HEADERS,
            ..Limits::default()
        };
        let mut conn = ServerConn::new(stream, limits);
        let Some((request, route)) = conn.next_request(|head| self.check(head)) else { return };
        let started = Instant::now();
        let response = match route {
            Route::Page(content_type, text) => answer(200, content_type, text.as_bytes().to_vec()),
            Route::Info => json(200, &self.info()),
            Route::Block => self.block(&request.head.target),
            Route::Read(path) => self.read(path, &request.head.target),
        };
        let _ = conn.respond(&response);
        // Only allowlisted paths get here, so the line holds no query and no secret.
        if response.status >= 500 {
            let (method, path) = (&request.head.method, request.head.path());
            let (status, ms) = (response.status, started.elapsed().as_secs_f64() * 1000.0);
            let _ = writeln!(io::stderr(), "atticdb studio: {method} {path} {status} {ms:.3}ms");
        }
    }

    /// In order: `Host`, `Origin`, the method, then the secret for `/api/`. Returns the route,
    /// or the error to answer.
    fn check(&self, head: &Head) -> Result<Route, Response> {
        let origin = head.headers("origin").all(|origin| origin == self.origin.as_bytes());
        if head.header("host") != Some(self.host.as_bytes()) || !origin {
            return Err(error(403, "forbidden"));
        }
        if head.method != "GET" {
            return Err(error(405, "method_not_allowed"));
        }
        let path = head.path();
        if let Some(&(_, content_type, text)) = PAGES.iter().find(|(page, ..)| *page == path) {
            return Ok(Route::Page(content_type, text));
        }
        if !path.starts_with("/api/") {
            return Err(error(404, "not_found"));
        }
        if !self.authorized(head) {
            return Err(error(401, "unauthorized"));
        }
        match path {
            "/api/info" => Ok(Route::Info),
            "/api/block" => Ok(Route::Block),
            _ => READS.into_iter().find(|read| *read == path).map(Route::Read).ok_or_else(|| {
                // `/api/v1/append` and every other write end here.
                error(404, "not_found")
            }),
        }
    }

    /// Whether exactly 1 `X-Atticdb-Studio` header holds the secret. After an exact length
    /// check, all 32 bytes are compared in constant time.
    fn authorized(&self, head: &Head) -> bool {
        let mut values = head.headers("x-atticdb-studio");
        let (Some(value), None) = (values.next(), values.next()) else { return false };
        let secret = self.secret.as_bytes();
        if value.len() != secret.len() {
            return false;
        }
        // `black_box` keeps the compiler from turning this into an early exit.
        let diff = value.iter().zip(secret).fold(0, |diff, (a, b)| black_box(diff | (a ^ b)));
        diff == 0
    }

    fn info(&self) -> Value {
        let store = match self.kind {
            StoreKind::Home => "home",
            StoreKind::DataDir => "data_dir",
            StoreKind::Url => "url",
        };
        let home = self.home.as_ref().map(|home| home.to_string_lossy());
        let version = crate::VERSION;
        json!({"version": version, "tenant": self.tenant, "store": store, "home": home})
    }

    /// Forwards `GET <target without /api>` with the token. The store's status and body come
    /// back unchanged, but a 401 becomes 502 `store_unauthorized`. So a 401 from the studio
    /// always means a wrong secret. 504 when the deadline passed, else 502 when the store failed.
    fn read(&self, path: &str, target: &str) -> Response {
        let Some(target) = target.strip_prefix("/api") else { return error(404, "not_found") };
        let content_type =
            if path == "/api/metrics" { "text/plain; version=0.0.4" } else { "application/json" };
        match get(&self.launch, target, Some(&self.token), Instant::now() + DEADLINE) {
            Ok(reply) if reply.status == 401 => {
                let message = "the store refused the studio's token";
                json(502, &json!({"error": "store_unauthorized", "message": message}))
            }
            Ok(reply) => answer(reply.status, content_type, reply.body),
            Err(failure) if failure.kind() == io::ErrorKind::TimedOut => {
                error(504, "store_timeout")
            }
            Err(failure) => {
                json(502, &json!({"error": "store_unreachable", "message": failure.to_string()}))
            }
        }
    }

    /// The chat-start block for `?repo=`, built by the hook's own code. The hook reads only
    /// the home, so this answers only when the studio uses the home too.
    fn block(&self, target: &str) -> Response {
        if self.kind != StoreKind::Home {
            return error(404, "not_home");
        }
        let repo = api::params(target).and_then(|mut params| {
            let repo = params.remove("repo").filter(|repo| (1..=255).contains(&repo.len()));
            match (repo, params.is_empty()) {
                (Some(repo), true) => Ok(repo),
                _ => Err("pass only repo, 1 to 255 bytes".to_owned()),
            }
        });
        let repo = match repo {
            Ok(repo) => repo,
            Err(message) => {
                return json(400, &json!({"error": "invalid_input", "message": message}))
            }
        };
        match crate::hook::block_for(&repo, Instant::now() + DEADLINE) {
            Ok(block) => {
                let chars = block.as_ref().map_or(0, |block| block.chars().count());
                let max = crate::hook::MAX_BLOCK;
                json(200, &json!({"repo": repo, "block": block, "chars": chars, "max": max}))
            }
            Err(message) => json(502, &json!({"error": "store_unreachable", "message": message})),
        }
    }
}

/// A response with the browser headers.
fn answer(status: u16, content_type: &'static str, body: Vec<u8>) -> Response {
    Response { status, body, content_type, headers: HEADERS, _held: None }
}

fn json(status: u16, body: &Value) -> Response {
    answer(status, "application/json", body.to_string().into_bytes())
}

/// `{"error": <error>}`.
fn error(status: u16, error: &str) -> Response {
    json(status, &json!({"error": error}))
}
