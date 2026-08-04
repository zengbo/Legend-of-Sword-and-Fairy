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
  "event": 54,
  "role": "exit",
  "dist": 256,
  "can_act": false,
  "keys": ["up"],
  "steps": 5,
  "path": ["up", "up", "right", "up", "right"],
  "reachable": true,
  "dest_scene": 5
}
```

| 字段 | 含义 |
| --- | --- |
| `event` | 推荐接近/交互的事件 id |
| `role` | 粗分类：`npc` / `exit` / `search` / `trigger` / `decor` |
| `can_act` | 已可调查或已在触碰范围 |
| `keys` | 当前一步应优先按的方向（已过滤不可走） |
| `path` | 短路径（最多约 16 步）；有则按 `path[0]` 走 |
| `reachable` | BFS 是否找到路径；`false` 时换目标或绕路 |
| `dest_scene` | 若脚本会切场景，目标场景号 |

- `can_act == true` → 立刻 `confirm` / `space`  
- 否则优先 `path[0]`，没有 path 再用 `keys[0]`  
- 无目标时 `nav` 为 `null`

#### 自然语言 `hint`

每拍一句，例如：

- `"dialog — confirm (李大娘：李逍遙！…)"`
- `"go to #54 (exit) path=up>up>right… — press up"`
- `"at event #68 (search) — confirm/space to interact"`
- `"select enemy target index=1 (蛇妖) — left/right, confirm"`

**可先读 `hint`，再读细节字段。**

#### 事件 `events[]`（找人、出口、调查）

| 字段 | 含义 |
| --- | --- |
| `id` | 对象编号 |
| `kind` | `search` / `touch` / `scenery` |
| `role` | 粗分类（同上） |
| `pos` / `delta` / `dist` | 位置与距离 |
| `can_search_now` | 是否已可调查 |
| `in_touch_range` | 是否已在触碰范围内 |
| `keys` | 走近该事件的推荐方向（过滤撞墙） |
| `dest_scene` | 可选；脚本切场景目标 |
| `trigger_script` 等 | 脚本/精灵编号（高级） |

`can_search_now` 或 `in_touch_range` 为 true 时用 `confirm` / `space`。

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
   - 先读 `hint` / `nav`  
   - `nav.can_act` → `confirm`/`space`  
   - 否则按 `nav.path[0]` 或 `nav.keys[0]` 移动（再 `step`）  
   - 无 `nav`：只在 `walk` 为 true 的方向探索  

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
