//! Pure terminal video backend (no window, no system graphics/audio libs).
//!
//! Two presentation paths:
//!   * **Kitty graphics protocol** — true 320×200 RGBA pixels (Kitty, Ghostty,
//!     recent WezTerm, …). Transmit path follows zenbu-labs/terminal-browser:
//!     zlib + base64 + APC chunks (`\x1b_G…\x1b\\`).
//!   * **ANSI half-block fallback** — truecolor `▄` cells for ordinary terminals.
//!
//! Input: raw stdin (termios on Unix), mapped to engine `KeyCode`s. Terminals
//! only deliver "press" events, so each press is immediately followed by a
//! synthetic release so `InputState` does not stick.

use std::io::{self, Write};
#[cfg(not(unix))]
use std::io::Read;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use crate::game_loop::{render_rgba, PalColor};
use crate::keys::KeyCode;
use crate::surface::{Surface, SCREEN_H, SCREEN_W};

const KITTY_CHUNK: usize = 4096;
const IMAGE_ID: u32 = 1;
/// Cap present rate so SSH / slow terminals stay usable (~20 fps).
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleMode {
    /// Auto-detect Kitty via env, else ANSI.
    Auto,
    Kitty,
    Ansi,
}

pub struct ConsoleVideo {
    use_kitty: bool,
    /// Integer nearest-neighbor scale (1 = native 320×200). Auto-fit by default.
    scale: u32,
    rgba: Vec<u8>,
    /// Upscaled RGBA when scale > 1 (reused each frame).
    scaled: Vec<u8>,
    last_present: Instant,
    close_requested: bool,
    #[cfg(unix)]
    orig_termios: Option<libc::termios>,
    /// Incomplete CSI sequence bytes.
    esc_buf: Vec<u8>,
    /// Synthetic (key, pressed) events waiting to be returned from `pump`.
    pending: Vec<(KeyCode, bool)>,
}

impl ConsoleVideo {
    pub fn new(mode: ConsoleMode) -> io::Result<ConsoleVideo> {
        let use_kitty = match mode {
            ConsoleMode::Kitty => true,
            ConsoleMode::Ansi => false,
            ConsoleMode::Auto => detect_kitty(),
        };
        let scale = resolve_scale(use_kitty);

        #[cfg(unix)]
        let orig_termios = Some(enter_raw_mode()?);

        let mut out = io::stdout();
        // Alternate screen buffer, hide cursor, clear.
        write!(out, "\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H")?;
        let label = if use_kitty {
            "Kitty pixels"
        } else {
            "ANSI half-block"
        };
        let dw = SCREEN_W as u32 * scale;
        let dh = SCREEN_H as u32 * scale;
        writeln!(
            out,
            "\x1b[36mrustpal console ({label} · {scale}× → {dw}×{dh}) — arrows/hjkl · Enter · Esc · Ctrl-C · RUSTPAL_CONSOLE_SCALE=N\x1b[0m"
        )?;
        out.flush()?;

        let scaled_len = (SCREEN_W * SCREEN_H * 4) * (scale as usize) * (scale as usize);
        Ok(ConsoleVideo {
            use_kitty,
            scale,
            rgba: vec![0; SCREEN_W * SCREEN_H * 4],
            scaled: vec![0; scaled_len],
            last_present: Instant::now() - MIN_FRAME_INTERVAL,
            close_requested: false,
            #[cfg(unix)]
            orig_termios,
            esc_buf: Vec::new(),
            pending: Vec::new(),
        })
    }

