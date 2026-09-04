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
use std::time::{Duration, Instant};

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

/// Shared control + game clock for the UI driver.
///
/// Clock model (always used while the driver is installed):
/// - **gating on** (`step_gating`): time only advances via `POST /v1/step`
/// - **gating off**: wall-clock realtime, seamless toggle without jumps
struct SharedControl {
    latest_frame: RwLock<LatestFrame>,
    /// Latest JSON body for `GET /v1/state` (without trailing framing).
    state_json: RwLock<String>,
    /// On-demand: party roster (not every state poll).
    party_json: RwLock<String>,
    /// On-demand: inventory (not every state poll).
    inventory_json: RwLock<String>,
    /// On-demand: sparse blocked map tiles (not every state poll).
    obstacles_json: RwLock<String>,
    /// When true, engine delays block until `POST /v1/step` (strict step mode).
    step_gating: AtomicBool,
    /// Milliseconds while gating is on.
    virtual_ms: AtomicU64,
    /// Realtime clock base when gating is off: `base + anchor.elapsed()`.
    realtime_base_ms: AtomicU64,
    realtime_anchor: Mutex<Instant>,
    step_pair: (Mutex<()>, Condvar),
}

impl SharedControl {
    fn new(initial_gating: bool) -> Self {
        let now = Instant::now();
        Self {
            latest_frame: RwLock::new(LatestFrame::default()),
            state_json: RwLock::new(
                "{\"status\":\"starting\",\"frame_id\":0,\"step_mode\":false}\n".into(),
            ),
            party_json: RwLock::new("{\"status\":\"starting\",\"party\":[]}\n".into()),
            inventory_json: RwLock::new("{\"status\":\"starting\",\"inventory\":[]}\n".into()),
            obstacles_json: RwLock::new("{\"status\":\"starting\",\"tiles\":[]}\n".into()),
            step_gating: AtomicBool::new(initial_gating),
            virtual_ms: AtomicU64::new(0),
            realtime_base_ms: AtomicU64::new(0),
            realtime_anchor: Mutex::new(now),
            step_pair: (Mutex::new(()), Condvar::new()),
        }
    }

    fn step_gating(&self) -> bool {
        self.step_gating.load(Ordering::SeqCst)
    }

    /// Current game clock (ms) — continuous across gating toggles.
    fn now_ms(&self) -> u64 {
        if self.step_gating() {
            self.virtual_ms.load(Ordering::SeqCst)
        } else {
            let base = self.realtime_base_ms.load(Ordering::SeqCst);
            let elapsed = self
                .realtime_anchor
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .elapsed()
                .as_millis() as u64;
            base.saturating_add(elapsed)
        }
    }

    /// Enable/disable strict step gating without jumping the game clock.
    fn set_step_gating(&self, on: bool) {
        if on == self.step_gating() {
            return;
        }
        if on {
            let now = self.now_ms(); // still realtime
            self.virtual_ms.store(now, Ordering::SeqCst);
            self.step_gating.store(true, Ordering::SeqCst);
            self.step_pair.1.notify_all();
        } else {
            let now = self.virtual_ms.load(Ordering::SeqCst); // still gated
            self.realtime_base_ms.store(now, Ordering::SeqCst);
            if let Ok(mut anchor) = self.realtime_anchor.lock() {
                *anchor = Instant::now();
            }
            self.step_gating.store(false, Ordering::SeqCst);
            self.step_pair.1.notify_all(); // unblock waiters when gating turns off
        }
    }

    fn advance_ms(&self, ms: u64) {
        if ms == 0 || !self.step_gating() {
            return;
        }
        self.virtual_ms.fetch_add(ms, Ordering::SeqCst);
        self.step_pair.1.notify_all();
    }

    /// Block until gated clock reaches `deadline` (or gating is turned off).
    fn wait_until_ms(&self, deadline: u64) {
        if !self.step_gating() {
            return;
        }
        let (lock, cvar) = &self.step_pair;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        while self.step_gating() && self.now_ms() < deadline {
            let result = cvar
                .wait_timeout(guard, Duration::from_secs(30))
                .unwrap_or_else(|e| e.into_inner());
            guard = result.0;
        }
    }

