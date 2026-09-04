//! Pure terminal video backend (no window, no system graphics/audio libs).
//!
//! * **Kitty** — upscale the logical 320×200 frame, then transmit; on-screen
//!   size uses protocol `c=` (columns). Upscale mode via
//!   `RUSTPAL_CONSOLE_UPSCALE`:
//!   * `nn` (default) — nearest-neighbor; scale pixel-aligned to cell size
//!     (`RUSTPAL_CONSOLE_KITTY_NN=1..8` overrides).
//!   * `hqx4` / `xbr4` — CPU HQ4x (two HQ2x passes) → 1280×800.
//!   * `neural` — same GPU mega-kernel as the GUI (`OfflineUpscaler` readback; `neural` feature);
//!     needs the `neural` feature (in `gui` or `console-neural`) + `SHADER_F16` adapter; falls back to `hqx4`.
//! * **ANSI** — half-block truecolor cells, modest integer upscale.
//!
//! Input is read on a **background thread** from `/dev/tty` so keys (and a
//! stuck main thread) still work. Ctrl-C is handled by a signal handler that
//! **restores the terminal** (leave alt screen, re-enable echo) before exit —
//! a bare SIGINT kill would leave Kitty stuck on the game buffer.
//! Compatible terminals report real functional-key press/repeat/release events;
//! legacy input is delivered as frame-latched taps so timing cannot create
//! duplicate movement steps.
//!
//! After the alternate screen is entered, **stderr is redirected** away from
//! the tty (to `RUSTPAL_CONSOLE_LOG` or `/dev/null`) so `eprintln!` from the
//! engine / autoplay pilot cannot scroll the game image. Set
//! `RUSTPAL_CONSOLE_VERBOSE=1` to keep stderr on the terminal (debug only).

use std::io::{self, Write};
#[cfg(not(unix))]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::IntoRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
#[cfg(feature = "neural")]
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{self, Receiver, TryRecvError};
#[cfg(feature = "neural")]
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use crate::game_loop::{render_rgba, PalColor};
use crate::keys::{KeyCode, KeyEvent};
use crate::surface::{Surface, SCREEN_H, SCREEN_W};

const KITTY_CHUNK: usize = 4096;
const IMAGE_ID: u32 = 1;
/// Local Kitty: ~14 fps. Over SSH we slow down further (see `frame_interval()`).
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(70);
/// SSH has higher latency; fewer full-frame APC blasts = less white flash.
const MIN_FRAME_INTERVAL_SSH: Duration = Duration::from_millis(100);
/// Neural is async: only used for warmup / rare fallback emits.
const MIN_FRAME_INTERVAL_NEURAL: Duration = Duration::from_millis(50);
const MIN_FRAME_INTERVAL_NEURAL_SSH: Duration = Duration::from_millis(80);
/// How often to push a new 320×200 job to the neural worker (~12 fps max).
const NEURAL_SUBMIT_INTERVAL: Duration = Duration::from_millis(80);
/// FPS text-only refresh when the image is unchanged.
const NEURAL_FPS_BANNER_INTERVAL: Duration = Duration::from_millis(500);
/// Ask compatible terminals to report repeat/release for functional keys.
/// Requesting only the event-types flag preserves legacy Ctrl-C/ISIG and
/// plain-text key encoding.
/// Kitty keyboard protocol flags: 1 = disambiguate escape codes (bare Esc
/// arrives as `CSI 27 u`, no timeout needed), 2 = report event types
/// (press/repeat/release), 8 = report all keys as escape codes (Enter, Space,
/// hjkl, WASD get real key-up too instead of legacy text bytes).
const KEYBOARD_PUSH_EVENT_TYPES: &[u8] = b"\x1b[>11u";
/// XTerm focus-in/out reporting (`CSI I` / `CSI O`), so a key released while
/// another window has focus does not stay held in the game.
const FOCUS_EVENTS_ON: &[u8] = b"\x1b[?1004h";

/// Pre-Kitty upscale filter (`RUSTPAL_CONSOLE_UPSCALE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsoleUpscale {
    /// Integer nearest-neighbor (scale from layout / env).
    Nearest,
    /// CPU HQ4x → 1280×800.
    Hqx4,
    /// GPU neural 4× → 1280×800 (`neural` feature + F16 GPU).
    Neural,
}

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

pub struct ConsoleVideo {
    use_kitty: bool,
    /// True when `SSH_CONNECTION` / `SSH_TTY` is set — use flicker-resistant path.
    over_ssh: bool,
    /// Pre-transmit filter (nn / hqx4 / neural).
    upscale: ConsoleUpscale,
    /// Kitty: display width in terminal cells (`c=`). Aspect preserved by Kitty.
    place_cols: u32,
    /// Kitty: integer nearest-neighbor scale when `upscale == Nearest` (1 = raw
    /// 320×200). HQX/neural always emit 1280×800 (logical 4×).
    kitty_nn_scale: u32,
    /// ANSI only: integer nearest-neighbor scale (1–3).
    ansi_scale: u32,
    /// 1-based terminal row where the game image starts. Row 1 is full-bleed;
    /// row 2 leaves the top line free for the optional FPS banner.
    image_row: u32,
    rgba: Vec<u8>,
    scaled: Vec<u8>,
    /// Mid buffer for HQ4x (640×400).
    hqx_tmp: Vec<u8>,
    prev_rgba: Vec<u8>,
    last_present: Instant,
    close_requested: bool,
    frame_n: u64,
    /// Show FPS on the status line when `RUSTPAL_CONSOLE_FPS` / `RUSTPAL_SHOW_FPS` is set.
    show_fps: bool,
    fps_window_start: Instant,
    fps_frames: u32,
    /// Smoothed FPS written to the banner (~1 Hz update).
    fps_value: f32,
    /// Bytes from the input thread.
    key_rx: Receiver<u8>,
    esc_buf: Vec<u8>,
    esc_solo_since: Option<Instant>,
    pending: Vec<KeyEvent>,
    /// Optional HTTP control API (`RUSTPAL_UI_DRIVER` / `--ui-driver`).
    ui_driver: Option<crate::ui_driver::UiDriver>,
    /// Async GPU neural path (`neural` feature). Worker thread owns OfflineUpscaler.
    #[cfg(feature = "neural")]
    neural: Option<NeuralAsync>,
    /// Last time we submitted a 320×200 job to the neural worker.
    last_neural_submit: Instant,
    /// Last time we rewrote the FPS banner without a new image.
    last_fps_banner: Instant,
    /// Restores original stderr when the console backend ends.
    #[cfg(unix)]
    _stderr_guard: Option<StderrRedirect>,
    #[cfg(unix)]
    _tty_guard: TtyRawGuard,
}

/// Restores termios on `/dev/tty` when dropped.
#[cfg(unix)]
struct TtyRawGuard {
    fd: i32,
    orig: libc::termios,
}

/// Redirects `stderr` to a file or `/dev/null` so log lines cannot scroll the
/// alternate-screen framebuffer. Restored on drop.
#[cfg(unix)]
struct StderrRedirect {
    saved_fd: i32,
}

