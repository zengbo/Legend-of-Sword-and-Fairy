# Legend of Sword and Fairy — AI Player Manual

You are a **player client**. Observe the game and send key presses over local HTTP, the same way a human would play.  
Default base URL: `http://127.0.0.1:8765` (loopback only).

---

## 1. What you can do

| Allowed | Not allowed |
| --- | --- |
| Read structured game state (JSON) | Mouse / touch input |
| Read a 320×200 PNG frame | Editing saves or memory |
| Tap, hold, or release keys | Arbitrary multi-touch gestures |
| Advance time in step mode | Relying on `/v1/step` when step mode is off |

Game rules match a human session: dialogue, menus, walking, and battles all work.

Start with `GET /v1/status`. If the connection fails, the game is not ready — stop and report.

---

## 2. API

Successful reads are usually `200`; writes `202`. JSON is `application/json`; frames are `image/png`.  
**Ignore unknown JSON fields.**

### 2.1 `GET /v1/status` — heartbeat

```json
{
  "status": "ok",
  "width": 320,
  "height": 200,
  "frame_id": 12,
  "step_mode": true,
  "step_configured": true,
  "ticks": 1200
}
```

| Field | Meaning |
| --- | --- |
| `frame_id` | Increases when a new frame is presented |
| `step_mode` | `true` = time only advances when you call `POST /v1/step` |
| `ticks` | In-game time in milliseconds |

### 2.2 `GET /v1/state` — primary observation

Prefer this every turn. Use the PNG only when the state is not enough.

#### Common fields

| Field | Meaning |
| --- | --- |
| `phase` | `boot` / `dialog` / `menu` / `battle` / `scene_transition` / `overworld` |
| `dialog` | Full dialogue text (string), or `null` |
| `menu` | Active menu object, or `null` |
| `in_battle` | Whether a battle is active |
| `in_main_game` | Whether the main game has started (past intro/title) |
| `entering_scene` | Scene transition in progress |
| `scene` | Scene number |
| `player` | Party world position `[x, y]` |
| `viewport` | Camera world position `[x, y]` |
| `party_direction` | Facing 0–3 |
| `walk` | Can take one step: `{up,right,down,left}` |
| `events` | Nearby interactive objects |
| `party` | Party members (names, HP/MP, gear, magic) |
| `inventory` | Items |
| `battle` | Battle details, or `null` |
| `keys_hint` | Suggested keys for the current phase |
| `actions` | **All legal key names** (only use these) |
| `cash` | Money |
| `playtime_secs` | Cumulative real-world play seconds for this save lineage (includes open session) |
| `quit_requested` | Stop when true |
| `frame_id` / `ticks` / `step_mode` | Same idea as status |

#### Dialogue `dialog`

- When present: one string, e.g.  
  `"Madam Li: Li Xiaoyao! Looking for a thrashing?\nHow dare you call me that!"`
- When none: `null`
- **If dialogue is present, read it and press `confirm`. Do not move the cursor with arrow keys.**

#### Menu `menu`

```json
{
  "kind": "menu",
  "index": 1,
  "items": [
    {"value": 0, "label": "New Game"},
    {"value": 1, "label": "Load Game"}
  ]
}
```

| Field | Meaning |
| --- | --- |
| `kind` | Menu type (`menu` / `item` / `magic` / `battle_main` / …) |
| `index` | Highlighted item index |
| `items[i].label` | Option text |
| `items[i].value` | Option value |
| `items[i].enabled` | Only present when `false` (disabled) |

- Current choice = `items[index]`
- **Arrow keys** move the cursor; **confirm** accepts; **menu** cancels

#### Walking `walk`

```json
"walk": {"up": true, "right": false, "down": true, "left": true}
```

Only move in directions that are `true`.

#### Events `events[]` (NPCs, exits, inspectables)

| Field | Meaning |
| --- | --- |
| `id` | Object id |
| `kind` | `search` / `touch` / `scenery` |
| `pos` | World position |
| `delta` | Offset from you |
| `dist` | Distance (smaller = closer) |
| `can_search_now` | Close enough to inspect |
| `in_touch_range` | Inside touch trigger radius |

Move toward a target using `delta` and allowed `walk` directions.  
When `can_search_now` is true, press `confirm` or `space`.

#### Battle `battle` (`null` when not fighting)

| Field | Meaning |
| --- | --- |
| `phase` / `ui_state` / `menu_state` | Battle flow and UI stage |
| `enemies[]` | Enemy `name`, `hp`, `level`, … |
| `players[]` | Party battle state |
| `force` / `flee` / `auto_attack` | Flags |

A `menu` may also open (magic/items). Use `keys_hint` and `actions`.

#### Inventory `inventory[]`

Includes `item`, `name`, `amount`; capability tags in `tags` (e.g. `use`, `eq`, `throw`).

