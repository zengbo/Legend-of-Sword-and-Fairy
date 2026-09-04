# 仙剑 · AI 操作手册

你是**玩家客户端**。通过本机 HTTP **观察**状态并发送按键，像真人一样推进游戏。  
默认地址：`http://127.0.0.1:8765`（仅本机）。

**原则：`/v1/state` 只提供信息，不提供策略。**  
引擎告诉你「在哪、附近有什么、对话写了什么、这一步能不能调查」；**去哪、先聊谁、怎么绕路，由你自己决定。**  
不要期望字段里有「推荐路径 / 请按 up」之类的指令。

---

## 1. 你能做什么

| 可以 | 不可以 |
| --- | --- |
| 读取游戏状态 JSON | 使用鼠标 / 触屏 |
| 读取 320×200 画面 PNG | 直接改存档或内存 |
| 按下、按住、松开键盘按键 | 一次发送多键组合以外的复杂手势 |
| 在步进模式下推进时间 | 在未开启步进时依赖 `/v1/step` 控时 |

游戏规则与真人游玩相同：对话、菜单、走路、战斗均有效。

先请求 `GET /v1/status`。若连不上，说明游戏未启动或地址不对，应停止并报告。

---

## 2. 接口

读操作成功一般为 `200`，写操作为 `202`。JSON 用 `application/json`，画面为 `image/png`。  
**忽略 JSON 中不认识的字段。**

### 2.1 `GET /v1/status` — 心跳

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

| 字段 | 用法 |
| --- | --- |
| `frame_id` | 画面更新计数；变大表示有新图 |
| `step_mode` / `step_gating` | `true` = 严格步进：时间由你 `POST /v1/step` 推进（运行时用 `/v1/config` 开关） |
| `ticks` | 游戏内时间（毫秒） |

### 2.2 `GET /v1/state` — 主观察（轻量，优先）

**不含**完整队伍/背包（见 `/v1/party`、`/v1/inventory`，按需请求）。  
画面 PNG 仅在状态看不懂时再用。

#### 常用字段

| 字段 | 含义 |
| --- | --- |
| `phase` | 阶段：`boot` / `title_menu` / `dialog` / `menu` / `battle` / `scene_transition` / `overworld` |
| `awaiting_input` | 是否在等按键（菜单/对话/未进主游戏的开场） |
| `boot_stage` | 仅未进主游戏时有：`intro`（开场动画，可用 menu/confirm 跳）/ `title_menu` |
| `dialog` | 当前对话全文（字符串），无对话为 `null` |
| `menu` | 当前菜单，无菜单为 `null` |
| `in_battle` | 是否在战斗 |
| `in_main_game` | 是否已进入正式游戏 |
| `entering_scene` | 是否正在切换场景 |
| `scene` | 场景编号 |
| `player` | 队伍位置 `[x, y]`（世界坐标） |
| **`on_grid`** | 是否在等距走步网格上（`x%16==0` 且 `y%8==0`） |
| **`grid_snap`** | 仅 `on_grid=false` 时：最近网格点 `[x,y]`（事实，非传送） |
| **`walk_from_snap`** | 仅 `on_grid=false`：在 `grid_snap` 上的四向是否可走（脱困用） |
| `viewport` | 镜头位置 `[x, y]` |
| `party_direction` / `facing` | 朝向 0–3 / 键名 |
| `walk` | 当前位置四向是否可走一步 |
| **`step_touch`** | 四向各一步后会进入哪些 touch 区（exit/door id 列表；仅 `walk` 为 true 的方向有意义） |
| **`in_touch_now`** | 当前站位已处于哪些 touch 区（含 `role`/`touch_radius`/`dest_scene?`） |
| `facing_note` | 事实：`walk` 为 false 的方向仍会改朝向（不位移） |
| `walk_span` | 从当前位置 BFS 能走到的格子数（连通区域大小） |
| `walk_blocked` / `walk_reason` / `trail` / `last_safe` | 卡住/脱困线索（见下） |
| **`last_safe_steps` / `last_safe_dir` / `last_safe_delta`** | 到 `last_safe` 的步数、几何方向、位移 |
| **`grid_snap_dir` / `grid_snap_delta`** | 仅 off_grid：朝网格点的几何方向 |
| `scene_change_exit_id` / `scene_change_from_scene` / `scene_change_dest` | 仅 `entering_scene`：**来源场景**踩到的出口（sticky），不是新场景里最近的出口 |
| **`last_scene_exit`** | sticky：`{exit_id,from_scene,dest_scene}` 上次站进的出口半径 |
| **`map`** | **本场景**几何：方向定义、出口、机关、障碍（见下） |
| `events` | 附近对象（距离截断列表，含 `walk_reachable`） |
| `battle` | 战斗详情；非战斗为 `null` |
| `resources` | `{party,inventory,obstacles}` 按需接口路径 |
| `actions` | 合法键名词汇表 |
| `cash` / `playtime_secs` / `quit_requested` / `frame_id` … | 金钱、游玩时间、退出等 |

