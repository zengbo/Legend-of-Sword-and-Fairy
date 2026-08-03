//! Pure terminal video backend (no window, no system graphics/audio libs).
//!
//! * **Kitty** — transmit native 320×200 RGBA; size on screen with protocol
//!   `c=` (columns). The terminal scales the image (fast, no huge bitmaps).
//! * **ANSI** — half-block truecolor cells, modest integer upscale.
//!
//! Input is read on a **background thread** from `/dev/tty` so keys (and a
//! stuck main thread) still work. Ctrl-C is handled by a signal handler that
//! **restores the terminal** (leave alt screen, re-enable echo) before exit —
//! a bare SIGINT kill would leave Kitty stuck on the game buffer.

use std::io::{self, Write};
#[cfg(not(unix))]
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use crate::game_loop::{render_rgba, PalColor};
use crate::keys::KeyCode;
use crate::surface::{Surface, SCREEN_H, SCREEN_W};

const KITTY_CHUNK: usize = 4096;
const IMAGE_ID: u32 = 1;
/// ~12–15 fps is enough for PAL and keeps Kitty APC traffic reasonable.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(70);
/// How long a key stays down for the engine after a terminal press / repeat.
const KEY_HOLD: Duration = Duration::from_millis(180);

/// Set by SIGINT/SIGTERM so the game loop can also quit cleanly if needed.
static CONSOLE_QUIT: AtomicBool = AtomicBool::new(false);
/// `/dev/tty` fd saved for emergency termios restore in the signal handler.
static TTY_FD: AtomicI32 = AtomicI32::new(-1);
/// Original termios bytes (platform `termios` layout) for async-signal-safe restore.
#[cfg(unix)]
static mut SAVED_TERMIOS: libc::termios = unsafe { std::mem::zeroed() };
#[cfg(unix)]
static TERMIOS_SAVED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleMode {
    Auto,
    Kitty,
    Ansi,
}

struct HeldKey {
    code: KeyCode,
    until: Instant,
}

pub struct ConsoleVideo {
    use_kitty: bool,
    /// Kitty: display width in terminal cells (`c=`). Aspect preserved by Kitty.
    place_cols: u32,
    /// ANSI only: integer nearest-neighbor scale (1–3).
    ansi_scale: u32,
    rgba: Vec<u8>,
    scaled: Vec<u8>,
    prev_rgba: Vec<u8>,
    last_present: Instant,
    close_requested: bool,
    frame_n: u64,
    /// Bytes from the input thread.
    key_rx: Receiver<u8>,
    esc_buf: Vec<u8>,
    esc_solo_since: Option<Instant>,
    held: Vec<HeldKey>,
    pending: Vec<(KeyCode, bool)>,
    #[cfg(unix)]
    _tty_guard: TtyRawGuard,
}

/// Restores termios on `/dev/tty` when dropped.
#[cfg(unix)]
struct TtyRawGuard {
    fd: i32,
    orig: libc::termios,
}

#[cfg(unix)]
impl Drop for TtyRawGuard {
    fn drop(&mut self) {
        restore_termios_fd(self.fd, &self.orig);
    }
}

/// Leave alt screen, show cursor, drop Kitty images, restore cooked termios.
/// Safe to call more than once. Used from `Drop` and from the SIGINT handler.
fn restore_terminal_ui() {
    // Best-effort writes — ignore errors (stdout may be half-dead in a handler).
    let seq = b"\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?25h\x1b[?1049l\r\n";
    let _ = io::stdout().write_all(seq);
    let _ = io::stdout().flush();
    #[cfg(unix)]
    unsafe {
        // Also write directly to the tty fd (async-signal-safe path for handlers).
        let fd = TTY_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            let _ = libc::write(fd, seq.as_ptr() as *const _, seq.len());
        }
        if TERMIOS_SAVED.load(Ordering::SeqCst) {
            let tfd = if fd >= 0 { fd } else { libc::STDIN_FILENO };
            let _ = libc::tcsetattr(tfd, libc::TCSANOW, std::ptr::addr_of!(SAVED_TERMIOS));
        }
    }
}

