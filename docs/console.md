# Console video backend

Pure terminal presentation for rustpal: **no window toolkit, no sound device,
no system GUI/audio packages** when built with the `console` feature alone.

## Build

```shell
# Console only (crates.io deps: base64, miniz_oxide, libc)
cargo build --release --no-default-features --features console
# Headless + GPU neural upscale (wgpu, no window/audio libs):
cargo build --release --no-default-features --features console-neural

# Default: GUI + console (both backends; --console selects terminal at runtime)
cargo build --release
```

## Run

```shell
./target/release/rustpal --console          # auto: Kitty if detected, else ANSI
./target/release/rustpal --console=kitty
./target/release/rustpal --console=ansi
# Kitty size: --console-scale=N ≈ N×40 terminal columns (e.g. 4 → ~160 cols)
./target/release/rustpal --console --console-scale=5

# Kitty pre-scale filter (default nn). Also: hqx4 / xbr4 / neural
RUSTPAL_CONSOLE_UPSCALE=hqx4 ./target/release/rustpal --console=kitty
RUSTPAL_CONSOLE_UPSCALE=neural ./target/release/rustpal --console=kitty
# NN only: auto pixel-aligns; force factor with RUSTPAL_CONSOLE_KITTY_NN=1..8
RUSTPAL_CONSOLE_KITTY_NN=4 ./target/release/rustpal --console=kitty

# Show FPS on the top status line (displayed frames / wall time, ~0.5s window)
RUSTPAL_CONSOLE_FPS=1 ./target/release/rustpal --console=kitty
# alias: RUSTPAL_SHOW_FPS=1

# Terminal video + external script control (HTTP on loopback)
./target/release/rustpal --console --ui-driver
./target/release/rustpal --console=kitty --ui-driver=127.0.0.1:8765
# Then from another shell: curl -X POST http://127.0.0.1:8765/v1/input/confirm/tap
# Full API: docs/autoplay.md

# Logs while the game is on the alternate screen (avoids scrolling the picture)
# Default: stderr is muted (/dev/null) after startup.
# RUSTPAL_CONSOLE_LOG=/tmp/rustpal.log   # append engine/pilot logs here
# RUSTPAL_CONSOLE_VERBOSE=1              # keep stderr on the tty (debug; may jump the image)
```

Needs the `pal/` data directory (same as GUI).

**Kitty:** upscales the logical **320×200** frame **before** transmit, then
sizes with graphics-protocol `c=` (columns). Filter via
`RUSTPAL_CONSOLE_UPSCALE`:

| Value | Method | Output | Notes |
|-------|--------|--------|--------|
| `nn` (default) | Nearest-neighbor | `320×N × 200×N` | Pixel-align `N` + `c=` to cell size when known |
| `hqx4` / `xbr4` | CPU HQ4x (2× HQ2x) | 1280×800 | No GPU; smoother than raw NN |
| `neural` | GUI mega-kernel (wgpu) | 1280×800 | **Async** worker: GPU + zlib/Kitty encode off main; needs the `neural` feature (in `gui`, or `--features console-neural` for headless builds) + F16 GPU; else `hqx4` |

When the terminal reports cell pixel size, **`c=` is chosen** for a sharp
display: **nn** aims on-screen width ≈ `320×N`; **hqx4/neural** keep a 1280×800
bitmap and pick an **integer display scale** `k` so width ≈ `1280×k` near full
terminal size (avoids a tiny 1:1 stamp on large fonts). Override NN factor with
`RUSTPAL_CONSOLE_KITTY_NN=1..8` (nn mode only). Width is capped so the image
fits the **available rows**. Help text is on the primary screen before alt
buffer; `RUSTPAL_CONSOLE_FPS=1` reserves the top row. Startup log shows filter,
size, and `1:1` / `near`.

**ANSI:** small integer upscale (1–3×) + half-block cells.