---

### 2.3 `GET /v1/frame.png` — screen image

- 320×200 pixels  
- May return 503 if no frame yet: wait or step first  
- **Prefer state; fetch the image only when needed**

### 2.4 `POST /v1/input/{key}/{action}` — keys

| action | Meaning |
| --- | --- |
| `tap` | Press and release (most common) |
| `press` | Hold down |
| `release` | Release |

**`key` must be from state `actions` (or the table below).**

| key | Role |
| --- | --- |
| `up` `down` `left` `right` | Move / menu cursor |
| `confirm` | Confirm, dialogue, inspect |
| `space` | Inspect / confirm-like |
| `menu` | Open menu or cancel |
| `force` | Battle magic / strong action |
| `auto` | Auto-battle related |
| `defend` | Defend |
| `use_item` | Use item |
| `throw_item` | Throw item |
| `flee` | Flee |
| `status` | Status |
| `repeat` | Repeat |
| `page_up` `page_down` `home` `end` | Paging |

Aliases: `enter`/`search`→confirm; `esc`→menu; `f`→force; `a`→auto; `d`→defend; `e`→use_item; `w`→throw_item; `q`→flee; `s`→status; `r`→repeat.

**Walking:** `press` a direction → hold (time or several steps) → `release`.  
**In step mode: send input first, then step**, so the press is seen this beat.

### 2.5 `POST /v1/step` — advance time (step mode only)

When `step_mode` is `true`, the clock does not run by itself.

| Request | Effect |
| --- | --- |
| `POST /v1/step` | About one overworld frame (100 ms) |
| `POST /v1/step?frames=N` | N frames |
| `POST /v1/step?ms=N` | N milliseconds |
| Body `{"frames":5}` or `{"ms":500}` | Same |

- 409 → step mode is not enabled; use a real-time loop  
- At boot, if `in_main_game` is false and `step_mode` is true: try `frames=200`–`300`, then re-read state  

Response field `gating: true` means the game waits for your steps.

---

## 3. How to play (decision order)

Each turn, decide in this priority:

1. **`quit_requested`** → stop.  
2. **`dialog` is not null** → read it, press `confirm`.  
3. **`menu` is not null** → choose from `items`; arrows to move, `confirm` to accept, `menu` to cancel.  
4. **`in_battle` or `phase` is `battle`** → use `battle` and any open `menu`; often `force` / `auto` / `confirm` / `defend`.  
5. **`entering_scene` or `phase` is `scene_transition`** → few inputs; step or wait briefly.  
6. **`phase` is `boot` or `in_main_game` is false** → use `confirm` for intro/title; in step mode also advance many steps.  
7. **Overworld**  
   - Interact: pick a nearby entry in `events`; if `can_search_now`, `confirm`/`space`; else walk using `delta` and `walk`  
   - Explore: only move where `walk` is true  

Use `keys_hint` as soft guidance; **`actions` is the hard list of legal keys.**

---

## 4. Control loops

### Step mode (`step_mode == true`)

```
GET /v1/status — must succeed
If not yet in main game: optional POST /v1/step?frames=200 and confirm
Loop:
  GET /v1/state
  If quit_requested → exit
  Decide using section 3
  POST /v1/input/...     (keys first)
  POST /v1/step?frames=1
  Hold-to-walk: press → several steps → release
```

### Real-time mode (`step_mode == false`)

```
Loop:
  GET /v1/state
  Decide and POST /v1/input/...
  Wait ~50–200 ms wall clock
Do not rely on /v1/step
```

---

## 5. Example requests

```http
GET /v1/status
GET /v1/state
GET /v1/frame.png

POST /v1/input/confirm/tap
POST /v1/input/down/press
POST /v1/input/down/release

POST /v1/step
POST /v1/step?frames=1
POST /v1/step?ms=500
```

```bash
curl -s http://127.0.0.1:8765/v1/state
curl -s -X POST http://127.0.0.1:8765/v1/input/confirm/tap
curl -s -X POST 'http://127.0.0.1:8765/v1/step?frames=1'
```

---

## 6. When things go wrong

| Situation | What to do |
| --- | --- |
| Connection failed | Stop; report game not ready |
| `/v1/step` → 409 | Not in step mode; use real-time loop |
| `/v1/frame.png` → 503 | Wait or step, then retry |
| Keys do nothing | Check dialogue/menu; in step mode, input before step; key must be in `actions` |
| `frame_id` stuck and `step_mode` | You must step |
| Stuck in place | Change direction, inspect nearest event, or confirm |

---

## 7. Saving bandwidth and tokens

- **Prefer `GET /v1/state`**; do not fetch PNG every turn.  
- `dialog` is already one full string — use it as-is.  
- Menu selection is `items[index]` — no extra label field.  
- Ignore unknown fields.