#[cfg(unix)]
fn restore_termios_fd(fd: i32, orig: &libc::termios) {
    unsafe {
        let _ = libc::tcsetattr(fd, libc::TCSANOW, orig);
    }
}

/// SIGINT/SIGTERM: restore the terminal then exit. Without this, a default
/// SIGINT aborts the process and skips `Drop`, leaving Kitty on the alt buffer
/// with echo off — looks like the game is still running.
#[cfg(unix)]
extern "C" fn console_signal_handler(sig: libc::c_int) {
    CONSOLE_QUIT.store(true, Ordering::SeqCst);
    // Inline minimal restore (only async-signal-safe calls).
    let seq = b"\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?25h\x1b[?1049l\r\n";
    unsafe {
        let fd = TTY_FD.load(Ordering::SeqCst);
        let out = if fd >= 0 { fd } else { libc::STDOUT_FILENO };
        let _ = libc::write(out, seq.as_ptr() as *const _, seq.len());
        if TERMIOS_SAVED.load(Ordering::SeqCst) {
            let tfd = if fd >= 0 { fd } else { libc::STDIN_FILENO };
            let _ = libc::tcsetattr(tfd, libc::TCSANOW, std::ptr::addr_of!(SAVED_TERMIOS));
        }
        // 128 + signal number (shell convention for fatal signals).
        let code = if sig == libc::SIGINT { 130 } else { 143 };
        libc::_exit(code);
    }
}

#[cfg(unix)]
fn install_console_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        // Classic handler (not SA_SIGINFO): signature fn(c_int).
        sa.sa_sigaction = console_signal_handler as *const () as usize;
        sa.sa_flags = 0;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
    }
}

impl ConsoleVideo {
    pub fn new(mode: ConsoleMode) -> io::Result<ConsoleVideo> {
        let use_kitty = match mode {
            ConsoleMode::Kitty => true,
            ConsoleMode::Ansi => false,
            ConsoleMode::Auto => detect_kitty(),
        };
        let (place_cols, ansi_scale) = resolve_display_size(use_kitty);

        #[cfg(unix)]
        let (key_rx, tty_guard) = spawn_tty_reader()?;
        #[cfg(not(unix))]
        let key_rx = spawn_stdin_reader();

        let mut out = io::stdout();
        // Alternate screen once; do NOT use CSI 2026 here (some Kitty builds
        // leave the alt buffer blank if sync mode is mishandled).
        write!(out, "\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H")?;
        let label = if use_kitty { "Kitty" } else { "ANSI" };
        let size_note = if use_kitty {
            format!("{place_cols} cols wide")
        } else {
            format!(
                "{}× → {}×{}",
                ansi_scale,
                SCREEN_W as u32 * ansi_scale,
                SCREEN_H as u32 * ansi_scale
            )
        };
        writeln!(
            out,
            "\x1b[36mrustpal console ({label} · {size_note}) — arrows/hjkl · Enter · Esc · Ctrl-C restores terminal & quits\x1b[0m"
        )?;
        out.flush()?;
        eprintln!(
            "rustpal: console backend ready ({label}, place_cols={place_cols}, ansi_scale={ansi_scale})"
        );

        #[cfg(unix)]
        install_console_signal_handlers();

        let scaled_len = if use_kitty {
            0
        } else {
            (SCREEN_W * SCREEN_H * 4) * (ansi_scale as usize).pow(2)
        };

        Ok(ConsoleVideo {
            use_kitty,
            place_cols,
            ansi_scale,
            rgba: vec![0; SCREEN_W * SCREEN_H * 4],
            scaled: vec![0; scaled_len],
            prev_rgba: Vec::new(),
            last_present: Instant::now() - MIN_FRAME_INTERVAL,
            close_requested: false,
            frame_n: 0,
            key_rx,
            esc_buf: Vec::new(),
            esc_solo_since: None,
            held: Vec::new(),
            pending: Vec::new(),
            #[cfg(unix)]
            _tty_guard: tty_guard,
        })
    }

