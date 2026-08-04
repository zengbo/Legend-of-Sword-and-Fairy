# rustpal AI 操控说明（给 Agent 读）

本文档说明如何通过 **本地 HTTP API** 观察并控制仙剑 DOS 引擎的 Rust 移植版（rustpal）。  
你是 **外部 Agent**：只通过 HTTP 与游戏进程通信，不要假设有 GUI 鼠标或剪贴板。

**默认基址：** `http://127.0.0.1:8765`  
（仅 loopback；非本机地址会被拒绝。）

---

## 1. 你能做什么 / 不能做什么

| 可以 | 不可以 |
| --- | --- |
| 读最新画面 PNG（逻辑分辨率 320×200） | 直接改内存/存档格式（除非另有接口） |
| 读结构化状态 JSON（场景、坐标、对话中…） | 绑定非 loopback 地址 |
| 注入与真人相同的按键（点按 / 按住 / 松开） | 一次发送鼠标或触屏手势 |
| 在 **步进模式** 下推进虚拟时钟（单帧控制） | 在未开 step 时用 `/v1/step` 控制时间（会 409） |

游戏逻辑与真人游玩相同：对话、菜单、走路、战斗都走真实引擎路径。

---

## 2. 人类如何启动游戏（你依赖此环境）

人类应已启动 rustpal，并打开 UI driver。常见方式：

```bash
# A. 无头 + 严格单帧（推荐给 AI 训练/规划）
./target/release/rustpal --ui-driver --offscreen --ui-step --mute

# B. 终端里能看见画面 + HTTP（console 下默认实时时钟，不必 step 也能动）
./target/release/rustpal --console --ui-driver --ui-step

# C. 终端里看，且严格单帧（画面会冻到你 POST /v1/step）
RUSTPAL_UI_STEP_STRICT=1 ./target/release/rustpal --console --ui-driver --ui-step
```

| 启动参数 | 时钟 | 画面 | Agent 是否必须 step |
| --- | --- | --- | --- |
| `--ui-step --offscreen` | 虚拟 | 无窗口 | **必须**，否则游戏完全停住 |
| `--console --ui-step` | 实时（默认） | 终端有画 | 不必须；仍可用 input + state |
| 上式 + `RUSTPAL_UI_STEP_STRICT=1` | 虚拟 | 终端有画但会冻 | **必须** |
| 仅 `--ui-driver`（无 step） | 实时 | 视后端 | 不必须；按实时节奏操作 |

**探测：** `GET /v1/status` 中  
- `step_mode === true` → 引擎在等你 step  
- `step_configured === true` 且 `step_mode === false` → 开了 step 配置但 console 实时放行  

先 `GET /v1/status` 或 `GET /` 确认服务在线，再进入主循环。

---

## 3. HTTP API 一览

所有成功写操作多为 `202 Accepted`；读为 `200 OK`。  
`Content-Type`：JSON 用 `application/json`，帧为 `image/png`。

### 3.1 `GET /v1/status`

快速心跳，体量小。

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

- `frame_id`：每次成功 present 画面后递增；**变大**表示有新图可取。  
- `ticks`：引擎时间（ms）。`step_mode` 时为虚拟时钟。

### 3.2 `GET /v1/state`

**丰富结构化状态**（字段会增加，**忽略未知字段**）。名称类字符串已从游戏 Big5 转成 UTF-8。

#### 顶层常用字段

