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
| `phase` | `boot` / `title_menu` / `dialog` / `menu` / `battle` / `scene_transition` / `overworld` |
| `awaiting_input` | Waiting for a key (menu/dialog, or pre-game intro) |
| `boot_stage` | Only before main game: `intro` (skippable) / `title_menu` |
| `dialog` | Full dialogue text (string), or `null` |
| `menu` | Active menu object, or `null` |
| `in_battle` | Whether a battle is active |
| `in_main_game` | Whether the main game has started (past intro/title) |
| `entering_scene` | Scene transition in progress |
| `scene` | Scene number |
| `player` | Party world position `[x, y]` |
| **`on_grid`** | On walk grid (`x%16==0` and `y%8==0`) |
| **`grid_snap`** | Only when `on_grid=false`: nearest grid point (fact, not a teleport) |
| **`walk_from_snap`** | Only when `on_grid=false`: four-way walk from `grid_snap` (recovery) |
| `viewport` | Camera world position `[x, y]` |
| `party_direction` | Facing 0–3 |
| `walk` | Can take one step from here: `{up,right,down,left}` |
| **`step_touch`** | Per-dir list of touch zone ids entered after one legal step |
| **`in_touch_now`** | Touch zones covering the player right now |
| `facing_note` | Fact: pressing a `false` walk dir still changes facing (no move) |
| `walk_span` | Cells reachable by engine BFS from here (connected-component size) |
| `walk_blocked` / `walk_reason` / `trail` / `last_safe` | Stuck / recovery facts |
| **`last_safe_steps` / `last_safe_dir` / `last_safe_delta`** | Graph steps / geometric dir / delta to `last_safe` |
| **`grid_snap_dir` / `grid_snap_delta`** | Off-grid only: geometric nudge toward grid |
| `scene_change_exit_id` / `scene_change_from_scene` / `scene_change_dest` | Only while `entering_scene`: **source** exit that caused the hop (sticky), not nearest exit in the new scene |
| **`last_scene_exit`** | Sticky `{exit_id,from_scene,dest_scene}` of the last exit radius you stood in |
| **`map`** | **This scene**: `dirs`, `exits`, `mechanisms`, `obstacles` (see below) |
| `events` | Nearby objects (incl. `walk_reachable`) |
| `battle` | Battle details, or `null` |
| `resources` | `{party,inventory}` on-demand paths |
| `actions` | Legal key names (vocabulary, not suggestions) |
| `cash` | Money |
| `playtime_secs` | Cumulative real-world play seconds for this save lineage (includes open session) |
| `quit_requested` | Stop when true |
| `frame_id` / `ticks` / `step_mode` / `step_gating` | Clock; `step_gating` toggled via `POST /v1/config` |

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

#### Walking `walk` and stuck diagnostics

```json
"walk": {"up": true, "right": false, "down": true, "left": true},
"walk_blocked": false,
"trail": [[640, 688], [624, 696]],
"last_safe": [640, 688]
```

Only move in directions that are `true`.

**Keys are isometric world steps**, not screen pixels:

| key | world step |
| --- | --- |
| `up` | `(+16, -8)` |
| `right` | `(+16, +8)` |
| `down` | `(-16, +8)` |
| `left` | `(-16, -8)` |

When every direction is blocked:

| Field | Meaning |
| --- | --- |
| `walk_blocked` | `true` if no one-step move is legal |
| `walk_reason` | `cornered` / `off_grid` / `dialog` / `menu` / `battle` / `scene_transition` / … |
| `trail` | Recent world positions (backtrack facts) |
| `last_safe` | Trail cell that still looks walkable when possible |
| `last_safe_steps` / `last_safe_dir` / `last_safe_delta` | BFS steps / geometric dir / delta to `last_safe` |
| `grid_snap_dir` / `grid_snap_delta` | Off-grid: geometric dir toward `grid_snap` |
| `walk_hint` | Short recovery note when stuck / off-grid |

`off_grid`: use `walk_from_snap` + `grid_snap_dir` + `last_safe` / `last_safe_dir`.

#### Map `map` (this scene only — geometry facts)

| Field | Meaning |
| --- | --- |
| **`dirs`** | Key → world `{dx,dy}` (required for path planning) |
| **`exits`** | Scene-changing doors **only** (`dest_scene`, `touch_radius`, `in_touch_range`, `screen`) |
| **`mechanisms`** | Non-exit interactables: `door`, `load_point`, `trigger`, `npc`, … |
| **`walk_reachable` / `walk_steps`** | **Safe-graph** BFS: paths avoid every scene-exit touch radius |
| **`path_crosses_exit` / `walk_steps_any`** | Only when safe graph fails but unrestricted graph succeeds |
| **`exit_detour`** | When `path_crosses_exit`: `blocking_exits[]` on the unrestricted path, each with `return_exits` in the dest scene back here |
| **`return_exits`** | On each `map.exits[]` entry: portals in `dest_scene` that walk back to this scene |
| **`touch_radius` / `in_touch_range` / `trigger_mode`** | All touch (exits + doors): engine fire radius |
| **`event_state` / `solid`** | Raw object state; solid blocker body |
| **`screen`** | Screen coords relative to `viewport` |
| **`obstacles.event_blockers`** | Solid NPCs/objects (`state>=2`) — kept small in state |
| **`obstacles.tiles`** | Pointer `"/v1/obstacles"` — **full blocked grid is not in state** (token cost) |