    pub fn pump(&mut self) -> Vec<(KeyCode, bool)> {
        if CONSOLE_QUIT.load(Ordering::SeqCst) {
            self.close_requested = true;
        }
        let now = Instant::now();

        // Drain all pending tty bytes from the input thread.
        loop {
            match self.key_rx.try_recv() {
                Ok(b) => self.feed_byte(b, now),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.close_requested = true;
                    break;
                }
            }
        }

        // Bare Esc timeout (~50ms without CSI follow-up).
        if self.esc_buf == [0x1b] {
            match self.esc_solo_since {
                None => self.esc_solo_since = Some(now),
                Some(t) if now.duration_since(t) >= Duration::from_millis(50) => {
                    self.esc_buf.clear();
                    self.esc_solo_since = None;
                    self.note_press(KeyCode::Escape, now);
                }
                Some(_) => {}
            }
        } else if self.esc_buf.is_empty() {
            self.esc_solo_since = None;
        }

        // Expire held keys.
        let mut still = Vec::with_capacity(self.held.len());
        for h in self.held.drain(..) {
            if now >= h.until {
                self.pending.push((h.code, false));
            } else {
                still.push(h);
            }
        }
        self.held = still;

        std::mem::take(&mut self.pending)
    }

    pub fn present(
        &mut self,
        surf: &Surface,
        palette: &[PalColor; 256],
        shake: Option<(u16, u16)>,
    ) {
        let now = Instant::now();
        if now.duration_since(self.last_present) < MIN_FRAME_INTERVAL {
            return;
        }

        render_rgba(surf, palette, shake, &mut self.rgba);

        // Always draw the first few frames even if identical (empty buffer →
        // all zeros can match after a black fade).
        let force = self.frame_n < 3;
        if !force && self.prev_rgba == self.rgba {
            return;
        }
        self.prev_rgba.clone_from(&self.rgba);
        self.last_present = now;
        self.frame_n = self.frame_n.saturating_add(1);

        let mut out = io::stdout();
        if self.use_kitty {
            // Stay on row 2; C=1 keeps the cursor from scrolling the buffer.
            if self.frame_n == 1 {
                let _ = write!(out, "\x1b[2;1H");
            }
            if let Err(e) = write_kitty_frame(
                &mut out,
                IMAGE_ID,
                SCREEN_W as u32,
                SCREEN_H as u32,
                &self.rgba,
                self.place_cols,
            ) {
                eprintln!("rustpal: kitty frame error: {e}");
            }
            if self.frame_n == 1 {
                eprintln!(
                    "rustpal: first kitty frame sent ({} bytes raw, c={})",
                    self.rgba.len(),
                    self.place_cols
                );
            }
        } else {
            let scale = self.ansi_scale.max(1);
            let (fw, fh, frame) = if scale <= 1 {
                (SCREEN_W, SCREEN_H, self.rgba.as_slice())
            } else {
                nearest_upscale(&self.rgba, SCREEN_W, SCREEN_H, scale, &mut self.scaled);
                (
                    SCREEN_W * scale as usize,
                    SCREEN_H * scale as usize,
                    self.scaled.as_slice(),
                )
            };
            let _ = write_ansi_halfblock(&mut out, fw, fh, frame);
        }
        let _ = out.flush();
    }

    pub fn close_requested(&self) -> bool {
        self.close_requested
    }

    pub fn enable_enhanced_opening_menu(
        &mut self,
        _baseline: Vec<u8>,
        _palette: [PalColor; 256],
    ) {
    }

    pub fn disable_enhanced_background(&mut self) {}

    fn feed_byte(&mut self, b: u8, now: Instant) {
        // With ISIG enabled, Ctrl-C is usually SIGINT (process dies). If we
        // still see 0x03 (e.g. ISIG off), treat it as quit.
        if b == 0x03 || b == 0x04 {
            self.close_requested = true;
            return;
        }
        if !self.esc_buf.is_empty() || b == 0x1b {
            self.esc_buf.push(b);
            if self.esc_buf == [0x1b] {
                self.esc_solo_since = Some(now);
            } else {
                self.esc_solo_since = None;
            }
            if let Some(code) = parse_escape(&mut self.esc_buf) {
                self.esc_solo_since = None;
                self.note_press(code, now);
            }
            return;
        }
        if let Some(code) = map_ascii(b) {
            self.note_press(code, now);
        }
    }

    fn note_press(&mut self, code: KeyCode, now: Instant) {
        let until = now + KEY_HOLD;
        if let Some(h) = self.held.iter_mut().find(|h| h.code == code) {
            h.until = until;
            return;
        }
        self.held.push(HeldKey { code, until });
        self.pending.push((code, true));
    }
}

