# 仙剑 · AI 操作手册

你是**玩家客户端**。通过本机 HTTP 观察状态并发送按键，像真人一样推进游戏。  
默认地址：`http://127.0.0.1:8765`（仅本机）。

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

### 2.2 `GET /v1/state` — 主观察（优先）

每帧尽量只依赖本接口。画面 PNG 仅在状态看不懂时再用。

#### 常用字段

| 字段 | 含义 |
| --- | --- |
| `phase` | 阶段：`boot` / `dialog` / `menu` / `battle` / `scene_transition` / `overworld` |
| `dialog` | 当前对话全文（字符串），无对话为 `null` |
| `menu` | 当前菜单，无菜单为 `null` |
| `in_battle` | 是否在战斗 |
| `in_main_game` | 是否已进入正式游戏（过了开场片头/主菜单后多为 true） |
| `entering_scene` | 是否正在切换场景 |
| `scene` | 场景编号 |
| `player` | 队伍位置 `[x, y]`（世界坐标） |
| `viewport` | 镜头位置 `[x, y]` |
| `party_direction` | 朝向 0–3 |
| `walk` | 四向是否可走一步：`{up,right,down,left}` |
| `events` | 附近可交互对象列表（含 `role`、`keys`） |
| `nav` | 推荐导航目标与路径；无目标时为 `null` |
| `hint` | 一句自然语言提示（优先阅读） |
| `party` | 队员（姓名、HP/MP、经验、装备、法术等） |
| `inventory` | 物品栏 |
| `battle` | 战斗详情；非战斗为 `null` |
| `keys_hint` | 当前更建议使用的键 |
| `actions` | **全部合法键名**（按键只能用这里的名字） |
| `cash` | 金钱 |
| `playtime_secs` | 本存档累计真实游玩秒数（含当前会话） |
| `quit_requested` | 为 true 时停止操作 |
| `frame_id` / `ticks` / `step_mode` | 同 status |

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

不要根据 `delta` 的正负直接猜屏幕方向；用下面的 `nav` / `events[].keys` / `hint`。

#### 导航 `nav`（大地图优先看）

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

| 字段 | 含义 |
| --- | --- |
| `event` | 推荐接近/交互的事件 id |
| `role` | 粗分类：`npc` / `exit` / `search` / `trigger` / `decor` |
| `progress` | 脚本进度等级：`item` / `quest` / `scene` / `dialog` / `battle` / `cash` / `mild` / `none` |
| `item_use` | 可选；背包中可对该事件「使用」的物品 id（需菜单→物品→使用） |
| `can_act` | 当前朝向已可调查，或已在触碰范围 |
| `in_search_range` | 已进入调查格（可能还要转身） |
| `face` | 需要面向的方向键；先 tap 该键再 `confirm` |
| `key` | **唯一**推荐下一步方向（稳定，避免左右抖） |
| `keys` | 与 `key` 相同的单元素数组（兼容旧客户端） |
| `path` | 短路径；有则按 `path[0]`（= `key`）走 |
| `reachable` | BFS 是否找到路径 |
| `dest_scene` | 若脚本会切场景，目标场景号 |

顶层还有 `facing`：当前朝向对应的键名（`down`/`left`/`up`/`right`）。

**选目标规则（引擎已按此排序 `nav`）：**  
优先 `item_use` / 会给物品·改状态的 `quest`·`item` 脚本 → 切场景 `scene` → 其它；**纯对话循环 `dialog`（`events[].loop=true`）会被降权**，避免卡在婶婶等重复台词 NPC。

- `nav.item_use` 有值 → **菜单使用该物品**对准目标（不要只 confirm 对话）  
- `can_act` → 立刻 `confirm` / `space`  
- `in_search_range` 且有 `face` → 先 tap `face` 转身，再 `confirm`  
- 否则 **只按 `key`（或 `path[0]`）一个方向**，不要在多个方向间切换  
- 无目标时 `nav` 为 `null`  
- 若仍空转：可读 `events[]` 里 `progress!=dialog` 且无 `loop` 的目标，用其 `key` 走（一般不必，`nav` 已避开循环）

#### 自然语言 `hint`

每拍一句，例如：

- `"dialog — confirm (李大娘：李逍遙！…)"`
- `"go to #16 (exit/item) path=up>right… — press up"`
- `"use item 272(桂花酒) on #63 (npc/item) — menu→item→use, face target"`
- `"at event #68 (search/quest) — confirm/space to interact"`
- `"select enemy target index=1 (蛇妖) — left/right, confirm"`

**可先读 `hint`，再读细节字段。**

#### 事件 `events[]`（找人、出口、调查）