#### 对话 `dialog`

- 有内容时：一整段字符串，例如  
  `"李大娘：李逍遙！你皮癢啊？\n敢說老娘是什麼鬼婆！"`
- 无对话：`null`
- **有对话时优先读完并按 `confirm` 推进，不要乱按方向键。**

#### 菜单 `menu`

```json
{
  "kind": "menu",
  "index": 1,
  "items": [
    {"value": 0, "label": "新的故事"},
    {"value": 1, "label": "讀取進度"}
  ]
}
```

| 字段 | 含义 |
| --- | --- |
| `kind` | 菜单类型（如 `menu` / `item` / `magic` / `battle_main` 等） |
| `index` | 当前高亮项下标 |
| `items[i].label` | 选项文字 |
| `items[i].value` | 选项值 |
| `items[i].enabled` | 仅在为 `false` 时出现（表示不可选） |

- 当前选项 = `items[index]`
- **上/下/左/右** 移动光标，**confirm** 确认，**menu** 取消

#### 可行走 `walk` 与卡住信息

```json
"walk": {"up": true, "right": false, "down": true, "left": true},
"step_touch": {"up": [], "right": [50], "down": [], "left": [47]},
"in_touch_now": [],
"walk_blocked": false,
"trail": [[640, 688], [624, 696]],
"last_safe": [640, 688],
"last_safe_steps": 2,
"last_safe_dir": "up"
```

只对 `walk` 为 `true` 的方向移动；若 `step_touch[dir]` 非空，该步会触发列出的 touch 事件。

**键名是等距世界步进**，不是屏幕上下左右：

| 键 | 世界步进 |
| --- | --- |
| `up` | `(+16, -8)` |
| `right` | `(+16, +8)` |
| `down` | `(-16, +8)` |
| `left` | `(-16, -8)` |

当四向都不可走时：

| 字段 | 含义 |
| --- | --- |
| `walk_blocked` | `true` = 当前位置一步都走不了 |
| `walk_reason` | `cornered` / `off_grid` / `dialog` / `menu` / `battle` / `scene_transition` / `boot` … |
| `trail` | 最近若干世界坐标（回溯线索） |
| `last_safe` | 轨迹上较可能还能走的位置 |
| `last_safe_steps` / `last_safe_dir` / `last_safe_delta` | 到 `last_safe` 的 BFS 步数、几何方向、位移 |
| `grid_snap_dir` / `grid_snap_delta` | off_grid 时朝 `grid_snap` 的几何方向 |
| `walk_hint` | 卡住 / off_grid 时的简短说明 |

`off_grid`：坐标落在半步（脚本推移后常见）。看 `walk_from_snap` + `grid_snap_dir` + `last_safe`/`last_safe_dir`。

#### 地图 `map`（本场景几何 — 事实，不是路线推荐）

```json
"map": {
  "dirs": { "up": {"dx":16,"dy":-8}, ... },
  "exits": [
    {"id": 46, "dest_scene": 1, "how": "walk_into", "walk_reachable": true, "walk_steps": 3, ...}
  ],
  "mechanisms": [
    {"id": 20, "role": "npc", "how": "face_and_confirm", "walk_reachable": false, "face": "down", ...}
  ],
  "obstacles": {
    "event_blockers": [{"id": 20, "pos": [704, 1072], "state": 2}],
    "tiles": "/v1/obstacles",
    "pathfind_note": "..."
  }
}
```