impl Drop for ConsoleVideo {
    fn drop(&mut self) {
        restore_terminal_ui();
        // TtyRawGuard also restores termios on drop.
    }
}

// --- input thread ------------------------------------------------------------

#[cfg(unix)]
fn spawn_tty_reader() -> io::Result<(Receiver<u8>, TtyRawGuard)> {
    use std::fs::OpenOptions;
    use std::os::fd::{AsRawFd, IntoRawFd};

    // Prefer /dev/tty so input works even if stdin is redirected.
    let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    let fd = tty.as_raw_fd();

    let orig = unsafe {
        let mut old: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut old) != 0 {
            return Err(io::Error::last_os_error());
        }
        // Stash for the SIGINT handler (must restore even if Drop never runs).
        SAVED_TERMIOS = old;
        TERMIOS_SAVED.store(true, Ordering::SeqCst);
        TTY_FD.store(fd, Ordering::SeqCst);

        let mut raw = old;
        libc::cfmakeraw(&mut raw);
        // Keep ISIG so Ctrl-C raises SIGINT (always works, even if main is busy).
        raw.c_lflag |= libc::ISIG;
        // Blocking reads in the worker thread (one byte at a time).
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
            return Err(io::Error::last_os_error());
        }
        old
    };

    let guard = TtyRawGuard { fd, orig };
    // Leak the File into the thread via raw fd duplicate for reading.
    let read_fd = unsafe { libc::dup(fd) };
    if read_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Prevent double-close of guard's fd ownership confusion: guard only does tcsetattr.
    // The OpenOptions File still owns fd; keep it alive by forgetting after dup.
    let _tty_fd = tty.into_raw_fd(); // ownership transferred; we won't close (process exit cleans)
    let _ = _tty_fd;

    let (tx, rx) = mpsc::channel::<u8>();
    thread::Builder::new()
        .name("rustpal-tty".into())
        .spawn(move || {
            let mut buf = [0u8; 64];
            loop {
                let n = unsafe {
                    libc::read(read_fd, buf.as_mut_ptr() as *mut _, buf.len())
                };
                if n <= 0 {
                    break;
                }
                for &b in &buf[..n as usize] {
                    if tx.send(b).is_err() {
                        break;
                    }
                }
            }
            unsafe {
                let _ = libc::close(read_fd);
            }
        })
        .map_err(|e| io::Error::other(e))?;

    Ok((rx, guard))
}

#[cfg(not(unix))]
fn spawn_stdin_reader() -> Receiver<u8> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 1];
        while stdin.read(&mut buf).ok() == Some(1) {
            if tx.send(buf[0]).is_err() {
                break;
            }
        }
    });
    rx
}

// --- key mapping -------------------------------------------------------------

