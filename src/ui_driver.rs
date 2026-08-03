//! Opt-in local HTTP driver for controlling and observing the native game.
//!
//! The server deliberately accepts loopback addresses only. It feeds key
//! transitions into the same queue as the active video backend (GUI window or
//! console), captures the logical 320×200 RGBA frame, publishes a structured
//! game **state** snapshot, and optionally runs a **step clock** so an external
//! agent can advance the engine one frame at a time.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use crate::keys::KeyCode;

use crate::game_loop::{render_rgba, FRAME_TIME, PalColor};
use crate::surface::{Surface, SCREEN_H, SCREEN_W};

pub const DEFAULT_BIND: &str = "127.0.0.1:8765";
const TAP_DURATION: Duration = Duration::from_millis(75);

/// Process-wide control plane installed when a UI driver starts (latest wins).
static CONTROL: RwLock<Option<Arc<SharedControl>>> = RwLock::new(None);

#[derive(Default)]
struct LatestFrame {
    id: u64,
    rgba: Vec<u8>,
}

struct SharedControl {
    latest_frame: RwLock<LatestFrame>,
    /// Latest JSON body for `GET /v1/state` (without trailing framing).
    state_json: RwLock<String>,
    step_enabled: AtomicBool,
    /// Virtual millisecond clock used when step mode is on (`Engine::ticks`).
    virtual_ms: AtomicU64,
    step_pair: (Mutex<()>, Condvar),
}

impl SharedControl {
    fn new(step_enabled: bool) -> Self {
        Self {
            latest_frame: RwLock::new(LatestFrame::default()),
            state_json: RwLock::new(
                "{\"status\":\"starting\",\"frame_id\":0,\"step_mode\":false}\n".into(),
            ),
            step_enabled: AtomicBool::new(step_enabled),
            virtual_ms: AtomicU64::new(0),
            step_pair: (Mutex::new(()), Condvar::new()),
        }
    }

    fn step_enabled(&self) -> bool {
        self.step_enabled.load(Ordering::SeqCst)
    }

    fn virtual_ms(&self) -> u64 {
        self.virtual_ms.load(Ordering::SeqCst)
    }

    fn advance_ms(&self, ms: u64) {
        if ms == 0 {
            return;
        }
        self.virtual_ms.fetch_add(ms, Ordering::SeqCst);
        self.step_pair.1.notify_all();
    }

    /// Block until `virtual_ms >= deadline` (or step mode is turned off).
    fn wait_until_ms(&self, deadline: u64) {
        if !self.step_enabled() {
            return;
        }
        let (lock, cvar) = &self.step_pair;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        while self.step_enabled() && self.virtual_ms() < deadline {
            let result = cvar
                .wait_timeout(guard, Duration::from_secs(30))
                .unwrap_or_else(|e| e.into_inner());
            guard = result.0;
            // Spurious wake / timeout: loop and re-check.
        }
    }
}

fn control() -> Option<Arc<SharedControl>> {
    CONTROL.read().ok().and_then(|g| g.clone())
}

fn install_control(control: Arc<SharedControl>) {
    if let Ok(mut slot) = CONTROL.write() {
        *slot = Some(control);
    }
}

/// True when the engine should use the **virtual** step clock and block in
/// `delay_until` until `POST /v1/step`.
///
/// `--ui-step` alone enables this for headless/offscreen agents. With
/// **console** video, the gate is off by default so the alternate screen keeps
/// animating in real time (otherwise the first `delay` freezes on a blank
/// frame until an agent steps). Force the freeze even on console with
/// `RUSTPAL_UI_STEP_STRICT=1`.
pub(crate) fn step_mode_enabled() -> bool {
    let Some(c) = control() else {
        return false;
    };
    if !c.step_enabled() {
        return false;
    }
    if env_flag("RUSTPAL_UI_STEP_STRICT") {
        return true;
    }
    // Console needs wall-clock presents; agent still has /v1/state + input.
    if std::env::var_os("RUSTPAL_CONSOLE").is_some() {
        return false;
    }
    true
}

/// `--ui-step` / `RUSTPAL_UI_STEP` was set (API may still accept /v1/step).
pub(crate) fn step_mode_configured() -> bool {
    control().is_some_and(|c| c.step_enabled())
}