| 字段 | 含义 |
| --- | --- |
| **`dirs`** | 按键 → 世界位移（寻路必用） |
| **`exits`** | **仅**含会切场景的门（必有 `dest_scene`）。含 `touch_radius` / `in_touch_range` / `screen` |
| **`mechanisms`** | 非出口可互动物：`door` / `load_point` / `trigger` / `npc` 等 |
| **`walk_reachable` / `walk_steps`** | **安全图** BFS：路径不进入任何场景出口的 touch 半径；步数估计 |
| **`path_crosses_exit` / `walk_steps_any`** | 仅当安全图不可达、但无约束图可达时出现（事实：必经出口半径） |
| **`exit_detour`** | 当 `path_crosses_exit`：无约束路径会进入的 `blocking_exits[]`，每项带目标场景回本场景的 `return_exits` |
| **`return_exits`** | 每个 `map.exits[]`：`dest_scene` 里走回本场景的出口列表 |
| **`touch_radius` / `in_touch_range` / `trigger_mode`** | 所有 touch（exit 与 door）：引擎触发半径与是否已在区内 |
| **`event_state` / `solid`** | 对象状态位；`solid` 表示挡路实体 |
| **`screen`** | 相对 `viewport` 的屏幕坐标 `[x,y]`（对齐截图） |
| **`obstacles.event_blockers`** | `state>=2` 实体挡路（体量小，留在 state） |
| **`obstacles.tiles`** | 指针 `"/v1/obstacles"` — **整图阻挡格不进 state**（省 token） |

**优先用安全图的 `walk_reachable`/`walk_steps`**，不要只靠 `dist`。  
触碰公式（与引擎一致）：`dist < touch_radius` 时触发，其中 `touch_radius = (trigger_mode-4)*32+16`（`trigger_mode≥4`）。  
完整阻挡格：仅当需要自建寻路时再 `GET /v1/obstacles`。

#### 事件 `events[]`（附近对象 — 事实，不是推荐列表）

按距离排序，最多约 48 个。**没有**「该去哪个」的排序策略。

| 字段 | 含义 |
| --- | --- |
| `id` | 对象编号 |
| `kind` | `search` / `touch` |
| `role` | `exit` / `door` / `load_point` / `npc` / `trigger` |
| `label` | 可选；脚本对话称呼（如「李大娘」）；**会过滤主角/队员名** |
| `how` | `walk_into` / `face_and_confirm` |
| `progress` | 脚本性质：`item`/`quest`/`scene`/`dialog`/… |
| `loop` | 可选；像纯对话循环 |
| `item_use` | 可选；相关道具 id |
| `pos` / `screen` / `delta` / `dist` | 世界坐标、屏幕坐标、相对位移、**直线**距离 |
| **`walk_reachable` / `walk_steps`** | 安全图下是否可达、最短步数（不进出口 touch 半径） |
| `path_crosses_exit` / `walk_steps_any` / `exit_detour` | 可选；不安全路径 + 会踩到的出口 + 回程门户 |
| `can_search_now` / `in_search_range` | 仅 `search`：当前是否可调查 |
| **`facing_ok` / `need_face`** | 仅 `search`：`facing_ok`≡`can_search_now`；在 range 但朝向不对时 `need_face` |
| **`face`** | 仅当 `in_search_range`：应用该朝向再 confirm |
| **`approach_dir`** | 仅 `search`：几何上更接近**身体**的方向（**不是**站位；见 `approach_note`） |
| **`best_spot`** | 仅 `search`：优先安全站位 `{pos,face,walk_steps,safe,in_exit_touch?}` |
| **`search_spots`** | 仅 `search`：`[{pos,face,walk_steps?,walk_steps_any?,in_exit_touch,exit_id?},…]` |
| `trigger_mode` / `touch_radius` / `in_touch_range` | 仅 `touch`（exit/door 等） |
| `event_state` / `solid` | 状态位 / 是否挡路 |
| `dest_scene` | 可选；切场景目标 |

