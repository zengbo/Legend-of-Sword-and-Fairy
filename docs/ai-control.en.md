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
| `events` | Nearby events (`role`, `keys`, …) |
| `nav` | Recommended navigation target + path; `null` if none |
| `hint` | One-line natural-language advice (read first) |
| `party` | Party (names, HP/MP, exp, gear, magic) |
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

**Keys are isometric**, not screen up/down/left/right:

| key | world step |
| --- | --- |
| `up` | `(+16, -8)` |
| `right` | `(+16, +8)` |
| `down` | `(-16, +8)` |
| `left` | `(-16, -8)` |

Do not map `delta` signs to screen arrows. Use `nav` / `events[].keys` / `hint`.

#### Navigation `nav` (prefer this on the overworld)

```json
"nav": {
  "event": 16,
  "role": "exit",
  "dist": 960,
  "can_act": false,
  "progress": "item",
  "keys": ["up"],
  "steps": 12,
  "path": ["up", "right", "up"],
  "reachable": true
}
```

| Field | Meaning |
| --- | --- |
| `event` | Recommended event id |
| `role` | Coarse class: `npc` / `exit` / `search` / `trigger` / `decor` |
| `progress` | Script rank: `item` / `quest` / `scene` / `dialog` / `battle` / `cash` / `mild` / `none` |
| `item_use` | Optional inventory item id to **use** on this event (menu → item → use) |
| `can_act` | Confirm works **with current facing**, or in touch range |
| `in_search_range` | Inside search tiles (may need to turn) |
| `face` | Face this key, then `confirm` |
| `key` | **Single** next walk key (stable; avoids left/right flip) |
| `keys` | One-element array = `[key]` (compat) |
| `path` | Short BFS path; press `path[0]` (= `key`) |
| `reachable` | Whether BFS found a path |
| `dest_scene` | Scene change target if the script teleports |

Top-level `facing` is the key name for current facing (`down`/`left`/`up`/`right`).

**Target selection (engine already ranks `nav` this way):**  
Prefer `item_use` / scripts that grant items or mutate quest state → scene exits → other; **pure dialog loops** are deprioritized; **walk-unreachable targets are skipped** (e.g. kitchen cannot path to upstairs delivery — nav points at a reachable door with `progress=scene`/`bridge` instead of spinning on `reachable:false`).

- `nav.item_use` set → **use that item from the menu** on the target (do not only confirm dialog)  
- `can_act` → `confirm` / `space` now  
- `in_search_range` + `face` → tap `face`, then `confirm`  
- else press **only** `key` / `path[0]` — do not alternate directions  
- no target → `nav` is `null`  
- if still stuck: pick `events[]` with `progress` in `item`/`quest`/`scene` and no `loop`

#### Natural-language `hint`

One line per turn, e.g.:

- `"dialog — confirm (…)"`
- `"go to #16 (exit/item) path=up>right… — press up"`
- `"use item 272(…) on #63 (npc/item) — menu→item→use, face target"`
- `"at event #68 (search/quest) — confirm/space to interact"`
- `"select enemy target index=1 (…) — left/right, confirm"`

**Read `hint` first, then drill into fields.**

#### Events `events[]` (NPCs, exits, inspectables)

| Field | Meaning |
| --- | --- |
| `id` | Object id |
| `kind` | `search` / `touch` / `scenery` |
| `role` | Coarse class (same as `nav.role`) |
| `progress` | Script rank (same labels as `nav.progress`) |
| `loop` | Optional; `true` = pure dialog loop (safe to skip) |
| `item_use` | Optional usable item id for this event |
| `pos` / `delta` / `dist` | Position and distance |
| `can_search_now` | Confirm hits with **current facing** (engine tile match) |
| `in_search_range` | Search works for some facing |
| `face` | Required facing key |
| `in_touch_range` | Inside touch trigger radius |
| `key` | Single preferred walk key toward this event |
| `dest_scene` | Optional scene-change target |
| `trigger_script` etc. | Advanced script/sprite ids |

Search uses the engine’s facing cone + map tiles. mode=1 needs you almost on the same tile.  
`can_search_now` / `in_touch_range` → `confirm`/`space`; only `face` → turn first, then confirm.  
Table dishes / delivery stairs may be `search` or sprite-less `exit`/`touch` — follow `progress` and `nav`, not a “table” label.

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
   - Read `hint` / `nav` first  
   - Read `hint` / `nav` first (`nav` prefers `item`/`quest` and deprioritizes dialog loops)  
   - `nav.item_use` set → **menu-use that item** on `nav.event`  
   - `nav.can_act` → `confirm`/`space`  
   - `nav.in_search_range` + `face` → tap `face`, then `confirm`  
   - else press **only** `nav.key` (= `path[0]`); hold that one direction until `can_act` / `hint` changes  
   - no `nav`: explore only where `walk` is true  
   - if still spinning: pick `events[]` with `progress` in `item`/`quest`/`scene` and no `loop`

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
- Read `hint` + `phase` + `nav`/`dialog`/`menu`/`battle` first; open other fields only as needed.  
- `dialog` is already one full string — use it as-is.  
- Menu selection is `items[index]` — no extra label field.  
- Ignore unknown fields.
