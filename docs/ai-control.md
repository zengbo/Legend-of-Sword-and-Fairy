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
| `step_mode` | `true` = 时间由你步进，必须 `POST /v1/step` 游戏才会走 |
| `ticks` | 游戏内时间（毫秒） |

### 2.2 `GET /v1/state` — 主观察（轻量，优先）

**不含**完整队伍/背包（见 `/v1/party`、`/v1/inventory`，按需请求）。  
画面 PNG 仅在状态看不懂时再用。

#### 常用字段

| 字段 | 含义 |
| --- | --- |
| `phase` | 阶段：`boot` / `dialog` / `menu` / `battle` / `scene_transition` / `overworld` |
| `dialog` | 当前对话全文（字符串），无对话为 `null` |
| `menu` | 当前菜单，无菜单为 `null` |
| `in_battle` | 是否在战斗 |
| `in_main_game` | 是否已进入正式游戏 |
| `entering_scene` | 是否正在切换场景 |
| `scene` | 场景编号 |
| `player` | 队伍位置 `[x, y]`（世界坐标） |
| `viewport` | 镜头位置 `[x, y]` |
| `party_direction` / `facing` | 朝向 0–3 / 键名 |
| `walk` | 当前位置四向是否可走一步 |
| **`map`** | **本场景**几何：方向定义、出口、机关、障碍（见下） |
| `events` | 附近对象（距离截断列表，补充细节） |
| `battle` | 战斗详情；非战斗为 `null` |
| `resources` | `{party,inventory}` 按需接口路径 |
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

#### 可行走 `walk`

```json
"walk": {"up": true, "right": false, "down": true, "left": true}
```

只对值为 `true` 的方向移动，避免撞墙。

**注意：键名是等距坐标系**，不是屏幕上下左右：

| 键 | 世界步进 |
| --- | --- |
| `up` | `(+16, -8)` |
| `right` | `(+16, +8)` |
| `down` | `(-16, +8)` |
| `left` | `(-16, -8)` |

键名是**等距世界步进**，不是屏幕像素方向。

#### 地图 `map`（本场景几何 — 事实，不是路线推荐）

```json
"map": {
  "dirs": {
    "up":    {"dx": 16, "dy": -8, "world": "(+16,-8)"},
    "right": {"dx": 16, "dy":  8, "world": "(+16,+8)"},
    "down":  {"dx":-16, "dy":  8, "world": "(-16,+8)"},
    "left":  {"dx":-16, "dy": -8, "world": "(-16,-8)"}
  },
  "coord_note": "player/events/exits 用世界坐标；一步 = dirs[key]",
  "exits": [
    {"id": 46, "kind": "touch", "pos": [1472, 1520], "dest_scene": 1, "how": "walk_into", ...}
  ],
  "mechanisms": [
    {"id": 20, "kind": "search", "role": "npc", "pos": [...], "how": "face_and_confirm", "progress": "dialog", ...}
  ],
  "obstacles": {
    "tiles": [[x, y, h], ...],
    "event_blockers": [{"id": 20, "pos": [704, 1072], "state": 2}],
    "tile_note": "tiles 为地图格 [x,y,h]…"
  }
}
```

| 字段 | 含义 |
| --- | --- |
| **`dirs`** | 按键 → 世界位移。AI 要用方向键走到某点时，**必须**按此换算（不是屏幕上下） |
| `coord_note` | 坐标系说明 |
| **`exits`** | **当前场景**的门/传送点（`dest_scene`）。迷宫只列本房间出口，不列其它房间总出口 |
| **`mechanisms`** | 本场景需走近/调查的机关、宝箱、NPC 等（非出口）。`how`：`walk_into` 或 `face_and_confirm` |
| **`obstacles.tiles`** | 当前地图阻挡格 `[tile_x, tile_y, half]`；世界约 `(x*32+h*16, y*16+h*8)` |
| **`obstacles.event_blockers`** | `state>=2` 的实体挡路（如站着的 NPC） |

规划路线：`player` + `dirs` + `obstacles` + `exits`/`mechanisms` 自行寻路；引擎**不**给 path。

#### 事件 `events[]`（附近对象 — 事实，不是推荐列表）

按距离排序，最多约 48 个。**没有**「该去哪个」的排序策略；你自己根据剧情、`progress`、`dest_scene` 等选择。

| 字段 | 含义 |
| --- | --- |
| `id` | 对象编号 |
| `kind` | `search` / `touch` / `scenery` |
| `role` | 粗分类：`npc` / `exit` / `search` / `trigger` / `decor` |
| `progress` | 脚本**性质**扫描（非优先级）：`item`/`quest`/`scene`/`dialog`/`battle`/`cash`/`mild`/`none` |
| `loop` | 可选；`true` = 当前入口像是纯对话循环（事实标注） |
| `item_use` | 可选；背包里 use 脚本会检查该事件的物品 id（可用道具的事实） |
| `pos` / `delta` / `dist` | 世界坐标、相对位移、距离度量 |
| `can_search_now` | **当前朝向**下 confirm 是否命中（引擎格匹配） |
| `in_search_range` | 任一方位下可调查 |
| `face` | 若要调查，需要面向的方向（几何事实，不是「请按」） |
| `in_touch_range` | 是否已在触碰半径内 |
| `dest_scene` | 可选；脚本会切到的场景号 |
| `trigger_script` 等 | 脚本/精灵编号（高级） |

调查与引擎一致：朝向锥 + 地图格子。mode=1 时几乎要贴格。  
如何接近、是否交互、是否用道具：**由你决定**。

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

---

### 2.5 `GET /v1/frame.png` — 画面

- 320×200 像素图  
- 可能暂时没有画面（503）：等待或先步进  
- **默认优先用 state；看不清 UI 时再取图**

### 2.6 `POST /v1/input/{key}/{action}` — 按键

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

### 2.7 `POST /v1/step` — 推进时间（仅步进模式）

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
6. **`overworld`** → 用 `player`、`map.dirs`、`map.exits` / `mechanisms` / `obstacles`、`walk`、`events[]` 自己规划；背包/队伍用 `/v1/inventory`、`/v1/party` 按需拉。  

合法键名见 `actions`。等距移动键含义见上文 `walk` 表。

---

## 4. 操作循环

### 步进模式（`step_mode == true`）

```
确认 GET /v1/status 成功
若尚未进入游戏：可 POST /v1/step?frames=200，并 confirm
循环：
  GET /v1/state
  若 quit_requested → 结束
  按第 3 节决策
  POST /v1/input/...   （先按键）
  POST /v1/step?frames=1
  长按移动：press → 多次 step → release
```

### 实时模式（`step_mode == false`）

```
循环：
  GET /v1/state
  决策并 POST /v1/input/...
  等待约 50～200 毫秒（墙钟）
不必调用 /v1/step
```

---

## 5. 示例请求

```http
GET /v1/status
GET /v1/state
GET /v1/party
GET /v1/inventory
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