/// Virtual clock (ms) when step mode is active.
pub(crate) fn virtual_ticks() -> u64 {
    control().map(|c| c.virtual_ms()).unwrap_or(0)
}

/// Wait until the virtual clock reaches `deadline` (step mode only).
pub(crate) fn wait_until_virtual(deadline: u64) {
    if let Some(c) = control() {
        c.wait_until_ms(deadline);
    }
}

/// Replace the published `GET /v1/state` body.
pub(crate) fn publish_state_json(json: String) {
    if let Some(c) = control() {
        if let Ok(mut slot) = c.state_json.write() {
            *slot = json;
        }
    }
}

/// Current frame id from the last capture (0 if none).
pub(crate) fn latest_frame_id() -> u64 {
    control()
        .and_then(|c| c.latest_frame.read().ok().map(|f| f.id))
        .unwrap_or(0)
}

fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim();
            !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false") || v == "no")
        }
        Err(_) => false,
    }
}

/// Running UI driver. Dropping it disconnects the game from the server; the
/// listener thread itself is intentionally process-scoped.
pub(crate) struct UiDriver {
    input_rx: Receiver<(KeyCode, bool)>,
    control: Arc<SharedControl>,
    capture_rgba: Vec<u8>,
    #[cfg(test)]
    local_addr: SocketAddr,
}

impl UiDriver {
    /// Start from `RUSTPAL_UI_DRIVER` when set; otherwise `Ok(None)`.
    pub(crate) fn start_from_env() -> io::Result<Option<Self>> {
        match std::env::var("RUSTPAL_UI_DRIVER") {
            Ok(bind) => Ok(Some(Self::start(&bind)?)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid RUSTPAL_UI_DRIVER: {error}"),
            )),
        }
    }

    pub(crate) fn start(bind: &str) -> io::Result<Self> {
        let bind = if matches!(bind, "" | "1" | "true") {
            DEFAULT_BIND
        } else {
            bind
        };
        let addr: SocketAddr = bind.parse().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid UI driver address {bind:?}: {error}"),
            )
        })?;
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "UI driver must bind to a loopback address, not {}",
                    addr.ip()
                ),
            ));
        }

        let step_enabled = env_flag("RUSTPAL_UI_STEP");
        let control = Arc::new(SharedControl::new(step_enabled));
        install_control(Arc::clone(&control));

        let listener = TcpListener::bind(addr)?;
        let local_addr = listener.local_addr()?;
        let (input_tx, input_rx) = mpsc::channel();
        let serve_control = Arc::clone(&control);
        thread::Builder::new()
            .name("rustpal-ui-driver".into())
            .spawn(move || serve(listener, input_tx, serve_control))
            .map_err(|error| io::Error::other(format!("start UI driver thread: {error}")))?;

        eprintln!("rustpal: UI driver listening on http://{local_addr}");
        if step_enabled {
            if std::env::var_os("RUSTPAL_CONSOLE").is_some()
                && !env_flag("RUSTPAL_UI_STEP_STRICT")
            {
                eprintln!(
                    "rustpal: UI step requested with console — using **realtime** display so the \
                     terminal keeps painting. Set RUSTPAL_UI_STEP_STRICT=1 to freeze the clock \
                     until POST /v1/step (needed for single-frame AI loops while watching)."
                );
            } else {
                eprintln!(
                    "rustpal: UI step mode ON — engine clock is virtual; the game will not advance \
                     until POST /v1/step (default +{FRAME_TIME}ms per call). Without steps the \
                     screen stays blank/frozen."
                );
            }
        }
        Ok(Self {
            input_rx,
            control,
            capture_rgba: vec![0; SCREEN_W * SCREEN_H * 4],
            #[cfg(test)]
            local_addr,
        })
    }

    pub(crate) fn drain_input(&self, events: &mut Vec<(KeyCode, bool)>) {
        events.extend(self.input_rx.try_iter());
    }

    pub(crate) fn capture(
        &mut self,
        surf: &Surface,
        palette: &[PalColor; 256],
        shake: Option<(u16, u16)>,
    ) {
        render_rgba(surf, palette, shake, &mut self.capture_rgba);
        if let Ok(mut frame) = self.control.latest_frame.write() {
            frame.id = frame.id.wrapping_add(1);
            if frame.rgba.len() == self.capture_rgba.len() {
                frame.rgba.copy_from_slice(&self.capture_rgba);
            } else {
                frame.rgba = self.capture_rgba.clone();
            }
        }
    }

    #[cfg(test)]
    fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    #[cfg(test)]
    fn control_for_test(&self) -> &SharedControl {
        &self.control
    }
}

