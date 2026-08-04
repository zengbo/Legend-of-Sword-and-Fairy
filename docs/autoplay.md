# Offscreen / Console Autoplay

> **For AI agents:** use the dedicated control brief
> [`docs/ai-control.md`](ai-control.md) (API, step loop, system prompt).

The native game exposes an opt-in HTTP interface for automation. Frames and
input use the same engine path as a real keyboard; you can watch on a GUI
window, headlessly, or in the **terminal console**.

```shell
# GUI offscreen (no window) + HTTP
cargo run --release -- --ui-driver --offscreen

# Terminal video + HTTP (watch in Kitty/ANSI while a script presses keys)
cargo run --release -- --console --ui-driver
cargo run --release -- --console=kitty --ui-driver=127.0.0.1:8765

# Headless-style agent loop: virtual clock, single-frame steps
cargo run --release -- --ui-driver --offscreen --ui-step --mute

# Built-in pilot example, visible in the terminal
cargo run --release --example autoplay -- record --console /tmp/ap 60
```

The default endpoint is `http://127.0.0.1:8765`. Use
`--ui-driver=127.0.0.1:PORT` to select another loopback port. Non-loopback
addresses are rejected.

`--ui-driver` works with **GUI**, **`--offscreen`**, and **`--console`**. On
console, logical 320×200 frames are still published to `/v1/frame.png` even
when terminal redraw is throttled.

## Interface

| Request | Purpose |
| --- | --- |
| `GET /v1/status` | `status`, size, `frame_id`, `step_mode`, virtual `ticks`. |
| `GET /v1/state` | Structured game snapshot (scene, party, dialog, battle, …). |
| `GET /v1/frame.png` | Latest logical 320×200 RGBA frame as PNG. |
| `POST /v1/step` | Advance virtual clock (step mode only). Default +100 ms (one overworld frame). |
| `POST /v1/step?frames=N` | Advance `N × 100` ms. |
| `POST /v1/step?ms=N` | Advance `N` ms. |
| `POST /v1/input/{key}/tap` | Press and release one game key. |
| `POST /v1/input/{key}/press` | Hold one game key down. |
| `POST /v1/input/{key}/release` | Release one held game key. |

Supported key names are `up`, `down`, `left`, `right`, `menu`, `confirm`,
`space`, `page_up`, `page_down`, `home`, `end`, `repeat`, `auto`, `defend`,
`use_item`, `throw_item`, `flee`, `force`, and `status`.

### `GET /v1/state` fields

Rich JSON for agents (fields may grow; ignore unknowns). Full field guide:
[`docs/ai-control.md`](ai-control.md). Highlights:

| Field | Meaning |
| --- | --- |
| `phase` | `boot` / `dialog` / `battle` / `overworld` / `scene_transition` |
| `walk` | `{up,right,down,left}` next-step free (collision) |
| `events` | Nearby scene event objects (pos, dist, scripts, search/touch) |
| `inventory` | Items with UTF-8 names and usable/equipable flags |
| `party` | Stats, equipment, magics (names decoded Big5→UTF-8) |
| `battle` | `null` or enemies/UI menu state |
| `keys_hint` / `actions` | Suggested keys / vocabulary |
| `frame_id`, `scene`, `player`, `in_*`, `cash`, … | Core scalars (as before) |

### Step mode (`--ui-step` / `RUSTPAL_UI_STEP=1`)

With step mode **gating on**, `Engine::ticks` no longer follows wall time.
Delays and frame pacing wait until an agent calls `POST /v1/step`. This is the
headless **single-frame** control loop for AI.

| How you run | Clock | Console picture |
| --- | --- | --- |
| `--ui-step --offscreen` | Virtual (must `POST /v1/step`) | n/a |
| `--ui-step --console` | **Realtime** by default | Animates normally |
| `--ui-step --console` + `RUSTPAL_UI_STEP_STRICT=1` | Virtual | Frozen until stepped |

Why: every boot path hits `delay` before the first full present. If the clock
is virtual and nothing calls `/v1/step`, the console stays on an empty alt
screen. So **console defaults to wall-clock** so you can still watch; use
`STRICT` when you want single-frame control while watching.

```shell
# Terminal 1 — headless agent: engine blocks until stepped
cargo run --release -- --ui-driver --offscreen --ui-step --mute

# Terminal 1 — watch in console (realtime) + HTTP state/input
cargo run --release -- --console --ui-driver --ui-step

# Terminal 1 — watch AND freeze until stepped
RUSTPAL_UI_STEP_STRICT=1 cargo run --release -- --console --ui-driver --ui-step

# Terminal 2 — agent
curl -s http://127.0.0.1:8765/v1/state
curl -s -X POST http://127.0.0.1:8765/v1/input/confirm/tap
curl -s -X POST 'http://127.0.0.1:8765/v1/step?frames=1'
curl -s http://127.0.0.1:8765/v1/frame.png -o frame.png
```

JSON body is also accepted: `{"frames":5}` or `{"ms":500}`.

Under strict/offscreen step mode you must advance boot delays yourself
(e.g. `POST /v1/step?frames=200`) until `in_main_game` is true.

Without step mode the game runs in real time; HTTP input still works.

For example, capture a checkpoint, advance dialogue, then walk:

```shell
mkdir -p autoplay-captures
curl http://127.0.0.1:8765/v1/frame.png \
  -o autoplay-captures/001-before-dialogue.png
curl -X POST http://127.0.0.1:8765/v1/input/confirm/tap
curl -X POST http://127.0.0.1:8765/v1/input/down/press
sleep 1
curl -X POST http://127.0.0.1:8765/v1/input/down/release
curl http://127.0.0.1:8765/v1/frame.png \
  -o autoplay-captures/002-after-walk.png
```

An autoplay client can poll `/v1/status`, fetch a frame after `frame_id`
changes, decide its next action from the image and `/v1/state`, submit input,
optionally `POST /v1/step`, and save milestone frames. Physical keyboard input
continues to work through the same engine input path.

## Captured demo

These frames were captured from one silent offscreen run using only the HTTP
interface.

| Main menu | Opening dialogue |
| --- | --- |
| ![Main menu](../screenshots/autoplay/01-main-menu.png) | ![Opening dialogue](../screenshots/autoplay/02-opening-dialogue.png) |

| Free movement | Leaving the bedroom |
| --- | --- |
| ![Free movement after the opening conversation](../screenshots/autoplay/03-free-movement.png) | ![Leaving the starting bedroom](../screenshots/autoplay/04-left-bedroom.png) |

| Corridor encounter | Navigating toward the stairs |
| --- | --- |
| ![Encounter in the upstairs corridor](../screenshots/autoplay/05-corridor-encounter.png) | ![Autoplay navigating the upstairs corridor](../screenshots/autoplay/06-stair-navigation.png) |