fn map_ascii(b: u8) -> Option<KeyCode> {
    Some(match b {
        b'\r' | b'\n' => KeyCode::Enter,
        b' ' => KeyCode::Space,
        0x7f | 0x08 => KeyCode::Escape,
        b'r' | b'R' => KeyCode::KeyR,
        b'a' | b'A' => KeyCode::KeyA,
        b'd' | b'D' => KeyCode::KeyD,
        b'e' | b'E' => KeyCode::KeyE,
        b'w' | b'W' => KeyCode::KeyW,
        b'q' | b'Q' => KeyCode::KeyQ,
        b'f' | b'F' => KeyCode::KeyF,
        b's' | b'S' => KeyCode::KeyS,
        b'h' => KeyCode::ArrowLeft,
        b'j' => KeyCode::ArrowDown,
        b'k' => KeyCode::ArrowUp,
        b'l' => KeyCode::ArrowRight,
        _ => return None,
    })
}

fn parse_escape(buf: &mut Vec<u8>) -> Option<KeyCode> {
    if buf.is_empty() || buf.len() == 1 {
        return None;
    }
    if buf[0] != 0x1b {
        buf.clear();
        return None;
    }
    if buf.len() == 2 && buf[1] != b'[' && buf[1] != b'O' {
        buf.clear();
        return Some(KeyCode::Escape);
    }
    if buf.get(1) == Some(&b'[') {
        if let Some(&last) = buf.last() {
            if (0x40..=0x7e).contains(&last) && buf.len() >= 3 {
                let code = match last {
                    b'A' => Some(KeyCode::ArrowUp),
                    b'B' => Some(KeyCode::ArrowDown),
                    b'C' => Some(KeyCode::ArrowRight),
                    b'D' => Some(KeyCode::ArrowLeft),
                    b'H' => Some(KeyCode::Home),
                    b'F' => Some(KeyCode::End),
                    b'~' => match &buf[2..buf.len() - 1] {
                        b"5" => Some(KeyCode::PageUp),
                        b"6" => Some(KeyCode::PageDown),
                        b"2" => Some(KeyCode::Insert),
                        b"1" => Some(KeyCode::Home),
                        b"4" => Some(KeyCode::End),
                        _ => None,
                    },
                    _ => None,
                };
                buf.clear();
                return code;
            }
        }
        if buf.len() > 16 {
            buf.clear();
        }
        return None;
    }
    if buf.get(1) == Some(&b'O') && buf.len() >= 3 {
        let code = match buf[2] {
            b'A' => Some(KeyCode::ArrowUp),
            b'B' => Some(KeyCode::ArrowDown),
            b'C' => Some(KeyCode::ArrowRight),
            b'D' => Some(KeyCode::ArrowLeft),
            _ => None,
        };
        buf.clear();
        return code;
    }
    if buf.len() > 8 {
        buf.clear();
    }
    None
}

fn detect_kitty() -> bool {
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return true;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    let prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
    term.contains("kitty")
        || prog.eq_ignore_ascii_case("ghostty")
        || prog.eq_ignore_ascii_case("WezTerm")
        || prog.eq_ignore_ascii_case("iTerm.app")
}

/// Returns (kitty place_cols, ansi_scale).
fn resolve_display_size(use_kitty: bool) -> (u32, u32) {
    let (cols, rows) = terminal_size().unwrap_or((100, 30));
    let manual = std::env::var("RUSTPAL_CONSOLE_SCALE")
        .ok()
        .and_then(|s| s.parse::<u32>().ok());

    if use_kitty {
        // `c=` columns: fill most of the terminal width. Manual scale N means
        // roughly N*40 cells (so 4 ≈ 160 cols, 6 ≈ 240).
        let place = if let Some(n) = manual {
            (n.clamp(1, 16) * 40).min(cols.saturating_sub(2).max(20))
        } else {
            cols.saturating_sub(2).clamp(40, 240)
        };
        // Also leave room vertically: 320:200 → cells tall ≈ place * 200/320
        // with ~half-width cells; Kitty preserves image aspect when only c= set.
        let _ = rows;
        (place, 1)
    } else {
        let scale = if let Some(n) = manual {
            n.clamp(1, 3)
        } else {
            let max_cols = cols.saturating_sub(2).max(1);
            let max_rows = rows.saturating_sub(3).max(1);
            let sx = max_cols / SCREEN_W as u32;
            let sy = max_rows / ((SCREEN_H as u32) / 2);
            sx.min(sy).clamp(1, 3)
        };
        (cols, scale)
    }
}