fn serve(
    listener: TcpListener,
    input_tx: Sender<(KeyCode, bool)>,
    control: Arc<SharedControl>,
) {
    for connection in listener.incoming() {
        match connection {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                if let Err(error) = handle_connection(&mut stream, &input_tx, &control) {
                    eprintln!("rustpal: UI driver request failed: {error}");
                }
            }
            Err(error) => eprintln!("rustpal: UI driver accept failed: {error}"),
        }
    }
}

fn handle_connection(
    stream: &mut TcpStream,
    input_tx: &Sender<(KeyCode, bool)>,
    control: &SharedControl,
) -> io::Result<()> {
    let mut request = [0u8; 8192];
    let size = stream.read(&mut request)?;
    let request = String::from_utf8_lossy(&request[..size]);
    let Some(request_line) = request.lines().next() else {
        return write_text(stream, 400, "Bad Request", "empty request\n");
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        return write_text(stream, 400, "Bad Request", "malformed request line\n");
    };
    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path, ""),
    };

    if method == "OPTIONS" {
        return write_response(stream, 204, "No Content", "text/plain", &[]);
    }

    // Body after headers (for POST /v1/step JSON).
    let body = request
        .split("\r\n\r\n")
        .nth(1)
        .or_else(|| request.split("\n\n").nth(1))
        .unwrap_or("");

    match (method, path) {
        ("GET", "/") => write_text(stream, 200, "OK", API_HELP),
        ("GET", "/v1/status") => {
            let frame = control
                .latest_frame
                .read()
                .map_err(|_| io::Error::other("frame lock poisoned"))?;
            let body = format!(
                "{{\"status\":\"ok\",\"width\":{SCREEN_W},\"height\":{SCREEN_H},\
                 \"frame_id\":{},\"step_mode\":{},\"step_configured\":{},\"ticks\":{}}}\n",
                frame.id,
                step_mode_enabled(),
                control.step_enabled(),
                if step_mode_enabled() {
                    control.virtual_ms()
                } else {
                    0
                },
            );
            write_response(stream, 200, "OK", "application/json", body.as_bytes())
        }
        ("GET", "/v1/state") => {
            let json = control
                .state_json
                .read()
                .map_err(|_| io::Error::other("state lock poisoned"))?
                .clone();
            write_response(stream, 200, "OK", "application/json", json.as_bytes())
        }
        ("GET", "/v1/frame.png") => {
            let rgba = {
                let frame = control
                    .latest_frame
                    .read()
                    .map_err(|_| io::Error::other("frame lock poisoned"))?;
                if frame.rgba.is_empty() {
                    return write_text(stream, 503, "Service Unavailable", "no frame yet\n");
                }
                frame.rgba.clone()
            };
            let png = encode_png(&rgba)?;
            write_response(stream, 200, "OK", "image/png", &png)
        }
        ("POST", "/v1/step") => handle_step(stream, control, query, body),
        ("POST", path) if path.starts_with("/v1/input/") => handle_input(stream, path, input_tx),
        _ => write_text(stream, 404, "Not Found", "not found\n"),
    }
}

/// Parse step advance: default one game frame (`FRAME_TIME` ms).
///
/// Accepts:
/// - query `frames=N` or `ms=N`
/// - JSON body `{"frames":N}` or `{"ms":N}`
fn parse_step_ms(query: &str, body: &str) -> Result<u64, String> {
    for part in query.split('&').filter(|p| !p.is_empty()) {
        if let Some(v) = part.strip_prefix("frames=") {
            let n: u64 = v
                .parse()
                .map_err(|_| format!("invalid frames={v}"))?;
            return Ok(n.saturating_mul(FRAME_TIME).max(1));
        }
        if let Some(v) = part.strip_prefix("ms=") {
            let n: u64 = v.parse().map_err(|_| format!("invalid ms={v}"))?;
            return Ok(n.max(1));
        }
    }
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        if let Some(n) = json_u64_field(trimmed, "frames") {
            return Ok(n.saturating_mul(FRAME_TIME).max(1));
        }
        if let Some(n) = json_u64_field(trimmed, "ms") {
            return Ok(n.max(1));
        }
    }
    Ok(FRAME_TIME)
}