**Prefer safe-graph `walk_reachable`/`walk_steps` over raw `dist`.**  
Touch fires when `dist < touch_radius` with `touch_radius = (trigger_mode-4)*32+16` (`trigger_mode≥4`).  
Full tile list only via `GET /v1/obstacles` when needed.

#### Events `events[]` (nearby objects — facts, not a ranked to-do list)

Sorted by distance (cap ~48). There is **no** engine-chosen “you should go here”.

| Field | Meaning |
| --- | --- |
| `id` | Object id |
| `kind` | `search` / `touch` |
| `role` | `exit` / `door` / `load_point` / `npc` / `trigger` |
| `label` | Optional speaker name from dialog script; **party/playable names filtered out** |
| `how` | `walk_into` / `face_and_confirm` |
| `progress` | Script character: `item`/`quest`/`scene`/`dialog`/… |
| `loop` | Optional; pure dialog loop lookalike |
| `item_use` | Optional related inventory item |
| `pos` / `screen` / `delta` / `dist` | World pos, screen pos, offset, straight-line metric |
| **`walk_reachable` / `walk_steps`** | Safe-graph reachability (avoids exit touch radii) |
| `path_crosses_exit` / `walk_steps_any` / `exit_detour` | Optional; only-unsafe path + which exits it enters + return portals |
| `can_search_now` / `in_search_range` | Search only: can inspect from here |
| **`facing_ok` / `need_face`** | Search only: facing matches / must turn before confirm |
| **`face`** | Only when `in_search_range`: facing that makes confirm hit |
| **`approach_dir`** | Search only: geometric closer-step toward **body** (see `approach_note`) |
| **`best_spot`** | Search only: preferred stand cell (safe first) |
| **`search_spots`** | Search only: `[{pos,face,walk_steps?,in_exit_touch,exit_id?},…]` |
| `trigger_mode` / `touch_radius` / `in_touch_range` | Touch only (exits/doors) |
| `event_state` / `solid` | Object state / blocker body |
| `dest_scene` | Optional scene-change target |

**Inspect flow (fact-driven):** prefer `best_spot` with `safe=true` → walk to `pos` → face `face` → if `need_face`, turn → `confirm`.  
**Avoid exits:** check `step_touch`, `in_touch_now`, and `in_exit_touch` on spots; `walk_steps` already avoids exit radii.  
**off_grid:** use `grid_snap` + `walk_from_snap` + `last_safe` / `last_safe_dir`.

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

### 2.3 `GET /v1/party` — party (on demand)

Not included in `/v1/state`. Fields: name, HP/MP, exp, equipment, magics, status, `screen_pos` (screen only).

### 2.4 `GET /v1/inventory` — inventory (on demand)

`item`, `name`, `amount`, `tags` (`use`/`eq`/`throw`/…).

### 2.5 `GET /v1/obstacles` — blocked tiles (on demand)

**Not** in `/v1/state`. Pull only if you build your own full-map pathfinder.

```json
{ "status":"ok", "format":"sparse_tiles", "tiles":[[x,y,h],...], "event_blockers":[...] }
```

For normal play loops, `walk_reachable` on state is enough.

---

### 2.6 `GET /v1/frame.png` — screen image

- 320×200 pixels  
- May return 503 if no frame yet: wait or step first  
- **Prefer state; fetch the image only when needed**

### 2.7 `POST /v1/input/{key}/{action}` — keys

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

### 2.8 `POST /v1/step` — advance time (step mode only)

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
6. **`overworld`** → plan from `player`, `map.dirs` / `exits` / `mechanisms` / `obstacles`, `walk`, `events[]` (use **`walk_reachable`**, not only `dist`); fetch `/v1/party` and `/v1/inventory` only when needed.  
7. **`walk_blocked`** → read `walk_reason` / `trail` / `last_safe`; do not spin on dead keys.

Legal key names: `actions`. Isometric move keys: see `walk` table above.  
**Tip:** for precise AI control, prefer `step_mode` so each input aligns with a frame.

---

## 4. Control loops

### Strict step (`step_gating == true`, **runtime toggle**)

```
POST /v1/config   {"step_gating": true}
POST /v1/config   {"step_gating": false}
GET  /v1/config

When gating on:
  GET /v1/state
  POST /v1/input/...     (keys first)
  POST /v1/step?frames=1
```

No need for `--ui-step` at launch. Optional startup default: `RUSTPAL_UI_STEP_STRICT=1`.

### Real-time (`step_gating == false`, default)

```
Loop:
  GET /v1/state
  POST /v1/input/...
  Wait ~50–200 ms wall clock
POST /v1/step → 409 until step_gating is enabled
```

---

## 5. Example requests

```http
GET /v1/status
GET /v1/state
GET /v1/party
GET /v1/inventory
GET /v1/obstacles
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
curl -s http://127.0.0.1:8765/v1/party
curl -s http://127.0.0.1:8765/v1/inventory
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