#[cfg(unix)]
impl StderrRedirect {
    fn install() -> io::Result<Option<Self>> {
        // Opt out for debugging (accepts that logs may jump the picture).
        if env_flag_enabled("RUSTPAL_CONSOLE_VERBOSE") {
            return Ok(None);
        }
        let target = match std::env::var("RUSTPAL_CONSOLE_LOG") {
            Ok(path) if !path.is_empty() => path,
            _ => "/dev/null".to_string(),
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)?;
        let saved_fd = unsafe { libc::dup(libc::STDERR_FILENO) };
        if saved_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let new_fd = file.into_raw_fd();
        let rc = unsafe { libc::dup2(new_fd, libc::STDERR_FILENO) };
        unsafe {
            let _ = libc::close(new_fd);
        }
        if rc < 0 {
            let err = io::Error::last_os_error();
            unsafe {
                let _ = libc::close(saved_fd);
            }
            return Err(err);
        }
        Ok(Some(Self { saved_fd }))
    }
}

#[cfg(unix)]
impl Drop for StderrRedirect {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::dup2(self.saved_fd, libc::STDERR_FILENO);
            let _ = libc::close(self.saved_fd);
        }
    }
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
    // Pop while still on the alternate screen. Send this sequence exactly
    // once: a second pop after `?1049l` would affect the main-screen stack.
    let seq = b"\x1b[?1004l\x1b[<u\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?25h\x1b[?1049l\r\n";
    #[cfg(unix)]
    {
        let fd = TTY_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            unsafe {
                let _ = libc::write(fd, seq.as_ptr() as *const _, seq.len());
            }
        } else {
            let _ = io::stdout().write_all(seq);
            let _ = io::stdout().flush();
        }
        if TERMIOS_SAVED.load(Ordering::SeqCst) {
            let tfd = if fd >= 0 { fd } else { libc::STDIN_FILENO };
            unsafe {
                let _ = libc::tcsetattr(tfd, libc::TCSANOW, std::ptr::addr_of!(SAVED_TERMIOS));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = io::stdout().write_all(seq);
        let _ = io::stdout().flush();
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
    let seq = b"\x1b[?1004l\x1b[<u\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?25h\x1b[?1049l\r\n";
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
        let over_ssh = is_over_ssh();
        let show_fps = env_flag_enabled("RUSTPAL_CONSOLE_FPS")
            || env_flag_enabled("RUSTPAL_SHOW_FPS");
        // Keep the top terminal row free only when the FPS banner is on.
        // Otherwise place the game full-bleed from row 1 so a default-wide
        // Kitty image is not clipped at the bottom by a one-line help bar.
        let image_row = if show_fps { 2 } else { 1 };
        let reserve_top = u32::from(show_fps);

        let mut upscale = resolve_console_upscale();
        #[cfg(not(feature = "neural"))]
        if upscale == ConsoleUpscale::Neural {
            eprintln!(
                "rustpal: console neural needs the `neural` feature (build with --features console-neural); falling back to hqx4"
            );
            upscale = ConsoleUpscale::Hqx4;
        }

        // Layout before starting the neural worker so it can pre-encode Kitty
        // payloads with the final `c=` (place_cols).
        let layout = resolve_console_layout(use_kitty, over_ssh, reserve_top, upscale);
        let place_cols = layout.place_cols;
        let kitty_nn_scale = layout.kitty_nn_scale;
        let ansi_scale = layout.ansi_scale;

        #[cfg(feature = "neural")]
        let mut neural = None;
        #[cfg(feature = "neural")]
        {
            if upscale == ConsoleUpscale::Neural {
                match NeuralAsync::start(place_cols) {
                    Some(n) => {
                        eprintln!(
                            "rustpal: console neural async (GPU {} + zlib/Kitty on worker) — place_cols={place_cols}",
                            n.adapter_name()
                        );
                        neural = Some(n);
                    }
                    None => {
                        eprintln!(
                            "rustpal: console neural unavailable (need SHADER_F16 GPU); falling back to hqx4"
                        );
                        upscale = ConsoleUpscale::Hqx4;
                        // place_cols already match 1280-wide path for hqx/neural.
                    }
                }
            }
        }

        #[cfg(unix)]
        let (key_rx, tty_guard) = spawn_tty_reader()?;
        #[cfg(not(unix))]
        let key_rx = spawn_stdin_reader();

        // HTTP driver + startup diagnostics go to the *main* screen (before
        // alt buffer). After alt screen, stderr is redirected so game/pilot
        // logs cannot scroll the picture.
        let ui_driver = crate::ui_driver::UiDriver::start_from_env()?;

        let label = if use_kitty { "Kitty" } else { "ANSI" };
        let filter_tag = match upscale {
            ConsoleUpscale::Nearest => "nn",
            ConsoleUpscale::Hqx4 => "hqx4",
            ConsoleUpscale::Neural => "neural",
        };
        let size_note = if use_kitty {
            let src_w = kitty_output_width(upscale, kitty_nn_scale);
            let src_h = kitty_output_height(upscale, kitty_nn_scale);
            match layout.cell {
                Some((cw, ch)) => {
                    let disp_w = place_cols * cw;
                    let align = if disp_w == src_w { "1:1" } else { "near" };
                    format!(
                        "{filter_tag} → {src_w}×{src_h}, {place_cols} cols ({disp_w}px, cell {cw}×{ch}, {align})"
                    )
                }
                None => format!("{filter_tag} → {src_w}×{src_h}, {place_cols} cols wide"),
            }
        } else {
            format!(
                "{}× → {}×{}",
                ansi_scale,
                SCREEN_W as u32 * ansi_scale,
                SCREEN_H as u32 * ansi_scale
            )
        };
        let ssh_note = if over_ssh { " · SSH" } else { "" };
        let fps_note = if show_fps { " · FPS on" } else { "" };
        // Help/controls live on the primary screen only — printing them inside
        // the alt buffer permanently steals a row and clips a full-height frame.
        eprintln!(
            "rustpal: console backend ready ({label}, upscale={filter_tag}, place_cols={place_cols}, kitty_nn={kitty_nn_scale}, ansi_scale={ansi_scale}, ssh={over_ssh}, fps={show_fps})"
        );
        eprintln!(
            "rustpal console ({label} · {size_note}{ssh_note}{fps_note}) — arrows/hjkl · Enter · Esc · Ctrl-C restores terminal & quits"
        );
        if !env_flag_enabled("RUSTPAL_CONSOLE_VERBOSE") {
            match std::env::var("RUSTPAL_CONSOLE_LOG") {
                Ok(path) if !path.is_empty() => {
                    eprintln!("rustpal: console stderr → {path} (set RUSTPAL_CONSOLE_VERBOSE=1 to keep on tty)");
                }
                _ => {
                    eprintln!(
                        "rustpal: console stderr muted during play (RUSTPAL_CONSOLE_LOG=path or VERBOSE=1)"
                    );
                }
            }
        }

        let mut out = io::stdout();
        // Alternate screen once — no status line on row 1 unless FPS is enabled.
        write!(out, "\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H")?;
        // The keyboard-mode stack is screen-local, so push this only after
        // entering the alternate screen and pop it before leaving.
        out.write_all(KEYBOARD_PUSH_EVENT_TYPES)?;
        out.write_all(FOCUS_EVENTS_ON)?;
        out.flush()?;

        #[cfg(unix)]
        install_console_signal_handlers();

        // Mute stderr only after the alt screen is up (messages above stay visible
        // on the primary screen when the user leaves alt buffer).
        #[cfg(unix)]
        let stderr_guard = StderrRedirect::install()?;

        // Output buffer: NN uses scale²; HQX/neural always 1280×800.
        let scaled_len = if use_kitty {
            match upscale {
                ConsoleUpscale::Nearest => {
                    let s = kitty_nn_scale.max(1) as usize;
                    SCREEN_W * SCREEN_H * 4 * s * s
                }
                ConsoleUpscale::Hqx4 | ConsoleUpscale::Neural => 1280 * 800 * 4,
            }
        } else {
            (SCREEN_W * SCREEN_H * 4) * (ansi_scale as usize).pow(2)
        };
        let hqx_tmp_len = if upscale == ConsoleUpscale::Hqx4 {
            640 * 400 * 4
        } else {
            0
        };

        Ok(ConsoleVideo {
            use_kitty,
            over_ssh,
            upscale,
            place_cols,
            kitty_nn_scale,
            ansi_scale,
            image_row,
            rgba: vec![0; SCREEN_W * SCREEN_H * 4],
            scaled: vec![0; scaled_len],
            hqx_tmp: vec![0; hqx_tmp_len],
            prev_rgba: Vec::new(),
            last_present: Instant::now() - frame_interval(over_ssh, upscale),
            close_requested: false,
            frame_n: 0,
            show_fps,
            fps_window_start: Instant::now(),
            fps_frames: 0,
            fps_value: 0.0,
            key_rx,
            esc_buf: Vec::new(),
            esc_solo_since: None,
            pending: Vec::new(),
            ui_driver,
            #[cfg(feature = "neural")]
            neural,
            last_neural_submit: Instant::now() - NEURAL_SUBMIT_INTERVAL,
            last_fps_banner: Instant::now() - NEURAL_FPS_BANNER_INTERVAL,
            #[cfg(unix)]
            _stderr_guard: stderr_guard,
            #[cfg(unix)]
            _tty_guard: tty_guard,
        })
    }

    fn frame_interval(&self) -> Duration {
        frame_interval(self.over_ssh, self.upscale)
    }

    /// Upscale `self.rgba` into `self.scaled`; returns (width, height) of the result.
    /// Neural mode is handled in `present_neural` (async worker).
    fn kitty_upscale_into_scaled(&mut self) -> (u32, u32) {
        match self.upscale {
            ConsoleUpscale::Nearest => {
                let nn = self.kitty_nn_scale.max(1);
                if nn <= 1 {
                    let n = SCREEN_W * SCREEN_H * 4;
                    self.scaled[..n].copy_from_slice(&self.rgba[..n]);
                    (SCREEN_W as u32, SCREEN_H as u32)
                } else {
                    nearest_upscale(
                        &self.rgba,
                        SCREEN_W,
                        SCREEN_H,
                        nn,
                        &mut self.scaled,
                    );
                    (SCREEN_W as u32 * nn, SCREEN_H as u32 * nn)
                }
            }
            ConsoleUpscale::Hqx4 => {
                crate::hqx::hqx4(
                    &self.rgba,
                    SCREEN_W,
                    SCREEN_H,
                    &mut self.hqx_tmp,
                    &mut self.scaled,
                );
                (1280, 800)
            }
            ConsoleUpscale::Neural => {
                // Warmup / fallback only — live path uses NeuralAsync.
                nearest_upscale(&self.rgba, SCREEN_W, SCREEN_H, 4, &mut self.scaled);
                (1280, 800)
            }
        }
    }

    pub fn pump(&mut self) -> Vec<KeyEvent> {
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
                    self.pending.push(KeyEvent::Tap(KeyCode::Escape));
                }
                Some(_) => {}
            }
        } else if self.esc_buf.is_empty() {
            self.esc_solo_since = None;
        }

        let mut events = std::mem::take(&mut self.pending);
        // External scripts inject keys through the same path as the tty.
        if let Some(driver) = self.ui_driver.as_ref() {
            let mut injected = Vec::new();
            driver.drain_input(&mut injected);
            events.extend(injected.into_iter().map(|(code, pressed)| KeyEvent::State {
                code,
                pressed,
            }));
        }
        events
    }

    pub fn present(
        &mut self,
        surf: &Surface,
        palette: &[PalColor; 256],
        shake: Option<(u16, u16)>,
    ) {
        // Always publish the latest logical frame for HTTP clients, even when
        // terminal output is throttled or de-duplicated.
        if let Some(driver) = self.ui_driver.as_mut() {
            driver.capture(surf, palette, shake);
        }

        // Neural: never block the engine clock on GPU readback.
        if self.upscale == ConsoleUpscale::Neural {
            self.present_neural(surf, palette, shake);
            return;
        }

        let now = Instant::now();
        if now.duration_since(self.last_present) < self.frame_interval() {
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
        let first = self.frame_n == 1;

        self.emit_kitty_or_ansi(first, now);
    }

    /// Async neural path: worker does GPU + zlib/base64; main only rate-limited
    /// submit + write_all when a *new* payload is ready (no re-send of same frame).
    fn present_neural(
        &mut self,
        surf: &Surface,
        palette: &[PalColor; 256],
        shake: Option<(u16, u16)>,
    ) {
        let now = Instant::now();
        let force = self.frame_n < 3;

        // 1) Poll worker first (cheap) — may already have something to show.
        #[cfg(feature = "neural")]
        let (have_frame, is_new) = if let Some(ref mut neural) = self.neural {
            neural.poll_payload()
        } else {
            (false, false)
        };
        #[cfg(not(feature = "neural"))]
        let (have_frame, is_new) = (false, false);

        // 2) Submit at most ~12 fps. Skip render_rgba + 256KB compare on every
        //    engine present() (was the main CPU hog when present is called 100+/s).
        let due_submit = force
            || now.duration_since(self.last_neural_submit) >= NEURAL_SUBMIT_INTERVAL;
        if due_submit {
            render_rgba(surf, palette, shake, &mut self.rgba);
            self.last_neural_submit = now;
            #[cfg(feature = "neural")]
            if let Some(ref neural) = self.neural {
                neural.submit(&self.rgba);
            }
        }

        // 3) Warmup only: main-thread NN4× until first worker payload.
        if !have_frame {
            if !force && now.duration_since(self.last_present) < self.frame_interval() {
                return;
            }
            if !due_submit {
                render_rgba(surf, palette, shake, &mut self.rgba);
            }
            nearest_upscale(&self.rgba, SCREEN_W, SCREEN_H, 4, &mut self.scaled);
            self.last_present = now;
            self.frame_n = self.frame_n.saturating_add(1);
            let first = self.frame_n == 1;
            self.emit_kitty_or_ansi(first, now);
            return;
        }

        // 4) Only push a full Kitty image when the worker finished a *new* encode.
        //    Re-sending the same multi-MB APC every 50ms was burning CPU + terminal.
        if is_new || force {
            self.last_present = now;
            self.frame_n = self.frame_n.saturating_add(1);
            let first = self.frame_n == 1;
            #[cfg(feature = "neural")]
            {
                let payload = self.neural.as_ref().and_then(|n| n.payload_arc());
                if let Some(payload) = payload {
                    self.emit_neural_payload_bytes(payload.as_slice(), first, now);
                    return;
                }
            }
            self.emit_kitty_or_ansi(first, now);
            return;
        }

        // 5) Unchanged image: optional FPS text only (no image rewrite).
        if self.show_fps && now.duration_since(self.last_fps_banner) >= NEURAL_FPS_BANNER_INTERVAL
        {
            self.last_fps_banner = now;
            self.note_displayed_frame(now);
            let mut out = io::stdout();
            let _ = write!(
                out,
                "\x1b[1;1H\x1b[36mrustpal\x1b[0m  \x1b[33m{:>5.1} FPS\x1b[0m\x1b[K",
                self.fps_value
            );
            let _ = out.flush();
        }
    }

    /// Write a prebuilt Kitty APC (no zlib on main).
    #[cfg(feature = "neural")]
    fn emit_neural_payload_bytes(&mut self, payload: &[u8], first: bool, now: Instant) {
        let mut out = io::stdout();
        let use_sync = sync_output_enabled(self.over_ssh);
        if use_sync {
            let _ = write!(out, "\x1b[?2026h");
        }
        if first {
            let _ = write!(out, "\x1b[{};1H", self.image_row);
        }
        let _ = out.write_all(payload);
        if self.show_fps {
            self.note_displayed_frame(now);
            self.last_fps_banner = now;
            let _ = write!(
                out,
                "\x1b[1;1H\x1b[36mrustpal\x1b[0m  \x1b[33m{:>5.1} FPS\x1b[0m\x1b[K",
                self.fps_value
            );
        }
        if use_sync {
            let _ = write!(out, "\x1b[?2026l");
        }
        let _ = out.flush();
    }

    fn emit_kitty_or_ansi(&mut self, first: bool, now: Instant) {
        let mut out = io::stdout();
        let use_sync = sync_output_enabled(self.over_ssh);
        if use_sync {
            let _ = write!(out, "\x1b[?2026h");
        }

        if self.use_kitty {
            let (fw, fh) = self.kitty_upscale_into_scaled();
            let nbytes = (fw as usize)
                .saturating_mul(fh as usize)
                .saturating_mul(4)
                .min(self.scaled.len());
            if first {
                let _ = write!(out, "\x1b[{};1H", self.image_row);
            }
            if let Err(e) = write_kitty_frame(
                &mut out,
                IMAGE_ID,
                fw,
                fh,
                &self.scaled[..nbytes],
                self.place_cols,
            ) {
                eprintln!("rustpal: kitty frame error: {e}");
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
            let _ = write_ansi_halfblock(&mut out, fw, fh, frame, self.image_row);
        }

        if self.show_fps {
            self.note_displayed_frame(now);
            let _ = write!(
                out,
                "\x1b[1;1H\x1b[36mrustpal\x1b[0m  \x1b[33m{:>5.1} FPS\x1b[0m\x1b[K",
                self.fps_value
            );
        }

        if use_sync {
            let _ = write!(out, "\x1b[?2026l");
        }
        let _ = out.flush();
    }

    /// Count a frame that was actually sent to the terminal; refresh `fps_value` ~1 Hz.
    fn note_displayed_frame(&mut self, now: Instant) {
        self.fps_frames = self.fps_frames.saturating_add(1);
        let elapsed = now.duration_since(self.fps_window_start).as_secs_f32();
        if elapsed >= 0.5 {
            self.fps_value = self.fps_frames as f32 / elapsed.max(0.001);
            self.fps_frames = 0;
            self.fps_window_start = now;
        }
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
            if let Some(event) = parse_escape(&mut self.esc_buf) {
                self.esc_solo_since = None;
                self.pending.push(event);
            }
            return;
        }
        if let Some(code) = map_ascii(b) {
            self.pending.push(KeyEvent::Tap(code));
        }
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

fn parse_escape(buf: &mut Vec<u8>) -> Option<KeyEvent> {
    if buf.is_empty() || buf.len() == 1 {
        return None;
    }
    if buf[0] != 0x1b {
        buf.clear();
        return None;
    }
    if buf.len() == 2 && buf[1] != b'[' && buf[1] != b'O' {
        buf.clear();
        return Some(KeyEvent::Tap(KeyCode::Escape));
    }
    if buf.get(1) == Some(&b'[') {
        if let Some(&last) = buf.last() {
            if (0x40..=0x7e).contains(&last) && buf.len() >= 3 {
                let params = &buf[2..buf.len() - 1];
                if params.is_empty() && last == b'O' {
                    buf.clear();
                    return Some(KeyEvent::ReleaseAll);
                }
                if params.is_empty() && last == b'I' {
                    buf.clear();
                    return None;
                }
                let code = match last {
                    b'A' => Some(KeyCode::ArrowUp),
                    b'B' => Some(KeyCode::ArrowDown),
                    b'C' => Some(KeyCode::ArrowRight),
                    b'D' => Some(KeyCode::ArrowLeft),
                    b'H' => Some(KeyCode::Home),
                    b'F' => Some(KeyCode::End),
                    b'~' => match csi_primary_param(params) {
                        Some(5) => Some(KeyCode::PageUp),
                        Some(6) => Some(KeyCode::PageDown),
                        Some(2) => Some(KeyCode::Insert),
                        Some(1 | 7) => Some(KeyCode::Home),
                        Some(4 | 8) => Some(KeyCode::End),
                        _ => None,
                    },
                    b'u' => csi_primary_param(params).and_then(map_kitty_key_code),
                    _ => None,
                };
                let event = code.map(|code| key_event_from_csi(code, params, last == b'u'));
                buf.clear();
                return event;
            }
        }
        if buf.len() > 64 {
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
        return code.map(KeyEvent::Tap);
    }
    if buf.len() > 8 {
        buf.clear();
    }
    None
}

/// Convert a CSI key sequence into either a real state transition (when the
/// terminal supplied a Kitty event type) or a deterministic one-frame tap for
/// legacy sequences that have no release information.
fn key_event_from_csi(code: KeyCode, params: &[u8], csi_u: bool) -> KeyEvent {
    match csi_event_type(params) {
        Some(1 | 2) => KeyEvent::State {
            code,
            pressed: true,
        },
        Some(3) => KeyEvent::State {
            code,
            pressed: false,
        },
        // `CSI code u` only exists in the Kitty protocol; a missing event
        // type there means press (release is always explicit as `:3`).
        None if csi_u => KeyEvent::State {
            code,
            pressed: true,
        },
        _ => KeyEvent::Tap(code),
    }
}

fn csi_primary_param(params: &[u8]) -> Option<u32> {
    let end = params
        .iter()
        .position(|&b| b == b';' || b == b':')
        .unwrap_or(params.len());
    std::str::from_utf8(&params[..end]).ok()?.parse().ok()
}

/// Kitty encodes the event type after the modifiers as `;modifiers:event`.
fn csi_event_type(params: &[u8]) -> Option<u8> {
    let text = std::str::from_utf8(params).ok()?;
    let modifiers = text.split(';').nth(1)?;
    modifiers.split(':').nth(1)?.parse().ok()
}

fn map_kitty_key_code(code: u32) -> Option<KeyCode> {
    Some(match code {
        13 => KeyCode::Enter,
        27 | 8 | 127 => KeyCode::Escape,
        32 => KeyCode::Space,
        97 | 65 => KeyCode::KeyA,
        100 | 68 => KeyCode::KeyD,
        101 | 69 => KeyCode::KeyE,
        102 | 70 => KeyCode::KeyF,
        104 | 72 => KeyCode::ArrowLeft,
        106 | 74 => KeyCode::ArrowDown,
        107 | 75 => KeyCode::ArrowUp,
        108 | 76 => KeyCode::ArrowRight,
        113 | 81 => KeyCode::KeyQ,
        114 | 82 => KeyCode::KeyR,
        115 | 83 => KeyCode::KeyS,
        119 | 87 => KeyCode::KeyW,
        57_399 => KeyCode::Numpad0,
        57_400 => KeyCode::Numpad1,
        57_401 => KeyCode::Numpad2,
        57_402 => KeyCode::Numpad3,
        57_403 => KeyCode::Numpad4,
        57_405 => KeyCode::Numpad6,
        57_406 => KeyCode::Numpad7,
        57_407 => KeyCode::Numpad8,
        57_408 => KeyCode::Numpad9,
        57_414 => KeyCode::NumpadEnter,
        57_417 => KeyCode::ArrowLeft,
        57_418 => KeyCode::ArrowRight,
        57_419 => KeyCode::ArrowUp,
        57_420 => KeyCode::ArrowDown,
        57_421 => KeyCode::PageUp,
        57_422 => KeyCode::PageDown,
        57_423 => KeyCode::Home,
        57_424 => KeyCode::End,
        57_425 => KeyCode::Insert,
        57_442 | 57_448 => KeyCode::ControlLeft,
        57_443 => KeyCode::AltLeft,
        57_449 => KeyCode::AltRight,
        _ => return None,
    })
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

/// Shared job slot: latest 320×200 frame + condvar (no 2ms poll spin).
#[cfg(feature = "neural")]
struct NeuralInputSlot {
    frame: Mutex<Option<Vec<u8>>>,
    cvar: Condvar,
}

/// Background GPU neural + zlib/base64 Kitty encode.
///
/// Main thread rate-limits `submit` (~12/s) and only `write_all`s when a new
/// `Arc` payload is ready — no multi-MB clone, no re-send of the same image.
#[cfg(feature = "neural")]
struct NeuralAsync {
    input: Arc<NeuralInputSlot>,
    /// Prebuilt Kitty APC (`Arc` so main never copies multi-MB buffers).
    payload: Arc<Mutex<Option<Arc<Vec<u8>>>>>,
    payload_gen: Arc<AtomicU64>,
    shown_gen: u64,
    cached_payload: Option<Arc<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
    adapter_name: String,
}

#[cfg(feature = "neural")]
impl NeuralAsync {
    fn start(place_cols: u32) -> Option<NeuralAsync> {
        use crate::native_upscale::offline::{OfflineUpscaler, INPUT_SIZE, OUTPUT_SIZE};

        let input = Arc::new(NeuralInputSlot {
            frame: Mutex::new(None),
            cvar: Condvar::new(),
        });
        let payload: Arc<Mutex<Option<Arc<Vec<u8>>>>> = Arc::new(Mutex::new(None));
        let payload_gen = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Option<String>>();

        let input_w = Arc::clone(&input);
        let payload_w = Arc::clone(&payload);
        let gen_w = Arc::clone(&payload_gen);
        let stop_w = Arc::clone(&stop);

        let join = thread::Builder::new()
            .name("rustpal-console-neural".into())
            .spawn(move || {
                let Some(up) = OfflineUpscaler::new() else {
                    let _ = ready_tx.send(None);
                    return;
                };
                let _ = ready_tx.send(Some(up.adapter_name().to_string()));
                let mut out_buf = vec![0u8; OUTPUT_SIZE];
                while !stop_w.load(Ordering::Relaxed) {
                    let job = {
                        let mut g = input_w
                            .frame
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        loop {
                            if stop_w.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Some(frame) = g.take() {
                                break frame;
                            }
                            let (guard, wait_res) = input_w
                                .cvar
                                .wait_timeout(g, Duration::from_millis(50))
                                .unwrap_or_else(|e| {
                                    let g = e.into_inner();
                                    (g.0, g.1)
                                });
                            g = guard;
                            let _ = wait_res;
                        }
                    };
                    // Coalesce anything queued during GPU/compress of previous job.
                    let mut job = job;
                    loop {
                        let newer = {
                            let mut g = input_w
                                .frame
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            g.take()
                        };
                        match newer {
                            Some(n) => job = n,
                            None => break,
                        }
                    }
                    if job.len() != INPUT_SIZE {
                        continue;
                    }
                    up.upscale(&job, &mut out_buf);
                    // Level 4: smaller APC than level 1 → less main-thread write/terminal CPU.
                    match encode_kitty_frame_level(
                        IMAGE_ID,
                        1280,
                        800,
                        &out_buf,
                        place_cols,
                        4,
                    ) {
                        Ok(apc) => {
                            let apc = Arc::new(apc);
                            let mut g = payload_w.lock().unwrap_or_else(|e| e.into_inner());
                            *g = Some(apc);
                            gen_w.fetch_add(1, Ordering::Release);
                        }
                        Err(_) => {}
                    }
                }
            })
            .ok()?;

        let adapter_name = match ready_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Some(name)) => name,
            _ => {
                stop.store(true, Ordering::Relaxed);
                input.cvar.notify_all();
                let _ = join.join();
                return None;
            }
        };

        Some(NeuralAsync {
            input,
            payload,
            payload_gen,
            shown_gen: 0,
            cached_payload: None,
            stop,
            join: Some(join),
            adapter_name,
        })
    }

    fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Queue a 320×200 RGBA frame (replaces any not-yet-processed job).
    fn submit(&self, rgba: &[u8]) {
        {
            let mut g = self.input.frame.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_mut() {
                Some(buf) if buf.len() == rgba.len() => buf.copy_from_slice(rgba),
                _ => *g = Some(rgba.to_vec()),
            }
        }
        self.input.cvar.notify_one();
    }

    /// Returns `(have_any_frame, is_new_since_last_poll)`.
    fn poll_payload(&mut self) -> (bool, bool) {
        let gen = self.payload_gen.load(Ordering::Acquire);
        if gen == 0 {
            return (false, false);
        }
        let is_new = gen != self.shown_gen;
        if is_new {
            let g = self.payload.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref p) = *g {
                self.cached_payload = Some(Arc::clone(p));
                self.shown_gen = gen;
                return (true, true);
            }
            return (false, false);
        }
        (self.cached_payload.is_some(), false)
    }

    fn payload_bytes(&self) -> Option<&[u8]> {
        self.cached_payload.as_ref().map(|a| a.as_slice())
    }

    fn payload_arc(&self) -> Option<Arc<Vec<u8>>> {
        self.cached_payload.clone()
    }
}

#[cfg(feature = "neural")]
impl Drop for NeuralAsync {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.input.cvar.notify_all();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Resolved console geometry for one backend.
struct ConsoleLayout {
    place_cols: u32,
    kitty_nn_scale: u32,
    ansi_scale: u32,
    /// Terminal cell size in pixels when `TIOCGWINSZ` reports xpixel/ypixel.
    cell: Option<(u32, u32)>,
}

/// `RUSTPAL_CONSOLE_UPSCALE=nn|hqx4|xbr4|neural` (default `nn`).
fn resolve_console_upscale() -> ConsoleUpscale {
    let raw = std::env::var("RUSTPAL_CONSOLE_UPSCALE")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match raw.trim() {
        "" | "nn" | "nearest" => ConsoleUpscale::Nearest,
        "hqx" | "hqx4" | "xbr" | "xbr4" | "xbrz" | "xbrz4" => ConsoleUpscale::Hqx4,
        "neural" | "gpu" | "nn-gpu" | "esrgan" => ConsoleUpscale::Neural,
        other => {
            eprintln!(
                "rustpal: unknown RUSTPAL_CONSOLE_UPSCALE={other:?}, using nn (try nn|hqx4|neural)"
            );
            ConsoleUpscale::Nearest
        }
    }
}

fn kitty_output_width(upscale: ConsoleUpscale, kitty_nn_scale: u32) -> u32 {
    match upscale {
        ConsoleUpscale::Nearest => SCREEN_W as u32 * kitty_nn_scale.max(1),
        ConsoleUpscale::Hqx4 | ConsoleUpscale::Neural => 1280,
    }
}

fn kitty_output_height(upscale: ConsoleUpscale, kitty_nn_scale: u32) -> u32 {
    match upscale {
        ConsoleUpscale::Nearest => SCREEN_H as u32 * kitty_nn_scale.max(1),
        ConsoleUpscale::Hqx4 | ConsoleUpscale::Neural => 800,
    }
}

/// Pick Kitty `c=` and NN scale, or ANSI scale.
///
/// * **Nearest:** choose NN + `c=` so on-screen width ≈ `320×NN` (hard edges).
/// * **HQX/neural:** bitmap is always 1280×800; pick `c=` near full terminal
///   width using an **integer display scale** `k` (`disp ≈ 1280×k`) so the
///   picture is not stuck at tiny 1:1 (e.g. ~58 cols on large fonts).
fn resolve_console_layout(
    use_kitty: bool,
    over_ssh: bool,
    reserve_top: u32,
    upscale: ConsoleUpscale,
) -> ConsoleLayout {
    let (cols, rows, cell) = terminal_geometry().unwrap_or((100, 30, None));
    let manual = std::env::var("RUSTPAL_CONSOLE_SCALE")
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    let avail_rows = rows.saturating_sub(reserve_top).max(1);

    if use_kitty {
        // Desired width in columns before pixel alignment.
        let want = if let Some(n) = manual {
            (n.clamp(1, 16) * 40).min(cols.saturating_sub(2).max(20))
        } else {
            cols.saturating_sub(2).clamp(40, 240)
        };
        let max_by_rows = max_kitty_cols_for_rows(avail_rows, cell);
        let place_want = want.min(max_by_rows).clamp(20, 240);
        let max_place = max_by_rows.min(240).max(20);

        let (place_cols, kitty_nn_scale) = match upscale {
            ConsoleUpscale::Nearest => align_kitty_nn_and_place(
                place_want,
                max_place,
                cell,
                over_ssh,
                kitty_nn_env_override(),
            ),
            // Fixed 1280×800 source — grow with integer k to fill the terminal.
            ConsoleUpscale::Hqx4 | ConsoleUpscale::Neural => {
                let place = align_fixed_src_place(1280, place_want, max_place, cell);
                (place, 4)
            }
        };
        ConsoleLayout {
            place_cols,
            kitty_nn_scale,
            ansi_scale: 1,
            cell,
        }
    } else {
        let scale = if let Some(n) = manual {
            n.clamp(1, 3)
        } else {
            let max_cols = cols.saturating_sub(2).max(1);
            // Half-block cells: one terminal row ≈ 2 source pixels.
            let max_rows = avail_rows.saturating_sub(1).max(1);
            let sx = max_cols / SCREEN_W as u32;
            let sy = max_rows / ((SCREEN_H as u32) / 2);
            sx.min(sy).clamp(1, 3)
        };
        ConsoleLayout {
            place_cols: cols,
            kitty_nn_scale: 1,
            ansi_scale: scale,
            cell,
        }
    }
}

/// Explicit `RUSTPAL_CONSOLE_KITTY_NN` if set and valid.
fn kitty_nn_env_override() -> Option<u32> {
    std::env::var("RUSTPAL_CONSOLE_KITTY_NN")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .map(|n| n.clamp(1, 8))
}

/// Choose `(place_cols, nn)` so transmitted `320×nn` is as close as possible to
/// `place_cols × cell_w` on screen (Kitty 1:1 / integer-ish scale).
///
/// * `nn_override` (from `RUSTPAL_CONSOLE_KITTY_NN`) forces NN; columns still
///   snap toward 1:1.
/// * Without cell metrics: place stays at `place_want`, NN defaults 4 / SSH 3.
fn align_kitty_nn_and_place(
    place_want: u32,
    max_place: u32,
    cell: Option<(u32, u32)>,
    over_ssh: bool,
    nn_override: Option<u32>,
) -> (u32, u32) {
    let max_place = max_place.max(20);
    let place_want = place_want.clamp(20, max_place);
    let nn_env = nn_override.map(|n| n.clamp(1, 8));

    let Some((cw, _ch)) = cell.filter(|(w, h)| *w > 0 && *h > 0) else {
        let nn = nn_env.unwrap_or(if over_ssh { 3 } else { 4 });
        return (place_want, nn);
    };

    if let Some(nn) = nn_env {
        let place = place_cols_for_src_width(SCREEN_W as u32 * nn, cw, max_place);
        return (place, nn);
    }

    // Prefer a 1:1 placement whose column count is closest to `place_want`.
    // Among equal distance, prefer larger image (sharper on big terminals).
    let mut best: Option<(u32, u32, u32)> = None; // (col_dist, nn, place) — min dist, then max nn
    for nn in 1..=8u32 {
        let place = place_cols_for_src_width(SCREEN_W as u32 * nn, cw, max_place);
        // Reject placements that would force heavy soft scale (>12.5% width error).
        let disp_w = place as u64 * cw as u64;
        let src_w = SCREEN_W as u64 * nn as u64;
        let err = disp_w.abs_diff(src_w);
        if err * 8 > src_w {
            continue;
        }
        let dist = place.abs_diff(place_want);
        let cand = (dist, nn, place);
        let take = match best {
            None => true,
            Some(b) => {
                // Lower column distance wins; tie-break: higher nn (more source pixels).
                cand.0 < b.0 || (cand.0 == b.0 && cand.1 > b.1)
            }
        };
        if take {
            best = Some(cand);
        }
    }

    if let Some((_dist, nn, place)) = best {
        return (place, nn);
    }

    // Fallback: NN from wanted display width, snap columns.
    let display_w = place_want as u64 * cw as u64;
    let nn = ((display_w + SCREEN_W as u64 / 2) / SCREEN_W as u64)
        .clamp(1, 8) as u32;
    let place = place_cols_for_src_width(SCREEN_W as u32 * nn, cw, max_place);
    (place, nn)
}

/// Columns `c` such that `c * cell_w ≈ src_w`, clamped to `[20, max_place]`.
fn place_cols_for_src_width(src_w: u32, cell_w: u32, max_place: u32) -> u32 {
    let cell_w = cell_w.max(1) as u64;
    let place = ((src_w as u64 + cell_w / 2) / cell_w).max(1) as u32;
    place.clamp(20, max_place.max(20))
}

/// Place a **fixed** source width (e.g. HQX/neural 1280) on the terminal.
///
/// Prefers integer display scales `k` so `place × cell_w ≈ src_w × k`, picking
/// the candidate closest to `place_want` (fill the window) rather than locking
/// to tiny 1:1 when the font is large.
fn align_fixed_src_place(
    src_w: u32,
    place_want: u32,
    max_place: u32,
    cell: Option<(u32, u32)>,
) -> u32 {
    let max_place = max_place.max(20);
    let place_want = place_want.clamp(20, max_place);

    let Some((cw, _ch)) = cell.filter(|(w, h)| *w > 0 && *h > 0) else {
        return place_want;
    };

    // Among integer scales k with ≤5% soft error, pick closest to place_want
    // (fill the terminal). Tie-break: lower err, then larger k.
    let mut best: Option<(u32, u64, u32, u32)> = None; // (dist, err, k, place)
    for k in 1..=8u32 {
        let ideal_w = src_w.saturating_mul(k);
        let place = place_cols_for_src_width(ideal_w, cw, max_place);
        let disp_w = place as u64 * cw as u64;
        let err = disp_w.abs_diff(ideal_w as u64);
        if err.saturating_mul(20) > ideal_w as u64 {
            continue;
        }
        let dist = place.abs_diff(place_want);
        let cand = (dist, err, k, place);
        let take = match best {
            None => true,
            Some(b) => {
                cand.0 < b.0
                    || (cand.0 == b.0 && cand.1 < b.1)
                    || (cand.0 == b.0 && cand.1 == b.1 && cand.2 > b.2)
            }
        };
        if take {
            best = Some(cand);
        }
    }

    best.map(|c| c.3).unwrap_or(place_want)
}

/// Largest Kitty `c=` that still fits in `avail_rows` at 320×200 aspect.
///
/// With only `c=` set, display height in cells is:
/// `rows ≈ c * (cell_w / cell_h) * (200 / 320)`.
fn max_kitty_cols_for_rows(avail_rows: u32, cell: Option<(u32, u32)>) -> u32 {
    let (cw, ch) = cell.unwrap_or((1, 2)); // typical monospace ~1:2
    if avail_rows == 0 || cw == 0 || ch == 0 {
        return 240;
    }
    // place * cw / ch * SCREEN_H / SCREEN_W <= avail_rows
    // place <= avail_rows * ch * SCREEN_W / (cw * SCREEN_H)
    let num = avail_rows as u64 * ch as u64 * SCREEN_W as u64;
    let den = cw as u64 * SCREEN_H as u64;
    (num / den.max(1)).max(20) as u32
}

/// `(cols, rows, optional (cell_w_px, cell_h_px))`.
fn terminal_geometry() -> Option<(u32, u32, Option<(u32, u32)>)> {
    #[cfg(unix)]
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0
            && ws.ws_col > 0
            && ws.ws_row > 0
        {
            let cols = ws.ws_col as u32;
            let rows = ws.ws_row as u32;
            let cell = if ws.ws_xpixel > 0 && ws.ws_ypixel > 0 {
                let cw = (ws.ws_xpixel as u32 / cols).max(1);
                let ch = (ws.ws_ypixel as u32 / rows).max(1);
                Some((cw, ch))
            } else {
                None
            };
            return Some((cols, rows, cell));
        }
    }
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())?;
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    Some((cols, rows, None))
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

fn is_over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
}