**Input:** background thread reads `/dev/tty`. **Ctrl-C** installs a signal
handler that leaves the alternate screen, restores echo/cooked mode, then
exits — so Kitty does not stay frozen on the game frame with invisible typing.

### Controls

| Key | Action |
| --- | --- |
| Arrows / hjkl | Move |
| Enter / Space | Confirm |
| Esc / Backspace | Menu / cancel |
| R A D E W Q F S | Battle shortcuts (same as GUI) |
| Ctrl-C | Quit |

Compatible terminals (Kitty, WezTerm, foot, Ghostty, recent Alacritty) get the
Kitty keyboard protocol with flags `1+2+8`: every key — arrows, hjkl, Enter,
Space, battle letters — reports real press/repeat/release, so holding a key
walks continuously exactly like the GUI build, and a bare Esc is recognised
immediately (no 50 ms wait). Focus-out reporting (`?1004h`) releases all held
keys when you switch windows, so a key-up lost to another window can never
leave the party walking.

Legacy terminals fall back to frame-latched taps: one arrow/hjkl sequence moves
at most one step, and repeat bursts are coalesced instead of queuing extra
movement after release. Holding a key there pauses until the OS autorepeat
delay kicks in.

### Flicker (especially over SSH)

Local Kitty is usually fine; **SSH latency** can make APC chunk gaps look like
white flashes.

Mitigations:

1. Stable Kitty image id + placement (`i=1`, `p=1`, `c=…`) every frame via `a=T`.
2. Over SSH: **`CSI ? 2026` synchronized output** (batch until the frame is complete).
   - Force on/off: `RUSTPAL_CONSOLE_SYNC=1` or `=0`
3. Slightly lower FPS when `SSH_*` is set.

Prefer `kitten ssh user@host`. Avoid tmux unless passthrough is enabled.

> Note: `a=t` (transmit-only) updates were tried for less flicker but left a
> **blank screen** on several Kitty builds, so they are not used.

### Kitty mode

Uses the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
to draw the real **320×200** frame as 24-bit RGB (zlib + base64 APC chunks), same idea as
[zenbu-labs/terminal-browser](https://github.com/zenbu-labs/terminal-browser)
(`pixel-core` kitty transmit). Works in Kitty, Ghostty, and other supporting
terminals (often over SSH).

### ANSI mode

Truecolor half-block cells (`▄`) for ordinary terminals. Full width is 320
columns × 100 rows of cells — zoom the font or use a large window.

## Architecture

```
Engine (unchanged game logic)
  → VideoBackend::Console
  → ConsoleVideo
       pump()    → tty keys + optional UiDriver HTTP input
       present() → UiDriver frame capture (if --ui-driver)
                 → render_rgba → Kitty or ANSI → stdout
  → audio = None  (optional offline mixer only in tools)
```

### Built-in pilot + recording

```shell
# Watch the synthetic pilot in the terminal while dumping frames/audio
cargo run --release --example autoplay -- record --console /tmp/ap 60

# Same, and also expose the HTTP control API for a second client
RUSTPAL_UI_DRIVER=127.0.0.1:8765 \
  cargo run --release --example autoplay -- record --console=kitty /tmp/ap 60

# Whole-game route probe, watched in the terminal (realtime; no video file)
cargo run --release --example fullgame_autoplay -- --console
cargo run --release --example fullgame_autoplay -- --console=kitty
# logs → recordings/fullgame-autoplay.log  (default when watching)
```

Feature flags (`Cargo.toml`):

| Feature | Provides |
| --- | --- |
| `gui` (default) | winit, pixels, cpal, png, neural upscale |
| `console` (default) | terminal backend |

Console-only binary: `--no-default-features --features console`.

## Limits

- No music/SFX in console mode
- Legacy terminals cannot report key-up; movement uses deterministic one-frame taps
- Smooth physical hold/release requires Kitty keyboard event-type support
- Bare Esc on legacy terminals is recognized after a ~50 ms CSI timeout
- Not a substitute for the 720p GUI / neural upscale path
