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
```

Needs the `pal/` data directory (same as GUI).

### Controls

| Key | Action |
| --- | --- |
| Arrows / hjkl | Move |
| Enter / Space | Confirm |
| Esc | Menu / cancel |
| R A D E W Q F S | Battle shortcuts (same as GUI) |
| Ctrl-C | Quit |

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