    pub fn pump(&mut self) -> Vec<(KeyCode, bool)> {
        // Drain stdin (non-blocking).
        loop {
            let mut byte = [0u8; 1];
            match read_stdin_nonblock(&mut byte) {
                Ok(0) => break,
                Ok(_) => self.feed_byte(byte[0]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        // Bare Esc (no CSI follow-up yet) → menu key.
        if self.esc_buf == [0x1b] {
            self.esc_buf.clear();
            self.push_tap(KeyCode::Escape);
        }
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
        self.last_present = now;
        render_rgba(surf, palette, shake, &mut self.rgba);

        let (fw, fh, frame) = if self.scale <= 1 {
            (SCREEN_W as u32, SCREEN_H as u32, self.rgba.as_slice())
        } else {
            nearest_upscale(
                &self.rgba,
                SCREEN_W,
                SCREEN_H,
                self.scale,
                &mut self.scaled,
            );
            (
                SCREEN_W as u32 * self.scale,
                SCREEN_H as u32 * self.scale,
                self.scaled.as_slice(),
            )
        };

        let mut out = io::stdout();
        let _ = write!(out, "\x1b[H"); // cursor home
        // Skip the banner line so the image sits at the top.
        let _ = write!(out, "\x1b[2;1H");
        if self.use_kitty {
            let _ = write_kitty_frame(&mut out, IMAGE_ID, fw, fh, frame);
        } else {
            let _ = write_ansi_halfblock(&mut out, fw as usize, fh as usize, frame);
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

    fn feed_byte(&mut self, b: u8) {
        if b == 0x03 || b == 0x04 {
            // Ctrl-C / Ctrl-D
            self.close_requested = true;
            return;
        }
        if !self.esc_buf.is_empty() || b == 0x1b {
            self.esc_buf.push(b);
            if let Some(code) = parse_escape(&mut self.esc_buf) {
                self.push_tap(code);
            }
            return;
        }
        if let Some(code) = map_ascii(b) {
            self.push_tap(code);
        }
    }

    fn push_tap(&mut self, code: KeyCode) {
        self.pending.push((code, true));
        self.pending.push((code, false));
    }
}

impl Drop for ConsoleVideo {
    fn drop(&mut self) {
        let mut out = io::stdout();
        // Delete kitty image, show cursor, leave alt screen.
        let _ = write!(out, "\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        #[cfg(unix)]
        if let Some(t) = self.orig_termios.take() {
            let _ = restore_termios(&t);
        }
    }
}

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

/// Parse CSI / SS3 arrow keys and Esc. Returns a key when the sequence is
/// complete; leaves incomplete sequences in `buf`.
fn parse_escape(buf: &mut Vec<u8>) -> Option<KeyCode> {
    if buf.is_empty() {
        return None;
    }
    // Lone Esc after a short wait is hard without a timer; treat Esc + non-[
    // as Esc, and Esc [ … letter as CSI.
    if buf.len() == 1 {
        return None; // wait for more (or next byte)
    }
    if buf[0] != 0x1b {
        buf.clear();
        return None;
    }
    // ESC alone followed by a non-CSI introducer → plain Escape
    if buf.len() == 2 && buf[1] != b'[' && buf[1] != b'O' {
        buf.clear();
        return Some(KeyCode::Escape);
    }
    // ESC [ … final byte (0x40–0x7E)
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
                        _ => None,
                    },
                    _ => None,
                };
                buf.clear();
                return code;
            }
        }
        if buf.len() > 16 {
            buf.clear(); // garbage
        }
        return None;
    }
    // ESC O A/B/C/D (SS3 — application cursor keys)
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
        || std::env::var("TERM_PROGRAM")
            .map(|p| p.to_ascii_lowercase().contains("vscode"))
            .unwrap_or(false)
}

/// Integer scale for console output.
///
/// Order: `RUSTPAL_CONSOLE_SCALE` (1–16) → auto-fit terminal size → default 4 (Kitty) / 2 (ANSI).
fn resolve_scale(use_kitty: bool) -> u32 {
    if let Ok(s) = std::env::var("RUSTPAL_CONSOLE_SCALE") {
        if let Ok(n) = s.parse::<u32>() {
            return n.clamp(1, 16);
        }
    }
    let (cols, rows) = terminal_size().unwrap_or((100, 30));
    if use_kitty {
        // Kitty draws 1 image pixel ≈ 1 device pixel. Cell size varies; use a
        // conservative ~8×16 so we rarely overflow the window on Retina Macs.
        let max_w = cols.saturating_sub(2).saturating_mul(8).max(SCREEN_W as u32);
        let max_h = rows.saturating_sub(3).saturating_mul(16).max(SCREEN_H as u32);
        let sx = max_w / SCREEN_W as u32;
        let sy = max_h / SCREEN_H as u32;
        sx.min(sy).clamp(2, 12) // at least 2× so 320×200 is not postage-stamp size
    } else {
        // ANSI: each source pixel is one cell wide; two pixels tall → one cell.
        // After scale S: cells = (320S) × (100S).
        let max_cols = cols.saturating_sub(2).max(1);
        let max_rows = rows.saturating_sub(3).max(1);
        let sx = max_cols / SCREEN_W as u32;
        let sy = max_rows / ((SCREEN_H as u32) / 2);
        sx.min(sy).clamp(1, 4).max(1)
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
    // Fallback env (some SSH setups).
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())?;
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    Some((cols, rows))
}

/// Nearest-neighbor integer upscale of RGBA8.
fn nearest_upscale(src: &[u8], sw: usize, sh: usize, scale: u32, dst: &mut [u8]) {
    let scale = scale as usize;
    let dw = sw * scale;
    let dh = sh * scale;
    assert_eq!(src.len(), sw * sh * 4);
    assert!(dst.len() >= dw * dh * 4);
    for y in 0..sh {
        for sy in 0..scale {
            let dy = y * scale + sy;
            for x in 0..sw {
                let si = (y * sw + x) * 4;
                let px = [src[si], src[si + 1], src[si + 2], src[si + 3]];
                for sx in 0..scale {
                    let di = (dy * dw + x * scale + sx) * 4;
                    dst[di] = px[0];
                    dst[di + 1] = px[1];
                    dst[di + 2] = px[2];
                    dst[di + 3] = px[3];
                }
            }
        }
    }
}

// --- Kitty graphics protocol -------------------------------------------------

fn write_kitty_frame(
    out: &mut impl Write,
    image_id: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> io::Result<()> {
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(rgba, 1);
    let payload = BASE64.encode(&compressed);
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(KITTY_CHUNK).collect();
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i != last);
        if i == 0 {
            // a=T transmit+display, f=32 RGBA, o=z zlib, t=d direct, C=1 keep cursor
            write!(
                out,
                "\x1b_Ga=T,f=32,o=z,s={width},v={height},t=d,i={image_id},p=1,C=1,q=2,m={more};"
            )?;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        out.write_all(chunk)?;
        write!(out, "\x1b\\")?;
    }
    Ok(())
}

// --- ANSI half-block fallback ------------------------------------------------

fn write_ansi_halfblock(
    out: &mut impl Write,
    width: usize,
    height: usize,
    rgba: &[u8],
) -> io::Result<()> {
    // Pair rows into `▄` (lower half = bottom pixel, upper = top via bg/fg).
    // Optionally scale down if the terminal is small — keep 1:1 for simplicity
    // (320 cols × 100 rows of cells); user can zoom the terminal font.
    let rows = height / 2;
    for cy in 0..rows {
        let y0 = cy * 2;
        let y1 = y0 + 1;
        for x in 0..width {
            let t = pixel(rgba, width, x, y0);
            let b = pixel(rgba, width, x, y1);
            // fg = bottom, bg = top, char = lower half block
            write!(
                out,
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▄",
                b[0], b[1], b[2], t[0], t[1], t[2]
            )?;
        }
        write!(out, "\x1b[0m\r\n")?;
    }
    Ok(())
}

fn pixel(rgba: &[u8], width: usize, x: usize, y: usize) -> [u8; 3] {
    let o = (y * width + x) * 4;
    [rgba[o], rgba[o + 1], rgba[o + 2]]
}


// --- termios raw mode (Unix) -------------------------------------------------

#[cfg(unix)]
fn enter_raw_mode() -> io::Result<libc::termios> {
    unsafe {
        let fd = libc::STDIN_FILENO;
        let mut old: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut old) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = old;
        libc::cfmakeraw(&mut raw);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(old)
    }
}