fn terminal_size() -> Option<(u32, u32)> {
    #[cfg(unix)]
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0
            && ws.ws_col > 0
            && ws.ws_row > 0
        {
            return Some((ws.ws_col as u32, ws.ws_row as u32));
        }
    }
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())?;
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    Some((cols, rows))
}

fn nearest_upscale(src: &[u8], sw: usize, sh: usize, scale: u32, dst: &mut [u8]) {
    let scale = scale as usize;
    let dw = sw * scale;
    assert_eq!(src.len(), sw * sh * 4);
    assert!(dst.len() >= dw * sh * scale * 4);
    for y in 0..sh {
        for sy in 0..scale {
            let dy = y * scale + sy;
            for x in 0..sw {
                let si = (y * sw + x) * 4;
                let px = [src[si], src[si + 1], src[si + 2], src[si + 3]];
                for sx in 0..scale {
                    let di = (dy * dw + x * scale + sx) * 4;
                    dst[di..di + 4].copy_from_slice(&px);
                }
            }
        }
    }
}

// --- Kitty -------------------------------------------------------------------

fn write_kitty_frame(
    out: &mut impl Write,
    image_id: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
    place_cols: u32,
) -> io::Result<()> {
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    // Fast compression level — present must stay snappy so pump() keeps running.
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(rgba, 1);
    let payload = BASE64.encode(&compressed);
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(KITTY_CHUNK).collect();
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i != last);
        if i == 0 {
            // c=N → display over N columns; Kitty computes rows to keep aspect.
            // C=1 → do not move cursor after the image (no scroll flicker).
            write!(
                out,
                "\x1b_Ga=T,f=32,o=z,s={width},v={height},t=d,i={image_id},p=1,c={place_cols},C=1,q=2,m={more};"
            )?;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        out.write_all(chunk)?;
        write!(out, "\x1b\\")?;
    }
    Ok(())
}

// --- ANSI --------------------------------------------------------------------

fn write_ansi_halfblock(
    out: &mut impl Write,
    width: usize,
    height: usize,
    rgba: &[u8],
) -> io::Result<()> {
    let rows = height / 2;
    for cy in 0..rows {
        let y0 = cy * 2;
        let y1 = y0 + 1;
        write!(out, "\x1b[{};1H", cy + 2)?;
        for x in 0..width {
            let t = pixel(rgba, width, x, y0);
            let b = pixel(rgba, width, x, y1);
            write!(
                out,
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▄",
                b[0], b[1], b[2], t[0], t[1], t[2]
            )?;
        }
        write!(out, "\x1b[0m")?;
    }
    Ok(())
}

fn pixel(rgba: &[u8], width: usize, x: usize, y: usize) -> [u8; 3] {
    let o = (y * width + x) * 4;
    [rgba[o], rgba[o + 1], rgba[o + 2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_header_has_columns() {
        let px = [255u8, 0, 0, 255];
        let mut out = Vec::new();
        write_kitty_frame(&mut out, 1, 1, 1, &px, 80).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("c=80"), "{s}");
        assert!(s.contains("a=T"));
    }

    #[test]
    fn parse_arrow_csi() {
        let mut buf = vec![0x1b, b'[', b'A'];
        assert_eq!(parse_escape(&mut buf), Some(KeyCode::ArrowUp));
    }

    #[test]
    fn nearest_upscale_2x() {
        let src = [
            255, 0, 0, 255, 0, 0, 255, 255, 0, 255, 0, 255, 255, 255, 0, 255,
        ];
        let mut dst = vec![0u8; 4 * 4 * 4];
        nearest_upscale(&src, 2, 2, 2, &mut dst);
        assert_eq!(&dst[0..4], &[255, 0, 0, 255]);
    }
}