fn frame_interval(over_ssh: bool, upscale: ConsoleUpscale) -> Duration {
    match (upscale, over_ssh) {
        (ConsoleUpscale::Neural, true) => MIN_FRAME_INTERVAL_NEURAL_SSH,
        (ConsoleUpscale::Neural, false) => MIN_FRAME_INTERVAL_NEURAL,
        (_, true) => MIN_FRAME_INTERVAL_SSH,
        (_, false) => MIN_FRAME_INTERVAL,
    }
}

fn sync_output_enabled(over_ssh: bool) -> bool {
    match std::env::var("RUSTPAL_CONSOLE_SYNC").ok().as_deref() {
        Some("1") | Some("true") | Some("yes") => true,
        Some("0") | Some("false") | Some("no") => false,
        // Default: on over SSH only (reduces white flash); off locally.
        _ => over_ssh,
    }
}

/// True for `1` / `true` / `yes` / `on` (case-insensitive). Empty/unset → false.
fn env_flag_enabled(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        }
        Err(_) => false,
    }
}

/// Build a full Kitty APC sequence (zlib + base64 + chunking) into a buffer.
fn encode_kitty_frame(
    image_id: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
    place_cols: u32,
) -> io::Result<Vec<u8>> {
    encode_kitty_frame_level(image_id, width, height, rgba, place_cols, 1)
}