    fn config_json(&self) -> String {
        format!(
            "{{\"status\":\"ok\",\"step_gating\":{},\"step_mode\":{},\"ticks\":{}}}\n",
            self.step_gating(),
            self.step_gating(),
            self.now_ms(),
        )
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

/// True when the engine should **gate** on `POST /v1/step` (strict step mode).
///
/// Toggle at runtime with `POST /v1/config` `{"step_gating":true|false}`.
/// Startup default: on only if `RUSTPAL_UI_STEP_STRICT=1`, or `--ui-step` without
/// console (headless). Console + `--ui-step` alone still defaults to realtime
/// so the terminal keeps painting until an agent enables gating.
pub(crate) fn step_mode_enabled() -> bool {
    control().is_some_and(|c| c.step_gating())
}

/// UI driver is installed (config / step endpoints available).
pub(crate) fn step_mode_configured() -> bool {
    control().is_some()
}

/// Driver game clock in ms (always available while UI driver is installed).
pub(crate) fn driver_ticks_ms() -> Option<u64> {
    control().map(|c| c.now_ms())
}

/// Driver clock (ms); alias of [`driver_ticks_ms`].
#[allow(dead_code)]
pub(crate) fn virtual_ticks() -> u64 {
    control().map(|c| c.now_ms()).unwrap_or(0)
}

/// Wait until the gated clock reaches `deadline` (no-op if gating is off).
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

/// Publish on-demand party / inventory / obstacles snapshots (updated each engine tick).
pub(crate) fn publish_party_inventory_json(party: String, inventory: String, obstacles: String) {
    if let Some(c) = control() {
        if let Ok(mut slot) = c.party_json.write() {
            *slot = party;
        }
        if let Ok(mut slot) = c.inventory_json.write() {
            *slot = inventory;
        }
        if let Ok(mut slot) = c.obstacles_json.write() {
            *slot = obstacles;
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

        // Initial gating: STRICT env always on; bare --ui-step only when not console.
        let initial_gating = env_flag("RUSTPAL_UI_STEP_STRICT")
            || (env_flag("RUSTPAL_UI_STEP") && std::env::var_os("RUSTPAL_CONSOLE").is_none());
        let control = Arc::new(SharedControl::new(initial_gating));
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
        eprintln!(
            "rustpal: step_gating={} (toggle anytime: POST /v1/config {{\"step_gating\":true|false}})",
            initial_gating
        );
        if initial_gating {
            eprintln!(
                "rustpal: strict step ON — clock advances only via POST /v1/step \
                 (+{FRAME_TIME}ms default). Screen freezes until stepped."
            );
        } else {
            eprintln!(
                "rustpal: realtime clock (step_gating off). Enable strict step over HTTP when needed."
            );
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
    // Read headers first, then body by Content-Length (small POSTs from curl/urllib).
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break Some(pos);
        }
        if buf.len() > 64 * 1024 {
            return write_text(stream, 413, "Payload Too Large", "request too large\n");
        }
    };
    let Some(header_end) = header_end else {
        return write_text(stream, 400, "Bad Request", "empty request\n");
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let mut body_buf = buf[header_end..].to_vec();
    let content_length = headers
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while body_buf.len() < content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        body_buf.extend_from_slice(&tmp[..n]);
    }
    body_buf.truncate(content_length);
    let body = String::from_utf8_lossy(&body_buf);

    let Some(request_line) = headers.lines().next() else {
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

    match (method, path) {
        ("GET", "/") => write_text(stream, 200, "OK", API_HELP),
        ("GET", "/v1/status") => {
            let frame = control
                .latest_frame
                .read()
                .map_err(|_| io::Error::other("frame lock poisoned"))?;
            let body = format!(
                "{{\"status\":\"ok\",\"width\":{SCREEN_W},\"height\":{SCREEN_H},\
                 \"frame_id\":{},\"step_mode\":{},\"step_gating\":{},\"step_configured\":true,\
                 \"ticks\":{}}}\n",
                frame.id,
                control.step_gating(),
                control.step_gating(),
                control.now_ms(),
            );
            write_response(stream, 200, "OK", "application/json", body.as_bytes())
        }
        ("GET", "/v1/config") => {
            write_response(
                stream,
                200,
                "OK",
                "application/json",
                control.config_json().as_bytes(),
            )
        }
        ("POST", "/v1/config") => handle_config(stream, control, body.as_ref()),
        ("GET", "/v1/state") => {
            let json = control
                .state_json
                .read()
                .map_err(|_| io::Error::other("state lock poisoned"))?
                .clone();
            write_response(stream, 200, "OK", "application/json", json.as_bytes())
        }
        ("GET", "/v1/party") => {
            let json = control
                .party_json
                .read()
                .map_err(|_| io::Error::other("party lock poisoned"))?
                .clone();
            write_response(stream, 200, "OK", "application/json", json.as_bytes())
        }
        ("GET", "/v1/inventory") => {
            let json = control
                .inventory_json
                .read()
                .map_err(|_| io::Error::other("inventory lock poisoned"))?
                .clone();
            write_response(stream, 200, "OK", "application/json", json.as_bytes())
        }
        ("GET", "/v1/obstacles") => {
            let json = control
                .obstacles_json
                .read()
                .map_err(|_| io::Error::other("obstacles lock poisoned"))?
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
        ("POST", "/v1/step") => handle_step(stream, control, query, body.as_ref()),
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

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

fn handle_config(stream: &mut TcpStream, control: &SharedControl, body: &str) -> io::Result<()> {
    // Accept {"step_gating":true|false} or {"step_mode":true|false} (aliases).
    let trimmed = body.trim();
    let gating = json_bool_field(trimmed, "step_gating")
        .or_else(|| json_bool_field(trimmed, "step_mode"))
        .or_else(|| json_bool_field(trimmed, "strict"));
    let Some(on) = gating else {
        return write_response(
            stream,
            400,
            "Bad Request",
            "application/json",
            b"{\"error\":\"expected JSON {\\\"step_gating\\\":true|false}\"}\n",
        );
    };
    control.set_step_gating(on);
    // Cached GET /v1/state is only rebuilt on engine frames. When gating turns
    // on the clock freezes, so without a patch the snapshot keeps stale
    // step_gating/step_mode until the next POST /v1/step — agents reading only
    // /v1/state would think gating never applied.
    patch_cached_state_gating(control, on);
    write_response(
        stream,
        200,
        "OK",
        "application/json",
        control.config_json().as_bytes(),
    )
}

/// Rewrite `step_mode` / `step_gating` booleans in the published state snapshot.
fn patch_cached_state_gating(control: &SharedControl, on: bool) {
    let Ok(mut slot) = control.state_json.write() else {
        return;
    };
    let val = if on { "true" } else { "false" };
    for key in ["step_gating", "step_mode"] {
        for old in ["true", "false"] {
            let from = format!("\"{key}\":{old}");
            let to = format!("\"{key}\":{val}");
            if slot.contains(&from) {
                *slot = slot.replacen(&from, &to, 1);
                break;
            }
        }
    }
}

fn json_bool_field(body: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{key}\"");
    let i = body.find(&pattern)?;
    let after = body[i + pattern.len()..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    if after.starts_with("true") {
        Some(true)
    } else if after.starts_with("false") {
        Some(false)
    } else if after.starts_with('1') {
        Some(true)
    } else if after.starts_with('0') {
        Some(false)
    } else {
        None
    }
}

fn handle_step(
    stream: &mut TcpStream,
    control: &SharedControl,
    query: &str,
    body: &str,
) -> io::Result<()> {
    if !control.step_gating() {
        return write_response(
            stream,
            409,
            "Conflict",
            "application/json",
            b"{\"error\":\"step_gating is off; POST /v1/config {\\\"step_gating\\\":true} first\"}\n",
        );
    }
    let ms = match parse_step_ms(query, body) {
        Ok(ms) => ms,
        Err(msg) => {
            return write_text(stream, 400, "Bad Request", &format!("{msg}\n"));
        }
    };
    control.advance_ms(ms);
    let ticks = control.now_ms();
    let body = format!(
        "{{\"accepted\":true,\"advanced_ms\":{ms},\"ticks\":{ticks},\
         \"frame_time_ms\":{FRAME_TIME},\"gating\":true}}\n"
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
GET  /v1/state                lightweight observe (no party/inventory/tiles)
GET  /v1/party                party roster (on demand)
GET  /v1/inventory            inventory (on demand)
GET  /v1/obstacles            sparse blocked tiles (on demand)
GET  /v1/config               step_gating / ticks
GET  /v1/frame.png
POST /v1/config               {\"step_gating\":true|false}  enable/disable strict step
POST /v1/step                 advance clock when step_gating is on
POST /v1/step?frames=N
POST /v1/step?ms=N
POST /v1/input/{key}/tap
POST /v1/input/{key}/press
POST /v1/input/{key}/release

step_gating (strict step): clock freezes until POST /v1/step.
Toggle anytime: POST /v1/config {\"step_gating\":true|false}.
Optional startup default: RUSTPAL_UI_STEP_STRICT=1.
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
        // Write this driver's slot directly (global CONTROL may race with parallel tests).
        *driver
            .control_for_test()
            .state_json
            .write()
            .expect("state lock") = "{\"hello\":1,\"frame_id\":3}\n".into();
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
    fn party_and_inventory_endpoints_return_published_json() {
        let driver = UiDriver::start("127.0.0.1:0").expect("start UI driver");
        *driver.control_for_test().party_json.write().expect("lock") =
            "{\"status\":\"ok\",\"party\":[{\"name\":\"X\"}]}\n".into();
        *driver
            .control_for_test()
            .inventory_json
            .write()
            .expect("lock") = "{\"status\":\"ok\",\"inventory\":[]}\n".into();
        *driver
            .control_for_test()
            .obstacles_json
            .write()
            .expect("lock") = "{\"status\":\"ok\",\"tiles\":[[1,2,0]]}\n".into();
        let party = request(
            driver.local_addr(),
            "GET /v1/party HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        let inv = request(
            driver.local_addr(),
            "GET /v1/inventory HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        let obs = request(
            driver.local_addr(),
            "GET /v1/obstacles HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&party).contains("\"name\":\"X\""));
        assert!(String::from_utf8_lossy(&inv).contains("\"inventory\":[]"));
        assert!(String::from_utf8_lossy(&obs).contains("\"tiles\":[[1,2,0]]"));
    }

    #[test]
    fn step_advances_virtual_clock_when_gating_enabled() {
        let driver = UiDriver::start("127.0.0.1:0").expect("start UI driver");
        // Enable gating via API (no startup flag required).
        let cfg = request(
            driver.local_addr(),
            "POST /v1/config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: 20\r\n\r\n{\"step_gating\":true}",
        );
        assert!(
            cfg.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&cfg)
        );
        assert!(driver.control_for_test().step_gating());
        let before = driver.control_for_test().now_ms();
        let response = request(
            driver.local_addr(),
            "POST /v1/step?frames=2 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(
            response.starts_with(b"HTTP/1.1 202"),
            "{}",
            String::from_utf8_lossy(&response)
        );
        assert_eq!(
            driver.control_for_test().now_ms(),
            before + FRAME_TIME * 2
        );
        // Disable gating
        let off = request(
            driver.local_addr(),
            "POST /v1/config HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: 21\r\n\r\n{\"step_gating\":false}",
        );
        assert!(off.starts_with(b"HTTP/1.1 200"));
        assert!(!driver.control_for_test().step_gating());
        // Step rejected when gating off
        let rejected = request(
            driver.local_addr(),
            "POST /v1/step?frames=1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(
            rejected.starts_with(b"HTTP/1.1 409"),
            "{}",
            String::from_utf8_lossy(&rejected)
        );
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