/// Tiny non-serde scanner for `"key": number`.
fn json_u64_field(body: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{key}\"");
    let i = body.find(&pattern)?;
    let after = &body[i + pattern.len()..];
    let after = after.trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let num: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num.parse().ok()
}

fn handle_step(
    stream: &mut TcpStream,
    control: &SharedControl,
    query: &str,
    body: &str,
) -> io::Result<()> {
    if !control.step_enabled() {
        return write_response(
            stream,
            409,
            "Conflict",
            "application/json",
            b"{\"error\":\"step mode not enabled; set RUSTPAL_UI_STEP=1 or pass --ui-step\"}\n",
        );
    }
    let ms = match parse_step_ms(query, body) {
        Ok(ms) => ms,
        Err(msg) => {
            return write_text(stream, 400, "Bad Request", &format!("{msg}\n"));
        }
    };
    control.advance_ms(ms);
    let ticks = control.virtual_ms();
    let gating = step_mode_enabled();
    let body = format!(
        "{{\"accepted\":true,\"advanced_ms\":{ms},\"ticks\":{ticks},\
         \"frame_time_ms\":{FRAME_TIME},\"gating\":{gating}}}\n"
    );
    write_response(stream, 202, "Accepted", "application/json", body.as_bytes())
}

fn handle_input(
    stream: &mut TcpStream,
    path: &str,
    input_tx: &Sender<(KeyCode, bool)>,
) -> io::Result<()> {
    let mut segments = path.trim_start_matches('/').split('/');
    let (Some("v1"), Some("input"), Some(key_name), action) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return write_text(
            stream,
            400,
            "Bad Request",
            "expected /v1/input/{key}/{action}\n",
        );
    };
    if segments.next().is_some() {
        return write_text(stream, 400, "Bad Request", "too many path segments\n");
    }
    let Some(key) = parse_key(key_name) else {
        return write_text(stream, 400, "Bad Request", "unknown key\n");
    };
    match action.unwrap_or("tap") {
        "press" => send_key(input_tx, key, true)?,
        "release" => send_key(input_tx, key, false)?,
        "tap" => {
            send_key(input_tx, key, true)?;
            thread::sleep(TAP_DURATION);
            send_key(input_tx, key, false)?;
        }
        _ => {
            return write_text(
                stream,
                400,
                "Bad Request",
                "action must be tap, press, or release\n",
            )
        }
    }
    write_response(
        stream,
        202,
        "Accepted",
        "application/json",
        b"{\"accepted\":true}\n",
    )
}

fn send_key(input_tx: &Sender<(KeyCode, bool)>, key: KeyCode, pressed: bool) -> io::Result<()> {
    input_tx
        .send((key, pressed))
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "game input queue closed"))
}

fn parse_key(name: &str) -> Option<KeyCode> {
    match name.to_ascii_lowercase().as_str() {
        "up" => Some(KeyCode::ArrowUp),
        "down" => Some(KeyCode::ArrowDown),
        "left" => Some(KeyCode::ArrowLeft),
        "right" => Some(KeyCode::ArrowRight),
        "menu" | "escape" | "esc" => Some(KeyCode::Escape),
        "confirm" | "search" | "enter" => Some(KeyCode::Enter),
        "space" => Some(KeyCode::Space),
        "page_up" | "pgup" => Some(KeyCode::PageUp),
        "page_down" | "pgdn" => Some(KeyCode::PageDown),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "repeat" | "r" => Some(KeyCode::KeyR),
        "auto" | "a" => Some(KeyCode::KeyA),
        "defend" | "d" => Some(KeyCode::KeyD),
        "use_item" | "e" => Some(KeyCode::KeyE),
        "throw_item" | "w" => Some(KeyCode::KeyW),
        "flee" | "q" => Some(KeyCode::KeyQ),
        "force" | "magic" | "f" => Some(KeyCode::KeyF),
        "status" | "s" => Some(KeyCode::KeyS),
        _ => None,
    }
}

fn encode_png(rgba: &[u8]) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, SCREEN_W as u32, SCREEN_H as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| io::Error::other(format!("encode frame header: {error}")))?;
        writer
            .write_image_data(rgba)
            .map_err(|error| io::Error::other(format!("encode frame pixels: {error}")))?;
    }
    Ok(bytes)
}

