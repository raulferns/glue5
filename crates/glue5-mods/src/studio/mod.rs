//! GLUE 5 Studio's server: the web page (embedded), a WebSocket for the
//! Studio (files, mods, logs, eval), one for the type checker (LSP), and the
//! MCP endpoint for coding agents (`POST /mcp`). Plain `std::net`, a thread
//! per connection: a handful of clients at most.
//!
//! Whoever reaches the server runs code in the game, so every route but
//! the page itself needs the token (`?token=` or `Authorization: Bearer`),
//! and WebSockets must come from the page's own origin (a page elsewhere in
//! the same browser could otherwise drive the game through localhost).

mod api;
mod assets;
mod lsp;
mod mcp;
mod session;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use api::GUIDE;

/// The Studio's default port.
pub const PORT: u16 = 47110;

/// A running server; dropping it stops it.
pub struct Studio {
    port: u16,
    token: String,
    lan: bool,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Studio {
    /// Serves on `port` (0: any), on every interface when `lan`, else only
    /// for this device.
    pub fn start(port: u16, lan: bool) -> std::io::Result<Studio> {
        let ip = if lan { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST };
        let listener = TcpListener::bind(SocketAddr::from((ip, port)))?;
        let port = listener.local_addr()?.port();
        let token = token();
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Server { token: token.clone(), failures: Mutex::new(Vec::new()) });
        let stopping = stop.clone();
        let thread = std::thread::Builder::new().name("studio".into()).spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let server = shared.clone();
                let _ = std::thread::Builder::new().name("studio client".into()).spawn(move || {
                    if let Err(e) = server.serve(stream) {
                        log::debug!("studio: {e}");
                    }
                });
            }
        })?;
        log::info!("studio: serving on port {port}{}", if lan { " (network)" } else { " (this device)" });
        Ok(Studio { port, token, lan, stop, thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The pairing code, also the token (`ABCD-EF12` as shown; either form works).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The code as people type it.
    pub fn pairing_code(&self) -> String {
        format!("{}-{}", &self.token[..4], &self.token[4..])
    }

    /// The page's address on `host` (an address of this device), token included.
    pub fn url(&self, host: &str) -> String {
        format!("http://{host}:{}/?token={}", self.port, self.token)
    }

    /// Whether other devices may connect.
    pub fn lan(&self) -> bool {
        self.lan
    }
}

impl Drop for Studio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the accept loop.
        let _ = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)), Duration::from_secs(1));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 8 characters of Crockford's base32: 40 bits.
fn token() -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("random");
    bytes.iter().map(|b| ALPHABET[(b & 31) as usize] as char).collect()
}

/// How many sessions use the type checker: it goes when none does.
static CHECKER_USERS: AtomicU32 = AtomicU32::new(0);

fn checker_users(delta: i32) {
    if delta > 0 {
        CHECKER_USERS.fetch_add(1, Ordering::AcqRel);
    } else if delta < 0 {
        let left = CHECKER_USERS.try_update(Ordering::AcqRel, Ordering::Acquire, |n| Some(n.saturating_sub(1)));
        if left.unwrap_or(0) <= 1 {
            crate::checker::Checker::release();
        }
    }
}

struct Server {
    token: String,
    /// Times of wrong tokens, for the rate limit.
    failures: Mutex<Vec<Instant>>,
}

/// A request's head and body.
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

const MAX_HEAD: usize = 32 << 10;
const MAX_BODY: usize = 8 << 20;

