# Web 存档与用户系统

## 现状概览

浏览器版引擎本身**不写磁盘**：存档由 wasm worker `postMessage` 给主线程，主线程再决定持久化策略。

```
游戏菜单「保存」
  → Globals::save_game  (src/global.rs)
  → web::store_save     (src/web.rs)
       1. 更新 worker 内 PAL_FILES["{slot}.RPG"]
       2. postMessage({ palSave, data })
  → save-store.js
       · 始终写入 localStorage["pal-save-{1..5}"]
       · 若已登录 → PUT /api/saves/<slot>  (Bearer token)
```

读档：启动时 `seedInto` 把本地（+ 云端）槽位塞进 `files`，worker 当作 `PAL_FILES`。

| 位置 | 职责 |
| --- | --- |
| `src/global.rs` | DOS 存档格式 |
| `src/web.rs` `store_save` | 内存 + 通知主线程 |
| `web/save-store.js` | 本地缓存 + 登录态云同步 |
| `web/auth-ui.js` | 注册 / 登录 / 登出 UI |
| `web/serve.py` | 静态资源 + 鉴权 + 存档 API |

---

## 用户系统

自建 `web/serve.py` 时提供账号：

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `POST` | `/api/auth/register` | body `{username, password}` → `{token, username}` |
| `POST` | `/api/auth/login` | 同上 |
| `POST` | `/api/auth/logout` | `Authorization: Bearer <token>` |
| `GET` | `/api/auth/me` | 校验会话 → `{username}` |

规则：

- 用户名：`[A-Za-z0-9_]{3,32}`
- 密码：至少 6 字符；服务端 **PBKDF2-HMAC-SHA256**（200k 迭代）+ 随机 salt
- 会话：随机 token，默认 **30 天**；存在 `web/saves/_auth/sessions.json`
- 用户档案：`web/saves/_auth/users/<username>.json`（仅 hash，无明文密码）

### 存档 API（必须登录）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/saves` | 列出当前用户槽位 |
| `GET` | `/api/saves/<1-5>` | 下载 `.rpg` |
| `PUT` | `/api/saves/<1-5>` | 上传 `.rpg` |
| `DELETE` | `/api/saves/<1-5>` | 删除 |

请求头：`Authorization: Bearer <token>`（或 `X-Pal-Token`）。  
磁盘：`web/saves/<username>/<slot>.rpg`。

未登录访问存档接口返回 **401**。纯静态托管（GitHub Pages）没有这些路由，前端自动降级为仅 localStorage。

---

## 前端行为

1. **游客**：只写 localStorage；右上角显示「遊客 · 僅本地存檔」+ 登入/註冊。
2. **登录后**：双写 localStorage + 云端；状态显示「已登入 · 用户名」。
3. **启动同步**（已登录）：云端有档则覆盖本地缓存；仅本地有则上传迁移。
4. **中途登录**：`syncFromCloud` 更新 localStorage；**已启动的 worker 内 `PAL_FILES` 不会热更新**，若要立刻读云端旧档需刷新页面（新存档会正常上云）。
5. **登出**：清 token；之后只写本地，云端档保留在服务器。

Token 保存在 `localStorage["pal-auth-token"]`。

---

## 使用

```shell
./web/build.sh
python3 web/serve.py
# http://127.0.0.1:8080/web/
# 右上角注册/登录
```

手动测 API：

```shell
# 注册
curl -s -X POST http://127.0.0.1:8080/api/auth/register \
  -H 'Content-Type: application/json' \
  -d '{"username":"demo_user","password":"secret1"}'
# → {"token":"…","username":"demo_user"}

TOKEN=…   # 上一步返回的 token

curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/auth/me
curl -s -H "Authorization: Bearer $TOKEN" \
  --data-binary @recordings/autoplay-probe-checkpoint.rpg \
  -X PUT http://127.0.0.1:8080/api/saves/1
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/saves
```

---

## 生产注意

1. **GitHub Pages** 无后端 → 无账号，仅本地存档。
2. API 需与页面 **同域**（COOP/COEP）。
3. 当前为轻量自托管方案：无邮箱验证、无密码重置、无 CSRF（Bearer token）、无速率限制。公网请加反向代理限流、HTTPS，并考虑更强账号体系。
4. 多实例需共享 `web/saves/`（含 `_auth/`）。
5. 备份时同时备份 `_auth/` 与各用户目录。

### 可选后续

- 密码重置 / 邮箱
- 导入导出 `.rpg` 按钮
- 登录后热注入 worker `PAL_FILES`（免刷新）
- 用数据库替换文件存储