| 字段 | 含义与用法 |
| --- | --- |
| `phase` | `boot` / `dialog` / `battle` / `scene_transition` / `overworld` |
| `in_dialog` | `true` → 优先 `confirm` |
| `in_battle` | `true` → 用战斗键；见 `battle` 对象 |
| `in_main_game` | `false` → 片头/菜单；多 `confirm` 或大量 `step` |
| `entering_scene` | 切场景中 |
| `player` / `viewport` | 地图坐标 `[x,y]`（等距格子像素） |
| `walk` | `{up,right,down,left: bool}` **下一步是否可走**（引擎碰撞） |
| `scene` / `scene_info` | 场景号与 map/传送脚本/事件数量 |
| `party[]` | 队员：姓名、HP/MP、攻防、装备、法术列表 |
| `inventory[]` | 物品：id、中文名、数量、usable/equipable/throwable 等 |
| `events[]` | 当前场景附近/活跃事件（最多 48）：坐标、距离、脚本、是否可调查/触碰 |
| `battle` | 非战斗为 `null`；否则含敌人/UI 菜单状态 |
| `keys_hint` | 当前阶段建议键（仅提示） |
| `actions` | 合法键名列表 |
| `quit_requested` | 结束循环 |

#### `events[]` 条目（过场/找 NPC 用）

| 字段 | 含义 |
| --- | --- |
| `id` | 事件对象 id（脚本用） |
| `kind` | `search` / `touch` / `scenery` |
| `pos` / `delta` / `dist` | 绝对坐标、相对玩家、曼哈顿距离（y×2） |
| `trigger_mode` / `trigger_script` / `auto_script` | 触发方式与脚本入口 |
| `can_search_now` | 是否已在调查范围内 |
| `in_touch_range` | 是否已在触碰半径内 |

#### `walk` 用法

```json
"walk": {"up": true, "right": false, "down": true, "left": true}
```

只对 `true` 的方向 `press`/`tap`，避免怼墙。

#### `battle` 对象（战斗中）

| 字段 | 含义 |
| --- | --- |
| `phase` | `select_action` / `perform_action` |
| `ui_state` | `wait` / `select_move` / `select_target_enemy` … |
| `menu_state` | `main` / `magic_select` / `use_item_select` … |
| `enemies[]` | `name`, `hp`, `level`, `state`, `time_meter` |
| `players[]` | 战斗槽状态、`defending` |
| `force` / `flee` / `auto_attack` | 战斗标志 |

#### 精简示例

```json
{
  "phase": "overworld",
  "scene": 1,
  "player": [2360, 1392],
  "walk": {"up": true, "right": true, "down": false, "left": true},
  "in_dialog": false,
  "in_battle": false,
  "party": [{"slot": 0, "name": "李逍遙", "hp": 100, "max_hp": 100, "magics": []}],
  "inventory": [{"item": 12, "name": "黃連", "amount": 3, "usable": true}],
  "events": [
    {"id": 5, "kind": "search", "dist": 32, "can_search_now": true, "trigger_script": 1200}
  ],
  "battle": null,
  "keys_hint": ["up", "down", "left", "right", "confirm", "space", "menu"]
}
```

状态在 `process_event` / `video_update` 时刷新；启动瞬间可能仍是占位 JSON。

### 3.3 `GET /v1/frame.png`

- 逻辑画面 **320×200** RGBA PNG（非 720p 超分）。  
- 尚无帧时可能 `503`：稍后重试或先 `step`。  
- 像素风、偏暗 UI：视觉模型建议放大后再理解；可与 `/v1/state` 交叉验证。

### 3.4 `POST /v1/input/{key}/{action}`

`action`：`tap` | `press` | `release`

| action | 行为 |
| --- | --- |
| `tap` | 按下 → 约 75ms → 松开（最常用） |
| `press` | 按住不放（走路） |
| `release` | 松开 |

**合法 `key` 名（小写，别名见下）：**

| key | 游戏作用 | 别名 |
| --- | --- | --- |
| `up` `down` `left` `right` | 方向 | — |
| `confirm` | 确认 / 调查 / 对话 | `enter`, `search` |
| `space` | 搜索/确认类 | — |
| `menu` | 菜单 / 取消 | `escape`, `esc` |
| `force` | 战斗「法术/强力」等 | `magic`, `f` |
| `auto` | 自动战斗相关 | `a` |
| `defend` | 防御 | `d` |
| `use_item` | 使用物品 | `e` |
| `throw_item` | 投掷 | `w` |
| `flee` | 逃跑 | `q` |
| `status` | 状态 | `s` |
| `repeat` | 重复 | `r` |
| `page_up` `page_down` `home` `end` | 翻页等 | `pgup` `pgdn` |