fn write_text(stream: &mut TcpStream, status: u16, reason: &str, body: &str) -> io::Result<()> {
    write_response(
        stream,
        status,
        reason,
        "text/plain; charset=utf-8",
        body.as_bytes(),
    )
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

const API_HELP: &str = "\
rustpal UI driver

GET  /v1/status
GET  /v1/state
GET  /v1/frame.png
POST /v1/step                 advance virtual clock (step mode only)
POST /v1/step?frames=N
POST /v1/step?ms=N
POST /v1/input/{key}/tap
POST /v1/input/{key}/press
POST /v1/input/{key}/release

Step mode: RUSTPAL_UI_STEP=1 or --ui-step (requires --ui-driver).
Default step advances one overworld frame (100ms virtual).

Keys: up, down, left, right, menu, confirm, space, page_up, page_down,
      home, end, repeat, auto, defend, use_item, throw_item, flee,
      force, status
";

#[cfg(test)]
mod tests {
    use super::*;

    fn request(addr: SocketAddr, request: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).expect("connect to UI driver");
        stream
            .write_all(request.as_bytes())
            .expect("write HTTP request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("read HTTP response");
        response
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let error = UiDriver::start("0.0.0.0:0")
            .err()
            .expect("non-loopback bind should fail");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn status_frame_and_input_endpoints_work() {
        let mut driver = UiDriver::start("127.0.0.1:0").expect("start UI driver");
        let status = request(
            driver.local_addr(),
            "GET /v1/status HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(status.starts_with(b"HTTP/1.1 200 OK"));
        assert!(status
            .windows(b"\"frame_id\":0".len())
            .any(|part| part == b"\"frame_id\":0"));

        let mut surface = Surface::screen();
        surface.pixels.fill(1);
        let mut palette = [[0; 3]; 256];
        palette[1] = [12, 34, 56];
        driver.capture(&surface, &palette, None);
        let frame = request(
            driver.local_addr(),
            "GET /v1/frame.png HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(frame.starts_with(b"HTTP/1.1 200 OK"));
        assert!(frame.windows(8).any(|part| part == b"\x89PNG\r\n\x1a\n"));

        let input = request(
            driver.local_addr(),
            "POST /v1/input/confirm/tap HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(input.starts_with(b"HTTP/1.1 202 Accepted"));
        let mut events = Vec::new();
        driver.drain_input(&mut events);
        assert_eq!(events, [(KeyCode::Enter, true), (KeyCode::Enter, false)]);
    }

    #[test]
    fn state_endpoint_returns_published_json() {
        let driver = UiDriver::start("127.0.0.1:0").expect("start UI driver");
        publish_state_json("{\"hello\":1,\"frame_id\":3}\n".into());
        let response = request(
            driver.local_addr(),
            "GET /v1/state HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));
        let body = String::from_utf8_lossy(&response);
        assert!(body.contains("\"hello\":1"));
        assert!(body.contains("\"frame_id\":3"));
    }

    #[test]
    fn step_advances_virtual_clock_when_enabled() {
        std::env::set_var("RUSTPAL_UI_STEP", "1");
        let driver = UiDriver::start("127.0.0.1:0").expect("start step driver");
        assert!(driver.control_for_test().step_enabled());
        assert_eq!(driver.control_for_test().virtual_ms(), 0);
        let response = request(
            driver.local_addr(),
            "POST /v1/step?frames=2 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(
            response.starts_with(b"HTTP/1.1 202"),
            "{}",
            String::from_utf8_lossy(&response)
        );
        assert_eq!(driver.control_for_test().virtual_ms(), FRAME_TIME * 2);
        std::env::remove_var("RUSTPAL_UI_STEP");
    }

    #[test]
    fn parse_step_ms_defaults_and_fields() {
        assert_eq!(parse_step_ms("", "").unwrap(), FRAME_TIME);
        assert_eq!(parse_step_ms("frames=3", "").unwrap(), FRAME_TIME * 3);
        assert_eq!(parse_step_ms("ms=50", "").unwrap(), 50);
        assert_eq!(
            parse_step_ms("", "{\"frames\": 2}").unwrap(),
            FRAME_TIME * 2
        );
        assert_eq!(parse_step_ms("", "{\"ms\":12}").unwrap(), 12);
    }
}