impl Server {
    fn serve(&self, stream: TcpStream) -> std::io::Result<()> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut out = stream;
        loop {
            let Some(request) = read_request(&mut reader)? else { return Ok(()) };
            let close = request.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
            let upgrade = request.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/studio" | "/lsp") if upgrade => {
                    if !self.authorized(&request) || !same_origin(&request) {
                        return respond(&mut out, 401, "text/plain", b"pair with the game first", &[]);
                    }
                    // The handshake is done: the reader's buffer is empty
                    // (a client waits for the answer before it sends).
                    drop(reader);
                    let ws = accept(out, &request)?;
                    if request.path == "/lsp" {
                        checker_users(1);
                        let r = lsp::run(ws);
                        checker_users(-1);
                        return r;
                    }
                    return session::run(ws);
                }
                ("POST", "/mcp") => {
                    if !self.authorized(&request) || request.header("origin").is_some_and(|_| !same_origin(&request)) {
                        respond(&mut out, 401, "application/json", br#"{"error":"missing or wrong token"}"#, &[])?;
                    } else {
                        let (status, body, extra) = mcp::handle(&request);
                        let headers: Vec<(&str, &str)> = extra.iter().map(|(a, b)| (*a, b.as_str())).collect();
                        respond(&mut out, status, "application/json", body.as_bytes(), &headers)?;
                    }
                }
                ("GET" | "DELETE", "/mcp") => respond(&mut out, 405, "text/plain", b"POST only", &[("Allow", "POST")])?,
                ("GET", "/natives.d.luau") => {
                    if self.authorized(&request) {
                        let defs = format!("{}{}", crate::world::definitions(), crate::natives::DEFINITIONS);
                        respond(&mut out, 200, "text/plain; charset=utf-8", defs.as_bytes(), &[])?;
                    } else {
                        respond(&mut out, 401, "text/plain", b"pair with the game first", &[])?;
                    }
                }
                ("GET" | "HEAD", path) => match assets::find(path) {
                    Some(file) if request.header("if-none-match") == Some(file.etag) => {
                        respond(&mut out, 304, file.mime, &[], &[("ETag", file.etag), ("Cache-Control", "no-cache")])?;
                    }
                    Some(file) => {
                        let gzip = request.header("accept-encoding").is_some_and(|e| e.contains("gzip"));
                        let (body, encoding) = match (gzip, file.gzip) {
                            (true, Some(gz)) => (gz, Some("gzip")),
                            _ => (file.body, None),
                        };
                        let mut headers = vec![("Cache-Control", "no-cache"), ("ETag", file.etag), ("Vary", "Accept-Encoding")];
                        if let Some(e) = encoding {
                            headers.push(("Content-Encoding", e));
                        }
                        let body = if request.method == "HEAD" { &[][..] } else { body };
                        respond(&mut out, 200, file.mime, body, &headers)?;
                    }
                    None => respond(&mut out, 404, "text/plain", b"not found", &[])?,
                },
                _ => respond(&mut out, 405, "text/plain", b"method not allowed", &[])?,
            }
            if close {
                return Ok(());
            }
        }
    }

    /// The token is right, and not guessed: after 10 wrong ones in a minute
    /// no token is taken for that minute.
    fn authorized(&self, request: &Request) -> bool {
        let given = request
            .param("token")
            .map(str::to_string)
            .or_else(|| request.header("authorization").and_then(|a| a.strip_prefix("Bearer ")).map(str::to_string));
        let Some(given) = given else { return false };
        let given: String = given.chars().filter(|c| *c != '-' && *c != ' ').collect::<String>().to_ascii_uppercase();
        let mut failures = self.failures.lock().unwrap();
        failures.retain(|t| t.elapsed() < Duration::from_secs(60));
        if failures.len() >= 10 {
            return false;
        }
        let ok = given.len() == self.token.len()
            && given.bytes().zip(self.token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
        if !ok {
            failures.push(Instant::now());
        }
        ok
    }
}

/// A WebSocket's Origin is the page's own (same host and port).
fn same_origin(request: &Request) -> bool {
    match (request.header("origin"), request.header("host")) {
        (Some(origin), Some(host)) => origin.split_once("://").is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(host)),
        // Not a browser.
        (None, _) => true,
        _ => false,
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<Request>> {
    let mut head = Vec::new();
    loop {
        let start = head.len();
        if reader.read_until(b'\n', &mut head)? == 0 {
            return Ok(None);
        }
        if head.len() > MAX_HEAD {
            return Err(std::io::Error::other("request head too large"));
        }
        if head[start..] == *b"\r\n" || head[start..] == *b"\n" {
            break;
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    if !parsed.parse(&head).map_err(std::io::Error::other)?.is_complete() {
        return Err(std::io::Error::other("incomplete request"));
    }
    let target = parsed.path.unwrap_or("/").to_string();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    let headers: Vec<(String, String)> = parsed
        .headers
        .iter()
        .map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).into_owned()))
        .collect();
    let mut request = Request {
        method: parsed.method.unwrap_or("GET").to_string(),
        path: decode(path),
        query,
        headers,
        body: Vec::new(),
    };
    let len: usize = request.header("content-length").and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return Err(std::io::Error::other("request body too large"));
    }
    request.body.resize(len, 0);
    reader.read_exact(&mut request.body)?;
    Ok(Some(request))
}

/// Percent-decoding (and `+` as a space in queries).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(out: &mut TcpStream, status: u16, mime: &str, body: &[u8], headers: &[(&str, &str)]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    out.write_all(body)?;
    out.flush()
}

pub(crate) type Socket = tungstenite::WebSocket<TcpStream>;

/// Answers a WebSocket handshake, then speaks WebSocket on the stream.
fn accept(mut out: TcpStream, request: &Request) -> std::io::Result<Socket> {
    let key = request.header("sec-websocket-key").ok_or_else(|| std::io::Error::other("no Sec-WebSocket-Key"))?;
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    write!(
        out,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    )?;
    out.flush()?;
    // Short reads let a session send while it waits for messages.
    out.set_read_timeout(Some(Duration::from_millis(50)))?;
    let config = tungstenite::protocol::WebSocketConfig::default().max_message_size(Some(MAX_BODY));
    Ok(tungstenite::WebSocket::from_raw_socket(out, tungstenite::protocol::Role::Server, Some(config)))
}

/// A message, or none yet (the read timed out), or the end.
pub(crate) fn receive(ws: &mut Socket) -> Result<Option<String>, ()> {
    match ws.read() {
        Ok(tungstenite::Message::Text(t)) => Ok(Some(t.to_string())),
        Ok(tungstenite::Message::Close(_)) => Err(()),
        Ok(_) => Ok(None),
        Err(tungstenite::Error::Io(e))
            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
        {
            Ok(None)
        }
        Err(_) => Err(()),
    }
}

pub(crate) fn send(ws: &mut Socket, value: &serde_json::Value) -> Result<(), ()> {
    ws.send(tungstenite::Message::text(value.to_string())).map_err(|_| ())
}
