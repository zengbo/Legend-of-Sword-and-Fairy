# Console video backend

Pure terminal presentation for rustpal: **no window toolkit, no sound device,
no system GUI/audio packages** when built with the `console` feature alone.

## Build

```shell
# Console only (crates.io deps: base64, miniz_oxide, libc)
cargo build --release --no-default-features --features console

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
```

Needs the `pal/` data directory (same as GUI).

**Kitty:** always sends **320×200** pixels; the terminal stretches them with the
graphics-protocol `c=` (column) placement — large and fast (no multi‑MB
upscaled bitmaps that froze the loop).

**ANSI:** small integer upscale (1–3×) + half-block cells.

**Input:** background thread reads `/dev/tty`. **Ctrl-C** installs a signal
handler that leaves the alternate screen, restores echo/cooked mode, then
exits — so Kitty does not stay frozen on the game frame with invisible typing.

### Controls

| Key | Action |
| --- | --- |
| Arrows / hjkl | Move |
| Enter / Space | Confirm |
| Esc | Menu / cancel |
| R A D E W Q F S | Battle shortcuts (same as GUI) |
| Ctrl-C | Quit |

Keys are held for ~150 ms in engine time (OS key-repeat extends this) so the
game sees a real press; earlier builds released in the same frame and menus
ignored input.

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
to draw the real **320×200 RGBA** frame (zlib + base64 APC chunks), same idea as
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
       pump()    → raw stdin → KeyCode
       present() → render_rgba → Kitty or ANSI → stdout
  → audio = None
```

Feature flags (`Cargo.toml`):

| Feature | Provides |
| --- | --- |
| `gui` (default) | winit, pixels, cpal, png, neural upscale |
| `console` (default) | terminal backend |

Console-only binary: `--no-default-features --features console`.

## Limits

- No music/SFX in console mode
- Terminals only report key *presses*; each press is paired with a synthetic release
- Bare Esc is recognized after the stdin poll (no long CSI wait)
- Not a substitute for the 720p GUI / neural upscale path
