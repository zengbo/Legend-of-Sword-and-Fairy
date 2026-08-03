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
./target/release/rustpal --console=kitty    # force Kitty graphics protocol
./target/release/rustpal --console=ansi     # force half-block truecolor
./target/release/rustpal --console --console-scale=6   # force 6× (1920×1200)
# or: RUSTPAL_CONSOLE_SCALE=6 ./target/release/rustpal --console
```

Needs the `pal/` data directory (same as GUI).

Native game resolution is **320×200**. Kitty mode used to draw 1:1 device
pixels (postage-stamp on Retina). The console backend now **integer-upscales**
(nearest-neighbor) to fit the terminal (or `RUSTPAL_CONSOLE_SCALE` / `--console-scale=N`).
Banner line shows the effective scale, e.g. `4× → 1280×800`.

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

### Flicker

Presents use the terminal **synchronized output** mode (`CSI ? 2026 h/l`) and
only retransmit when the 320×200 frame changes. Kitty placement is set once;
later frames replace the same image id without moving the cursor.

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