/// Like `encode_kitty_frame` with an explicit zlib level (neural worker uses 4).
fn encode_kitty_frame_level(
    image_id: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
    place_cols: u32,
    zlib_level: u8,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(rgba.len() / 4 + 512);
    write_kitty_frame_level(
        &mut out,
        image_id,
        width,
        height,
        rgba,
        place_cols,
        zlib_level,
    )?;
    Ok(out)
}

/// Transmit + display (a=T) with stable image/placement ids so Kitty replaces
/// the same slot. Same `i`/`p`/`c` every frame is the portable path that
/// actually paints; `a=t` alone left a black screen on several Kitty builds.
fn write_kitty_frame(
    out: &mut impl Write,
    image_id: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
    place_cols: u32,
) -> io::Result<()> {
    write_kitty_frame_level(out, image_id, width, height, rgba, place_cols, 1)
}

fn write_kitty_frame_level(
    out: &mut impl Write,
    image_id: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
    place_cols: u32,
    zlib_level: u8,
) -> io::Result<()> {
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    let level = zlib_level.clamp(1, 9);
    // The game frame is opaque: ship 24-bit RGB (f=24), 25% less raw data
    // before zlib and a proportionally shorter base64 stream.
    let rgb = rgba_to_rgb(rgba);
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&rgb, level);
    let payload = BASE64.encode(&compressed);
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(KITTY_CHUNK).collect();
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i != last);
        if i == 0 {
            // a=T transmit+display, c= columns (Kitty scales), C=1 keep cursor.
            write!(
                out,
                "\x1b_Ga=T,f=24,o=z,s={width},v={height},t=d,i={image_id},p=1,c={place_cols},C=1,q=2,m={more};"
            )?;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        out.write_all(chunk)?;
        write!(out, "\x1b\\")?;
    }
    Ok(())
}