**对话/调查用法（事实驱动）**：优先 `best_spot`（`safe=true` / `in_exit_touch=false`）→ 走到 `pos` → 按 `face` 转身（`walk[face]==false` 时只改朝向）→ 若 `need_face` 先转身 → `confirm`。  
**避出门**：看 `step_touch`、`in_touch_now`、spot 上的 `in_exit_touch`；`walk_steps` 已绕开出口半径。  
**off_grid**：看 `grid_snap` + `walk_from_snap` + `last_safe`/`last_safe_dir`。

#### 战斗 `battle`（非战斗为 `null`）

| 字段 | 含义 |
| --- | --- |
| `phase` / `ui_state` / `menu_state` | 战斗流程与菜单阶段 |
| `target` | 选目标时出现：`{side,index,all}` |
| `enemies[]` | `name`、`hp`、`max_hp`、`level`、可选 `selected`、`status` |
| `players[]` | `name`、`hp`/`mp`、`defending`、可选 `selected`/`acting`、`status` |
| `force` / `flee` / `auto_attack` | 相关标志 |

选目标：`ui_state` 为 `select_target_enemy*` / `select_target_player*` 时，看 `target` 与 `selected`，用左右切换，`confirm` 确认。  
战斗中也可出现 `menu`（法术、道具）。合法键见 `actions`。

### 2.3 `GET /v1/party` — 队伍（按需）

不在 `/v1/state` 里。需要时再拉。

```json
{ "status": "ok", "frame_id": 12, "party": [ { "name": "李逍遙", "hp": 150, "max_hp": 150, "magics": [...], ... } ] }
```

| 字段 | 含义 |
| --- | --- |
| `name` / `level` / `hp` / `max_hp` / `mp` / `max_mp` | 基本属性 |
| `exp` / `next_exp` | 经验 |
| `equipment[]` / `magics[]` / `status[]` | 装备、法术、异常 |
| `screen_pos` | 屏幕坐标（**不是**世界坐标） |

### 2.4 `GET /v1/inventory` — 背包（按需）

```json
{ "status": "ok", "frame_id": 12, "inventory": [ { "item": 92, "name": "水果", "amount": 1, "tags": ["use","sell"] } ] }
```

`tags` 如 `use`、`eq`、`throw`、`consume`、`sell`。

### 2.5 `GET /v1/obstacles` — 阻挡格（按需）

**不在** `/v1/state` 里。只有你要自建整图寻路时才拉。

```json
{ "status":"ok", "format":"sparse_tiles", "tiles":[[x,y,h],...], "event_blockers":[...] }
```

日常 AI 轮询用 `walk_reachable` 即可，不必每回合读这个。

---

### 2.6 `GET /v1/frame.png` — 画面

- 320×200 像素图  
- 可能暂时没有画面（503）：等待或先步进  
- **默认优先用 state；看不清 UI 时再取图**

### 2.7 `POST /v1/input/{key}/{action}` — 按键

| action | 含义 |
| --- | --- |
| `tap` | 点按（按下再松开，最常用） |
| `press` | 按住 |
| `release` | 松开 |

**`key` 必须来自 state 的 `actions`（或下表）。** 常用：

| key | 作用 |
| --- | --- |
| `up` `down` `left` `right` | 移动 / 菜单光标 |
| `confirm` | 确认、对话、调查 |
| `space` | 调查/确认类 |
| `menu` | 打开菜单或取消 |
| `force` | 战斗中法术/强力等 |
| `auto` | 自动战斗相关 |
| `defend` | 防御 |
| `use_item` | 使用物品 |
| `throw_item` | 投掷 |
| `flee` | 逃跑 |
| `status` | 状态 |
| `repeat` | 重复 |
| `page_up` `page_down` `home` `end` | 翻页 |

别名（也可用）：`enter`/`search`→confirm；`esc`→menu；`f`→force；`a`→auto；`d`→defend；`e`→use_item；`w`→throw_item；`q`→flee；`s`→status；`r`→repeat。

