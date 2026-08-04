# Legend of Sword and Fairy — AI Player Manual

You are a **player client**. Observe the game and send key presses over local HTTP, the same way a human would play.  
Default base URL: `http://127.0.0.1:8765` (loopback only).

**Principle: `/v1/state` is information, not strategy.**  
The engine reports where you are, nearby objects, dialogue text, and whether search would hit — **where to go and what to do is your decision.**  
Do not expect fields that say “press up” or ship a full recommended path.

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
| `events` | Nearby objects (facts: pos, search, scripts — not ranked goals) |
| `party` | Party (names, HP/MP, exp, gear, magic) |
| `inventory` | Items |
| `battle` | Battle details, or `null` |
| `actions` | **All legal key names** (input vocabulary, not suggestions) |
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

**Keys are isometric**, not screen up/down/left/right:

| key | world step |
| --- | --- |
| `up` | `(+16, -8)` |
| `right` | `(+16, +8)` |
| `down` | `(-16, +8)` |
| `left` | `(-16, -8)` |

Keys are **isometric world steps**, not screen pixels. Plan routes from `player`, `events[].pos` / `delta`, and `walk`.

Top-level `facing` is the current facing key name (`down`/`left`/`up`/`right`).

#### Events `events[]` (nearby objects — facts, not a ranked to-do list)

Sorted by distance (cap ~48). There is **no** engine-chosen “you should go here”.

| Field | Meaning |
| --- | --- |
| `id` | Object id |
| `kind` | `search` / `touch` / `scenery` |
| `role` | Coarse class: `npc` / `exit` / `search` / `trigger` / `decor` |
| `progress` | Script **character** scan (not priority): `item`/`quest`/`scene`/`dialog`/… |
| `loop` | Optional; `true` if the entry looks like pure dialog |
| `item_use` | Optional inventory item whose use-script checks this event |
| `pos` / `delta` / `dist` | World position, offset, distance metric |
| `can_search_now` | Confirm hits with **current facing** |
| `in_search_range` | Search works for some facing |
| `face` | Facing needed for search (geometry fact, not a “press this” order) |
| `in_touch_range` | Inside touch radius |
| `dest_scene` | Optional scene-change target |
| `trigger_script` etc. | Advanced ids |

Search matches the engine (facing cone + tiles). How to approach and when to act is **your** call.

#### Party `party[]`

| Field | Meaning |
| --- | --- |
| `name` / `level` / `hp` / `max_hp` / `mp` / `max_mp` | Basics |
| `exp` / `next_exp` | Current / next-level experience |
| `equipment[]` | Gear |
| `magics[]` | `id`, `name`, `mp` cost, `tgt` (`enemy`/`ally`), optional `all`, `ok:false` (can't afford), `battle:false` / `field:false` |
| `status[]` | `name`: `conf`/`para`/`sleep`/`silence`/`puppet`/`brave`/`prot`/`haste`/`dual`; `t` rounds left |
| `screen_pos` | Screen coords (**not** world; use top-level `player` for walking) |

#### Battle `battle` (`null` when not fighting)

| Field | Meaning |
| --- | --- |
| `phase` / `ui_state` / `menu_state` | Battle flow and UI stage |
| `target` | While selecting: `{side,index,all}` |
| `enemies[]` | `name`, `hp`, `max_hp`, `level`, optional `selected`, `status` |
| `players[]` | `name`, `hp`/`mp`, `defending`, optional `selected`/`acting`, `status` |
| `force` / `flee` / `auto_attack` | Flags |

On `select_target_enemy*` / `select_target_player*`, use `target` / `selected`, left/right, then `confirm`.  
A `menu` may also open (magic/items). Use `actions` for legal key names.

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

## 3. How to play (you decide)

The engine does **not** publish “press this next”. UI facts only:

1. **`quit_requested`** → stop.  
2. **`dialog` is not null** → dialogue is open; you usually `confirm` to advance.  
3. **`menu` is not null** → use `items`/`index`; arrows, `confirm`, `menu`.  
4. **`phase` is `battle`** → use `battle` + menus; keys in `actions`.  
5. **`scene_transition` / boot** → few inputs or skip intros.  
6. **`overworld`** → plan from `player`, `walk`, `events[]`, `inventory`, and your own story goals.

Legal key names: `actions`. Isometric move keys: see `walk` table above.

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
- Read `phase` + `dialog`/`menu`/`battle`/`player`/`events` first; there is no recommended-path field.  
- `dialog` is already one full string — use it as-is.  
- Menu selection is `items[index]` — no extra label field.  
- Ignore unknown fields.