/// Drop the alpha channel: Kitty `f=24` expects packed RGB.
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    rgb
}

// --- ANSI --------------------------------------------------------------------

fn write_ansi_halfblock(
    out: &mut impl Write,
    width: usize,
    height: usize,
    rgba: &[u8],
    image_row: u32,
) -> io::Result<()> {
    let rows = height / 2;
    let origin = image_row.max(1);
    for cy in 0..rows {
        let y0 = cy * 2;
        let y1 = y0 + 1;
        write!(out, "\x1b[{};1H", origin + cy as u32)?;
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
    fn kitty_frame_header_has_place_and_display() {
        let px = [255u8, 0, 0, 255];
        let mut out = Vec::new();
        write_kitty_frame(&mut out, 1, 1, 1, &px, 80).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("c=80"), "{s}");
        assert!(s.contains("a=T"), "{s}");
        assert!(s.contains("f=24"), "{s}");
        assert!(s.contains("i=1"), "{s}");
    }

    #[test]
    fn kitty_payload_is_packed_rgb() {
        let px = [1u8, 2, 3, 255, 4, 5, 6, 255];
        assert_eq!(rgba_to_rgb(&px), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn max_kitty_cols_respects_available_rows() {
        // 1×2 cells, 40 rows free → place ≤ 40 * 2 * 320 / (1 * 200) = 128
        assert_eq!(max_kitty_cols_for_rows(40, Some((1, 2))), 128);
        // Fewer rows → tighter width cap.
        assert_eq!(max_kitty_cols_for_rows(20, Some((1, 2))), 64);
        // Fallback cell aspect still returns a usable value.
        assert!(max_kitty_cols_for_rows(10, None) >= 20);
    }

    #[test]
    fn parse_arrow_csi() {
        let mut buf = vec![0x1b, b'[', b'A'];
        assert_eq!(
            parse_escape(&mut buf),
            Some(KeyEvent::Tap(KeyCode::ArrowUp))
        );
    }

    #[test]
    fn parse_kitty_arrow_press_repeat_release() {
        for (event_type, pressed) in [(1, true), (2, true), (3, false)] {
            let mut buf = format!("\x1b[1;1:{event_type}C").into_bytes();
            assert_eq!(
                parse_escape(&mut buf),
                Some(KeyEvent::State {
                    code: KeyCode::ArrowRight,
                    pressed,
                })
            );
            assert!(buf.is_empty());
        }
    }

    #[test]
    fn parameterized_legacy_arrow_is_still_a_tap() {
        let mut buf = b"\x1b[1;2D".to_vec();
        assert_eq!(
            parse_escape(&mut buf),
            Some(KeyEvent::Tap(KeyCode::ArrowLeft))
        );
    }

    #[test]
    fn focus_out_releases_all_and_focus_in_is_ignored() {
        let mut buf = b"\x1b[O".to_vec();
        assert_eq!(parse_escape(&mut buf), Some(KeyEvent::ReleaseAll));
        assert!(buf.is_empty());
        let mut buf = b"\x1b[I".to_vec();
        assert_eq!(parse_escape(&mut buf), None);
        assert!(buf.is_empty());
    }

    #[test]
    fn kitty_csi_u_without_event_type_is_a_press() {
        let mut buf = b"\x1b[13u".to_vec();
        assert_eq!(
            parse_escape(&mut buf),
            Some(KeyEvent::State {
                code: KeyCode::Enter,
                pressed: true,
            })
        );
        let mut buf = b"\x1b[104;1:3u".to_vec();
        assert_eq!(
            parse_escape(&mut buf),
            Some(KeyEvent::State {
                code: KeyCode::ArrowLeft,
                pressed: false,
            })
        );
    }

    #[test]
    fn parse_kitty_csi_u_key_event() {
        let mut buf = b"\x1b[27;1:3u".to_vec();
        assert_eq!(
            parse_escape(&mut buf),
            Some(KeyEvent::State {
                code: KeyCode::Escape,
                pressed: false,
            })
        );
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

    #[test]
    fn nearest_upscale_4x_logical_frame_size() {
        // Matches the Kitty default path: 320×200 → 1280×800.
        let src = vec![0u8; SCREEN_W * SCREEN_H * 4];
        let mut dst = vec![0u8; SCREEN_W * SCREEN_H * 4 * 16];
        nearest_upscale(&src, SCREEN_W, SCREEN_H, 4, &mut dst);
        assert_eq!(dst.len(), 1280 * 800 * 4);
    }

    #[test]
    fn place_cols_rounds_to_src_pixel_width() {
        // 1280px source, 8px cells → 160 columns.
        assert_eq!(place_cols_for_src_width(1280, 8, 240), 160);
        // 960px / 10px → 96 cols.
        assert_eq!(place_cols_for_src_width(960, 10, 240), 96);
    }

    #[test]
    fn align_kitty_picks_1to1_nn_near_want() {
        // cell 8×16, want 160 cols → 1280px → nn=4, place=160 exact.
        let (place, nn) = align_kitty_nn_and_place(160, 240, Some((8, 16)), false, None);
        assert_eq!(nn, 4);
        assert_eq!(place, 160);
        assert_eq!(place * 8, 320 * nn);
    }

    #[test]
    fn align_kitty_nn_override_snaps_cols() {
        let (place, nn) =
            align_kitty_nn_and_place(160, 240, Some((8, 16)), false, Some(2));
        assert_eq!(nn, 2);
        // 640px / 8 = 80 cols (1:1 for nn=2, not full 160).
        assert_eq!(place, 80);
        assert_eq!(place * 8, 320 * nn);
    }

    #[test]
    fn align_kitty_without_cell_uses_defaults() {
        let (place, nn) = align_kitty_nn_and_place(120, 240, None, false, None);
        assert_eq!(place, 120);
        assert_eq!(nn, 4);
        let (place_ssh, nn_ssh) = align_kitty_nn_and_place(120, 240, None, true, None);
        assert_eq!(place_ssh, 120);
        assert_eq!(nn_ssh, 3);
    }

    #[test]
    fn align_kitty_prefers_want_width_among_1to1() {
        // cell 10×20, want 100 cols → 1000px. nn=3 → 960px → place=96 (near 100).
        let (place, nn) = align_kitty_nn_and_place(100, 240, Some((10, 20)), false, None);
        assert_eq!(nn, 3);
        assert_eq!(place, 96);
        assert_eq!(place * 10, 320 * nn);
    }

    #[test]
    fn fixed_src_place_grows_with_integer_k() {
        // Ghostty-like large cells: 1:1 of 1280 is only ~58 cols; with want=204
        // we should pick k≥2 so the image is not a postage stamp.
        let place = align_fixed_src_place(1280, 204, 204, Some((22, 55)));
        assert!(
            place >= 100,
            "expected large placement for hqx/neural, got {place}"
        );
        // k=3 → 3840px / 22 ≈ 175
        assert_eq!(place, 175);
    }

    #[test]
    fn fixed_src_place_without_cell_fills_want() {
        assert_eq!(align_fixed_src_place(1280, 160, 240, None), 160);
    }
}