**走路：** `press` 某一方向 → 保持一段时间或多次步进 → `release`。  
**步进模式下务必：先发送 input，再 step**，本拍才能读到键。

### 2.8 `POST /v1/step` — 推进时间（仅步进模式）

当 `step_mode` 为 `true` 时，游戏时间不自动走，必须步进。

| 请求 | 效果 |
| --- | --- |
| `POST /v1/step` | 前进约 1 个大地图帧（100ms） |
| `POST /v1/step?frames=N` | 前进 N 帧 |
| `POST /v1/step?ms=N` | 前进 N 毫秒 |
| 正文 `{"frames":5}` 或 `{"ms":500}` | 同上 |

- 若返回 409：当前未开启步进，不要依赖 step 控时  
- 开场若 `in_main_game` 为 false 且 `step_mode` 为 true：可先 `frames=200`～`300` 再观察 state  

响应中 `gating: true` 表示必须 step 游戏才会动。

---

## 3. 怎么玩（你自己决策）

引擎**不**给出「下一步按哪个键」。下面只是 UI 层常见情况说明，不是强制策略树：

1. **`quit_requested`** → 停止。  
2. **`dialog` 不是 null** → 对话进行中；通常用 `confirm` 翻页（你决定何时确认）。  
3. **`menu` 不是 null** → 根据 `items`/`index` 选目标；方向移动，`confirm`/`menu`。  
4. **`phase` 为 `battle`** → 看 `battle` 与菜单；按键见 `actions`。  
5. **`scene_transition` / boot** → 少操作或过片头。  
6. **`overworld`** → 用 `player`、`map.dirs`、`map.exits` / `mechanisms` / `obstacles`、`walk`、`events[]` 自己规划；**优先看 `walk_reachable`，不要只看 `dist`**。背包/队伍用 `/v1/inventory`、`/v1/party` 按需拉。  
7. **`walk_blocked`** → 看 `walk_reason` / `trail` / `last_safe`，不要对着死键空转。调试时建议开 `step_mode` 便于对齐帧。  

合法键名见 `actions`。等距移动键含义见上文 `walk` 表。

---

## 4. 操作循环

### 严格步进（`step_gating == true`，**运行时开关**）

```
# 不必启动时开 --ui-step；随时：
POST /v1/config   body: {"step_gating": true}
POST /v1/config   body: {"step_gating": false}
GET  /v1/config

开启后循环：
  GET /v1/state
  POST /v1/input/...   （先按键）
  POST /v1/step?frames=1
  长按：press → 多次 step → release
```

开场看 `boot_stage` / `awaiting_input`；intro 可按住 menu/confirm 并 step 跳过。

### 实时模式（`step_gating == false`，默认）

```
循环：
  GET /v1/state
  决策并 POST /v1/input/...
  等待约 50～200 毫秒（墙钟）
POST /v1/step → 409（需先 step_gating:true）
```

---

## 5. 示例请求

```http
GET /v1/status
GET /v1/state
GET /v1/config
GET /v1/party
GET /v1/inventory
GET /v1/obstacles
GET /v1/frame.png

POST /v1/config
{"step_gating": true}

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

## 6. 异常时怎么办

| 情况 | 做法 |
| --- | --- |
| 连接失败 | 停止，报告游戏未就绪 |
| `/v1/step` → 409 | 当前非步进模式；改用实时循环 |
| `/v1/frame.png` → 503 | 等待或先 step 再取图 |
| 按键无效果 | 检查是否在对话/菜单；步进时是否先 input 后 step；key 是否在 `actions` 中 |
| `frame_id` 不变且 step_mode | 需要 step |
| 长时间位置不动 | 换方向、调查最近 event、或 confirm |

---

## 7. 省流量与 token

- **主读 `GET /v1/state`**，不要每拍都拉 PNG。  
- 先读 `phase` + `dialog`/`menu`/`battle`/`player`/`events`，细节按需看；**不要**指望推荐路径字段。  
- `dialog` 已是完整一句/一段，直接用，不要自行拆字段。  
- 菜单用 `items[index]`，不要假设额外字段。  
- 未知字段忽略即可。