| 字段 | 含义 |
| --- | --- |
| `id` | 对象编号 |
| `kind` | `search` / `touch` / `scenery` |
| `role` | 粗分类（同上） |
| `progress` | 脚本进度：`item`/`quest`/`scene`/`dialog`/…（同 `nav`） |
| `loop` | 可选；`true` 表示当前脚本是纯对话循环，可跳过 |
| `item_use` | 可选；可用物品 id |
| `pos` / `delta` / `dist` | 位置与距离 |
| `can_search_now` | **当前朝向**下按 confirm 能否命中（引擎格匹配） |
| `in_search_range` | 任一方位下可调查 |
| `face` | 需要面向的方向 |
| `in_touch_range` | 是否已在触碰范围内 |
| `key` | 走近该事件的**单一**推荐方向 |
| `dest_scene` | 可选；脚本切场景目标 |
| `trigger_script` 等 | 脚本/精灵编号（高级） |

调查判定与引擎一致：朝向锥 + 地图格子。mode=1 时几乎要站在目标格旁。  
`can_search_now` / `in_touch_range` → `confirm`/`space`；仅有 `face` → 先转身再确认。  
**桌上酒菜 / 楼梯送菜** 等可能是 `search` 或无精灵的 `exit`/`touch`，不一定叫「桌子」；看 `progress=item|quest` 与 `nav` 即可。

#### 队伍 `party[]`

| 字段 | 含义 |
| --- | --- |
| `name` / `level` / `hp` / `max_hp` / `mp` / `max_mp` | 基本属性 |
| `exp` / `next_exp` | 当前经验 / 升级所需经验 |
| `equipment[]` | 装备 |
| `magics[]` | 法术：`id`、`name`、`mp`（消耗）、`tgt`（`enemy`/`ally`）、可选 `all`、`ok:false`（MP 不够）、`battle:false` / `field:false` |
| `status[]` | 异常：`name` 为 `conf`/`para`/`sleep`/`silence`/`puppet`/`brave`/`prot`/`haste`/`dual`，`t` 为剩余回合 |
| `screen_pos` | 屏幕坐标（**不是**世界坐标；走路用顶层 `player`） |

#### 战斗 `battle`（非战斗为 `null`）

| 字段 | 含义 |
| --- | --- |
| `phase` / `ui_state` / `menu_state` | 战斗流程与菜单阶段 |
| `target` | 选目标时出现：`{side,index,all}` |
| `enemies[]` | `name`、`hp`、`max_hp`、`level`、可选 `selected`、`status` |
| `players[]` | `name`、`hp`/`mp`、`defending`、可选 `selected`/`acting`、`status` |
| `force` / `flee` / `auto_attack` | 相关标志 |

选目标：`ui_state` 为 `select_target_enemy*` / `select_target_player*` 时，看 `target` 与 `selected`，用左右切换，`confirm` 确认。  
战斗中也可出现 `menu`（法术、道具）。结合 `keys_hint` 与 `actions`。

#### 物品 `inventory[]`

含 `item`、`name`、`amount`；可用标签在 `tags` 中（如 `use`、`eq`、`throw`）。

---

### 2.3 `GET /v1/frame.png` — 画面

- 320×200 像素图  
- 可能暂时没有画面（503）：等待或先步进  
- **默认优先用 state；看不清 UI 时再取图**

### 2.4 `POST /v1/input/{key}/{action}` — 按键

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

### 2.5 `POST /v1/step` — 推进时间（仅步进模式）

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

## 3. 怎么玩（决策顺序）

每一拍按下面优先级判断：

1. **`quit_requested`** → 停止。  
2. **`dialog` 不是 null** → 阅读内容，`confirm`。  
3. **`menu` 不是 null** → 根据 `items` 与目标选择；方向移动光标，`confirm` 确认，`menu` 取消。  
4. **`in_battle` 或 `phase` 为 `battle`** → 看 `battle` 与可能的 `menu`；常用 `force` / `auto` / `confirm` / `defend`。  
5. **`entering_scene` 或 `phase` 为 `scene_transition`** → 少操作，步进或短暂等待。  
6. **`phase` 为 `boot` 或 `in_main_game` 为 false** → 多用 `confirm` 过片头/主菜单；步进模式下配合大量 `step`。  
7. **大地图 `overworld`**  
   - 先读 `hint` / `nav`（`nav` 已优先 `item`/`quest`，并避开 `dialog` 循环 NPC）  
   - `nav.item_use` 有值 → **菜单使用该物品**对准 `nav.event`（如桂花酒对醉道士）  
   - `nav.can_act` → `confirm`/`space`  
   - `nav.in_search_range` + `face` → tap `face` 转身 → `confirm`  
   - 否则 **只** 按 `nav.key`（= `path[0]`）一个方向；长按该方向直到 `can_act` 或 `hint` 变化  
   - 无 `nav`：只在 `walk` 为 true 的方向探索  
   - 仍空转时：在 `events[]` 选 `progress` 为 `item`/`quest`/`scene` 且无 `loop` 的目标  

`keys_hint` 可作辅助，**以 `actions` 为合法键范围**。

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
- 先读 `hint` + `phase` + `nav`/`dialog`/`menu`/`battle`，细节字段按需看。  
- `dialog` 已是完整一句/一段，直接用，不要自行拆字段。  
- 菜单用 `items[index]`，不要假设额外字段。  
- 未知字段忽略即可。