示例：

```http
POST /v1/input/confirm/tap HTTP/1.1
Host: 127.0.0.1:8765
Content-Length: 0
```

```http
POST /v1/input/down/press
POST /v1/input/down/release
```

**注意：**

- 走路：先 `press` 方向，保持一段时间（实时模式 `sleep`；步进模式多次 `step`），再 `release`。  
- 对话：反复 `confirm` tap，不要用方向键乱选（除非你确认在菜单上）。  
- 主菜单：方向移动光标 + `confirm` 确认。  
- 输入经队列注入，与真键盘相同；在 `step_mode` 下应 **先 input 再 step**，以便本帧读到按键。

### 3.5 `POST /v1/step`（仅步进配置开启时）

推进虚拟时钟。未配置 step 时返回 **409**。

| 请求 | 效果 |
| --- | --- |
| `POST /v1/step` | +100ms（1 个 overworld 帧，`FRAME_TIME`） |
| `POST /v1/step?frames=N` | +N×100ms |
| `POST /v1/step?ms=N` | +N ms |
| body `{"frames":5}` 或 `{"ms":500}` | 同上 |

响应示例：

```json
{"accepted":true,"advanced_ms":100,"ticks":500,"frame_time_ms":100,"gating":true}
```

- `gating: true`：引擎时钟被 step 卡住，必须 step 才会动。  
- `gating: false`：console 实时模式，step 仍会加虚拟计数，但 **不卡住** 游戏。

**开场建议：** 若 `step_mode` 且 `in_main_game == false`，可先：

```http
POST /v1/step?frames=300
```

再轮询 `/v1/state` 直到 `in_main_game` 为 true（或配合 `confirm`）。

---

## 4. 推荐控制循环

### 4.1 严格单帧（`--offscreen --ui-step`）

```
1. GET /v1/status  → 确认在线；记下 step_mode
2. 若 step_mode 且未进主游戏：
     可选 POST /v1/step?frames=50~300 跳过片头
3. loop:
     a. GET /v1/state
     b. 若 quit_requested: 结束
     c. 可选 GET /v1/frame.png（视觉模型）
     d. 决策动作
     e. POST /v1/input/...   （先输入）
     f. POST /v1/step?frames=1   （再步进）
     g. 若需长按移动：press → 多次 step → release
```

### 4.2 实时 / console 观看（`step_mode == false`）

```
loop:
  GET /v1/state 和/或 frame.png
  POST input
  sleep 50~200ms（墙钟）  # 不要死循环空转 CPU
```

不必调用 `/v1/step`（调用也无门控作用）。

### 4.3 策略启发式（省 token）

1. **`phase` / `in_dialog`** → `confirm`  
2. **`in_battle`** → 读 `battle.ui_state` / `enemies[]`；`force`/`auto`/`confirm`  
3. **找人/出口** → 在 `events[]` 里按 `dist` 选目标；`can_search_now` 则 `confirm`/`space`；否则朝 `delta` 在 `walk` 允许的方向移动  
4. **走路** → 仅 `walk.* == true` 的方向；长按用 `press` + 多帧 `step` + `release`  
5. **道具** → `inventory[]` 里 `usable`/`name`；战斗外菜单路径仍需你自己按键导航  
6. **卡住** → `player`/`frame_num` 不变则换方向，或交互最近 `events`  

不要每帧把整张 PNG 塞进上下文；**优先完整 `/v1/state`**，画面按需。

---

## 5. curl 速查