#[cfg(unix)]
fn restore_termios(t: &libc::termios) -> io::Result<()> {
    unsafe {
        if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, t) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_stdin_nonblock(buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut _, buf.len()) };
    if n < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EAGAIN) || err.raw_os_error() == Some(libc::EWOULDBLOCK)
        {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, err));
        }
        return Err(err);
    }
    Ok(n as usize)
}

#[cfg(not(unix))]
fn read_stdin_nonblock(buf: &mut [u8]) -> io::Result<usize> {
    let n = io::stdin().read(buf)?;
    if n == 0 {
        Err(io::Error::new(io::ErrorKind::WouldBlock, "eof"))
    } else {
        Ok(n)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_transmit_roundtrip_header() {
        let px = [255u8, 0, 0, 255];
        let mut out = Vec::new();
        write_kitty_frame(&mut out, 1, 1, 1, &px).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\u{1b}_Ga=T,f=32,o=z,s=1,v=1,t=d,i=1,"));
        assert!(s.ends_with("\u{1b}\\"));
    }

    #[test]
    fn parse_arrow_csi() {
        let mut buf = vec![0x1b, b'[', b'A'];
        assert_eq!(parse_escape(&mut buf), Some(KeyCode::ArrowUp));
        assert!(buf.is_empty());
    }

    #[test]
    fn nearest_upscale_2x() {
        // 2×2 red/blue pattern → 4×4
        let src = [
            255, 0, 0, 255, 0, 0, 255, 255, // row0
            0, 255, 0, 255, 255, 255, 0, 255, // row1
        ];
        let mut dst = vec![0u8; 4 * 4 * 4];
        nearest_upscale(&src, 2, 2, 2, &mut dst);
        // top-left 2×2 block (out rows 0–1) should be red
        assert_eq!(&dst[0..4], &[255, 0, 0, 255]);
        assert_eq!(&dst[4..8], &[255, 0, 0, 255]);
        let row1 = 4 * 4; // second output row, first pixel
        assert_eq!(&dst[row1..row1 + 4], &[255, 0, 0, 255]);
        // out row 2 comes from source y=1 → green
        let row2 = 2 * 4 * 4;
        assert_eq!(&dst[row2..row2 + 4], &[0, 255, 0, 255]);
    }
}