```bash
BASE=http://127.0.0.1:8765

curl -s $BASE/v1/status
curl -s $BASE/v1/state | jq .
curl -s $BASE/v1/frame.png -o /tmp/pal.png

curl -s -X POST $BASE/v1/input/confirm/tap
curl -s -X POST $BASE/v1/input/down/press
# ... 等待或 step ...
curl -s -X POST $BASE/v1/input/down/release

curl -s -X POST "$BASE/v1/step?frames=1"
curl -s -X POST "$BASE/v1/step?ms=500"
curl -s -X POST $BASE/v1/step -H 'Content-Type: application/json' -d '{"frames":10}'
```

---

## 6. Python 最小 Agent 骨架

```python
import json, time, urllib.request

BASE = "http://127.0.0.1:8765"

def get_json(path):
    with urllib.request.urlopen(BASE + path) as r:
        return json.load(r)

def post(path, data=None):
    req = urllib.request.Request(
        BASE + path,
        data=(data if data is not None else b""),
        method="POST",
        headers={"Content-Type": "application/json"} if data else {},
    )
    with urllib.request.urlopen(req) as r:
        return r.read()

def tap(key):
    post(f"/v1/input/{key}/tap")

def step(frames=1):
    post(f"/v1/step?frames={frames}")

def frame_png():
    with urllib.request.urlopen(BASE + "/v1/frame.png") as r:
        return r.read()

# 启动探测
st = get_json("/v1/status")
assert st.get("status") == "ok"

if st.get("step_mode"):
    step(200)  # 尝试越过片头

while True:
    state = get_json("/v1/state")
    if state.get("quit_requested"):
        break
    if state.get("in_dialog"):
        tap("confirm")
    elif state.get("in_battle"):
        tap("force")
    else:
        # TODO: 用地图策略或 VLM(frame_png()) 决策
        tap("confirm")
    if state.get("step_mode"):
        step(1)
    else:
        time.sleep(0.1)
```

将 `# TODO` 换成你的模型调用即可。

---

## 7. 错误与边界

| 情况 | 处理 |
| --- | --- |
| 连接失败 | 游戏未启动或端口不对 |
| `POST /v1/step` → 409 | 未开 `--ui-step` |
| `GET /v1/frame.png` → 503 | 尚无 present；先 step 或等待 |
| 画面一直空 + step_mode | 你必须 step；或改用 console 非 STRICT |
| 按键无效果 | 是否在菜单/脚本等待；是否先 input 后 step；是否误用未知 key 名 |
| `frame_id` 不变 | 无新 present（卡在 delay 且无人 step） |

---

## 8. 与内置自动脚本的区别（勿混淆）

| 程序 | 用途 |
| --- | --- |
| **主程序 + 本文 API** | 给你（AI）控 |
| `examples/autoplay` | 内置随机探索 Pilot + 录像 |
| `examples/fullgame_autoplay` | 规则寻路通关探针（非 HTTP Agent） |

不要同时让 fullgame Pilot 与 HTTP Agent 抢键，除非明确只要其中一个。

---

## 9. 系统提示词摘要（可直接贴进 Agent）

```
你在控制 rustpal（仙剑 DOS 引擎）。基址 http://127.0.0.1:8765。
观察：GET /v1/state（优先，含 walk/events/inventory/party/battle）、GET /v1/frame.png、GET /v1/status。
动作：POST /v1/input/{key}/tap|press|release；
键：up down left right confirm space menu force auto defend use_item throw_item flee status。
step_mode=true 时每次决策后 POST /v1/step?frames=1（开场可 frames=200）。
策略：in_dialog→confirm；in_battle→读 battle 对象再用 force/auto；overworld→用 walk 与 events[].delta/dist 寻路与交互；只用 walk 为 true 的方向。
先 input 再 step。忽略未知 JSON 字段。无鼠标。
```

---

## 10. 相关文件

| 文件 | 内容 |
| --- | --- |
| `docs/ai-control.md` | 本文（给 AI） |
| `docs/autoplay.md` | HTTP / step 设计说明 |
| `docs/console.md` | 终端画面后端 |
| `src/ui_driver.rs` | API 实现 |

若 API 与本文冲突，以运行中的 `GET /` 帮助文本与源码为准。
