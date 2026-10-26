# farwork-hub

上游 `anywhere-labs/Agents-Anywhere` 的 `server/agent_server`（Python/FastAPI）在 Farwork 里的
**纯 Rust + Postgres** 替代实现。当前交付 P0 子集：打通一条真实会话链路
（登录 → 设备 → 项目 → 会话 → timeline → 发消息 → WS 增量）。

**边界**：本 crate 只写 `crates/farwork-hub/**`（含 `migrations/`、`scripts/`、`tests/`）。
timeline 的域语义来自 `crates/farwork-timeline`（另一个 agent 拥有），本 crate 只做
持久化、序列化、鉴权与广播。

---

## 1. 快速开始

```bash
# 1) 建库（本机 PG 18，用户 tianjun 为 superuser）
createdb -h 127.0.0.1 -U tianjun farwork

# 2) 启动（默认端口 7300；--port 覆盖；端口被占用时非 0 退出）
cargo run -p farwork-hub
cargo run -p farwork-hub -- --port 7311
```

环境变量：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `DATABASE_URL` | `postgres://tianjun@127.0.0.1:5432/farwork` | 启动时用 `sqlx::migrate!` 应用 `migrations/` |
| `FARWORK_HUB_PORT` / `PORT` | `7300` | `--port` 优先级最高 |
| `FARWORK_HUB_SECRET` | `agent-server-dev-secret` | HS256 签名密钥；未设置会 warn |
| `FARWORK_HUB_SETUP_TOKEN` | 随机生成并打印到日志 | 首个 admin 注册所需的引导令牌（TTL 30 分钟） |
| `FARWORK_HUB_DB_POOL` | `10` | 连接池上限 |
| `TEST_DATABASE_URL` | 未设置 | 集成测试用库；**未设置时测试会明确 SKIP 并打印提示** |

端口占用实测：

```
$ FARWORK_HUB_SETUP_TOKEN=x ./target/debug/farwork-hub --port 7311
farwork-hub: cannot bind port 7311: Address already in use (os error 48)
exit=1
```

---

## 2. 端到端复现

```bash
# 终端 A
createdb -h 127.0.0.1 -U tianjun farwork            # 可选：rm 旧库
FARWORK_HUB_SETUP_TOKEN=setup-e2e-token \
FARWORK_HUB_SECRET=e2e-secret \
  cargo run -p farwork-hub -- --port 7311

# 终端 B
BASE_URL=http://127.0.0.1:7311 SETUP_TOKEN=setup-e2e-token \
  bash crates/farwork-hub/scripts/e2e.sh
```

脚本只用 `curl` / `jq` / `node`（node 仅用于按浏览器算法算 PBKDF2 与跑 WS 客户端），
**全程没有任何 Python**。

实测输出（2026-10，PG 18.6，本机）：

```text
=== healthz ===
{"status":"ok","version":"0.1.0"}

=== auth/config (bootstrap) ===
{"needsBootstrap":true,"registrationOpen":false,"setupTokenExpiresAt":"2026-10-26T03:23:17.000000Z"}

=== auth/password-salt ===
salt=2FLwjwea-F0TjejbHtbpRw

=== derive verifier (PBKDF2-SHA256 / 120000 iterations / 32 bytes / base64url) ===
verifier=uTjSg08wVbt72bn2JI6f-OM9yilHVcC-CbLMKkRWI6A

=== auth/register (first account becomes admin) ===
{"userId":"usr_e09088a52816655363c69c47","role":"admin","email":"ada+1792983197@example.com","tokenType":"bearer"}
token=eyJhbGciOiJIUzI1NiIsInR5...

=== auth/login ===
{"userId":"usr_e09088a52816655363c69c47","role":"admin","tokenType":"bearer"}

=== auth/me ===
{"userId":"usr_e09088a52816655363c69c47","role":"admin","disabled":false}

=== connectors: create ===
{"connector":{"id":"conn_Zpr_a9bRL4MAQA","status":"offline"},"tokenPrefix":"cxt_M0XEbzvn"}

=== projects: create ===
{"project":{"id":"proj_nSjHDlN2RmE5fg","workspacePath":"/srv/farwork","activeSessionCount":0}}

=== sessions/create-and-start ===
{"session":{"id":"sess_W_sRN6Dp0zM_Gw","status":"idle","takeover":true,"updatedSeq":1},"connectorResult":"connector_offline","eventCursor":"seq:1"}

=== sessions: list (limit<=100) ===
{"count":1,"hasMore":false,"first":"sess_W_sRN6Dp0zM_Gw"}

=== runtime/messages (persist a user message as a timeline item) ===
{"ok":true,"eventCursor":"seq:2","item":{"id":"itm_s3b9-8kEpju-4XEu","role":"user","updatedSeq":2,"text":"second message"}}

=== runtime/messages replay with the same clientMessageId (idempotent) ===
{"ok":true,"duplicate":true,"sameItem":"itm_s3b9-8kEpju-4XEu"}

=== snapshot ===
{"items":2,"nextSeq":2,"eventCursor":"seq:2","capabilities":0}

=== events?after=seq:0 (cursor catch-up, not polling) ===
{"events":[{"type":"timeline.item_created","sequence":1,"cursor":"seq:1"},{"type":"timeline.item_created","sequence":2,"cursor":"seq:2"}],"nextCursor":"seq:2","snapshotRequired":false}

=== runtime/state + runtime/capabilities ===
{"status":"idle","updatedSeq":2}
{"connectorId":"conn_Zpr_a9bRL4MAQA","revision":0,"capabilitySet":{"steer":false,"interrupt":false,"selections":false,"commands":false}}

=== not implemented: session sync answers 501 (steer is implemented now) ===
{"error":"not_implemented","detail":"POST /api/v2/sessions/sess_W_sRN6Dp0zM_Gw/sync is not implemented in this build"}

=== ws-ticket + session socket (session.subscribed, then a live delta) ===
{"type":"session.subscribed","cursor":"seq:2","payloadKeys":["clientId","eventCursor"]}
post ok=true
{"type":"timeline.item_created","cursor":"seq:3","payloadKeys":["item"]}
```

Token 是真实 HS256 JWT（`header.payload.signature`，payload `{"exp","sub"}`，7 天），
`userId=usr_...`、`seq:1..3` 都是该次运行的真实值；密码明文不出现（只用 verifier）。

### 2.1 重启后从 PG 回放（实测，非声明）

杀进程重启、同一个库，重新登录后：

```text
$ kill <hub>; ./target/debug/farwork-hub --port 7311 &
salt-after-restart=1SmBBIjVVXiaJ9rX3oQdfg      # 盐来自库里存的 password_hash
session=sess_7pxnwJBLi9YVNg
{"items":[{"id":"itm_6bv7UY7nCQ6LEdJF","updatedSeq":1,"text":"hello from the e2e script","cmid":"cm_e2e_1"},
          {"id":"itm_DzpFfEylNALUu-tT","updatedSeq":2,"text":"second message","cmid":"cm_e2e_2"},
          {"id":"itm_CegpnmNcvEyCUYJi","updatedSeq":3,"text":"delta from the ws test","cmid":null}],
 "nextSeq":3,"eventCursor":"seq:3"}
{"types":["timeline.item_created",...],"seqs":[1,2,3],"nextCursor":"seq:3","snapshotRequired":false}
{"ok":true,"eventCursor":"seq:4","updatedSeq":4}    # 续写继续用新 sequence
```

item id、`updated_seq` 与 `sessions.seq` 跨重启保持一致，序列从 4 继续 —— 回放是精确的。

---

## 3. 集成测试（可重复的强证据）

```bash
createdb -h 127.0.0.1 -U tianjun farwork_test
TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test \
  cargo test -p farwork-hub --test e2e -- --nocapture
```

覆盖：bootstrap 令牌校验 → admin 注册 → 重复注册 403 → 错误 verifier/未知邮箱 401 →
/me 鉴权失败 401 → connector/project → `POST /sessions` 缺 externalSessionId 422 →
create-and-start → 消息写入 + `clientMessageId` 幂等 + `source.clientMessageId` 回显 →
snapshot 信封 10 个必填字段 → events 游标补拉 / 超前 / 非法三种分支 →
501 → ws-ticket 一次性 → WS `session.subscribed` + 实时增量 → dashboard.snapshot。

管理台另有一个文件（`tests/admin.rs`，5 个用例，每个用例自己 truncate + bootstrap 一个
admin 与一个 member，并用进程内异步锁互斥）：非 admin 403 / 未登录 401 / 建号后能用新密码登录 /
重复邮箱 409 / 自己停用与降级 409 / 降级最后一个 admin 409 / 列表里 grep 不到任何哈希或盐 /
PATCH settings 生效且 `/auth/config` 跟着变。

未设置 `TEST_DATABASE_URL` 时：

```text
SKIP farwork-hub integration test: TEST_DATABASE_URL is not set.
Point it at a disposable database to run this test, e.g.
  export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test
test p0_journey_login_to_websocket_delta ... ok
```

---

## 4. 已实现端点（HTTP/WS 契约对齐上游 `server/agent_server/api/*`）

**39 个路径 / 47 个 method+path**（含 hub 自有的 `/healthz` 与 `/install.sh`；其余都在 `/api/v2` 下）。

| 方法 | 路径 | 上游依据 |
| --- | --- | --- |
| GET | `/healthz` | ——（本 crate 自有） |
| GET | `/install.sh` | ——（本 crate 自有）：**免鉴权**，回仓库根 `install.sh` 的原字节（`include_str!` 编进二进制），配对页那句 `curl -fsSL <hubUrl>/install.sh \| sh` 靠它才成立 |
| GET | `/api/v2/auth/config` | `api/auth.py:81` `AuthConfigResponse` |
| POST | `/api/v2/auth/password-salt` | `api/auth.py:114` `{salt, serverTime}` |
| POST | `/api/v2/auth/register` | `api/auth.py:126`（首启需 setupToken，之后看 registrationOpen） |
| POST | `/api/v2/auth/login` | `api/auth.py:183` `AuthResponse` |
| GET | `/api/v2/auth/me` | `api/auth.py:385` `AuthMeResponse` |
| POST | `/api/v2/ws-ticket` | `api/client_ws.py:21` `{ticket, expiresAt, serverTime}`（一次性、60s） |
| GET/POST | `/api/v2/connectors` | `api/connectors.py` `{connectors, serverTime}` / `{connector, connectorToken, tokenPrefix}` |
| GET/PATCH/DELETE | `/api/v2/connectors/{id}` | 同上（DELETE 204） |
| POST | `/api/v2/connectors/{id}/revoke` | 轮换 `cxt_` 设备令牌 |
| GET | `/api/v2/connectors/{id}/preferences` | 无 daemon 上报时返回 `{}` |
| POST | `/api/v2/connectors/{id}/sessions/archive-all` | `{sessions, affected, serverTime}` |
| GET/POST | `/api/v2/projects` | `api/projects.py` `{projects, serverTime}` / `{project, attachedSessions, serverTime}` |
| GET/PATCH/DELETE | `/api/v2/projects/{id}` | 命名/工作区冲突 → 409 `{code: project_name_conflict}` |
| GET | `/api/v2/projects/{id}/sessions` | 分页 `limit ≤ 100` |
| GET | `/api/v2/sessions` | `{sessions, hasMore, nextCursor, serverTime}`（limit ≤ 100） |
| GET | `/api/v2/sessions/list` | 任务台清单 `{sessions, serverTime}`（limit ≤ 100） |
| POST | `/api/v2/sessions` | 导入既有 runtime 会话，**必须带 `externalSessionId`**，否则 422 |
| POST | `/api/v2/sessions/create-and-start` | 建会话 + 落首条用户消息 |
| GET/PATCH | `/api/v2/sessions/{id}/meta` | `{session, serverTime}` |
| GET | `/api/v2/sessions/{id}/timeline` | `mode=latest\|changes\|history`、`afterSeq`、`beforeOrderSeq`、`limit ≤ 500` |
| GET | `/api/v2/sessions/{id}/snapshot` | `ProtocolSessionSnapshotResponse`（10 个必填字段齐全） |
| GET | `/api/v2/sessions/{id}/events?after=seq:N` | `{events, nextCursor, snapshotRequired, serverTime}` |
| GET | `/api/v2/sessions/{id}/runtime/state` | `SessionRuntimeStateResponse` |
| GET | `/api/v2/sessions/{id}/runtime/capabilities` | `ProtocolCapabilitiesResponse`（空能力集，见 §7） |
| POST | `/api/v2/sessions/{id}/runtime/messages` | `{ok, result:{item, eventCursor, clientMessageId, duplicate}}` |
| POST | `/api/v2/sessions/{id}/shares` | `201 {share:{id, scope, urlPath, createdAt}, serverTime}`；`scope=session\|message`（`message` 需 `messageId`）。快照在会话写锁下取，**建完即不可变** |
| DELETE | `/api/v2/sessions/{id}/shares/{shareId}` | `204`（无 body）；owner 才能删，软删除后公开读 404 |
| GET | `/api/v2/public/shares/{shareId}` | **免鉴权**（不带 `Authorization` 也 200）：`{share:{id, scope, sessionTitle, createdAt, timeline:{items}}, serverTime}`，只回快照 |
| WS | `/api/v2/sessions/{id}/ws?ticket=` | `session.subscribed` + `timeline.item_created/updated` + `session.meta.updated` + `session.refetch_required` + `keepalive` |
| WS | `/api/v2/dashboard/ws?ticket=` | `dashboard.snapshot` + 变更后重发快照 |
| GET | `/api/v2/admin/users` | 管理台用户目录 `{users, nextCursor, serverTime}`；**仅 role=admin**；`limit` 默认 50、上限 100，游标是 `(createdAt, id)` 的 keyset |
| POST | `/api/v2/admin/users` | `201 {user, serverTime}`；body `{email, password, role:"admin"\|"member"}`，密码由服务端派生 verifier 落库（与注册同一条 PBKDF2 路径）；重复邮箱 409 |
| PATCH | `/api/v2/admin/users/{id}` | `200 {user, serverTime}`；body `{role?, disabled?, displayName?}`；会把自己或最后一个 admin 锁在门外时 409 `{code:"admin_lockout", reason, message}` |
| GET/PATCH | `/api/v2/admin/settings` | `{settings:{registrationOpen, emailVerificationRequired}, serverTime}`；写 `app_settings` 的 `registration_open` / `email_verification_required`，`GET /auth/config` 读同一行 |
| POST | `/api/v2/pairing/start` | `201 {pairing:{id, code, expiresAt}, serverTime}`；body `{ttlSeconds?}`（默认 600，60..=3600，越界 422）。`code` 形如 `4F2K-9QXM`，**只出现这一次**（库里只存 HMAC 摘要） |
| POST | `/api/v2/pairing/claim` | **免鉴权**（code 就是凭据）：body `{code, name, platform?}` → `200 {connector:{id,name,deviceOs,status}, connectorToken, serverTime}`；在配对所属用户名下建 connector 并标记 claimed（一次性）。错误 `404 invalid_code` / `409 code_already_claimed` / `410 code_expired` / `429 too_many_attempts`（同源 IP 滑动窗口 10 次/分钟） |
| GET | `/api/v2/pairing/{id}` | 供 Web 轮询：`{pairing:{id, status:"pending"\|"claimed"\|"expired"\|"unknown", connectorId?, expiresAt}, serverTime}`；别人的 id 与不存在的 id 都回 `unknown`（不泄露存在性） |

### 501 清单（`{"error":"not_implemented","detail":...,"serverTime":...}`）· **35 条**

`POST|DELETE /sessions/{id}/takeover` / `POST /sessions/{id}/sync` / 账号级分享列表
（`/shares/**`；会话/消息分享的三条端点已实现，见上表）/ 批量 archive|unarchive|read / device runtime 管理
（`/connectors/{id}/runtimes/**`、`/runtimes/**`、`/devices/**`）/ OAuth（`/auth/oauth/*`）/
扫码登录（`/auth/mobile-login/*`）/ 邮箱验证码、改密、换头像 / announcements、agents、
usage。**配对码已实现**（上表三条，`/pairing*` 已从清单移出）；`/connectors/{id}/pairing` 仍在表里
——那是每台设备的运行时开关，与配对码不是同一件事。**会话命名空间整体已实现**：`/sessions/{id}/files/list`、`/files/readText`、
`/sessions/{id}/terminal` 与 `/terminal/{terminalId}/stream`、`runtime/capabilities`、
`runtime/catalogs/{catalog}`、`runtime/commands`、`runtime/steer`、`runtime/selections` 都不在这份清单里。
管理台（`/admin/**`）已实现，见上表四条，不再在这份清单里。未知路径返回 404
`{"detail":"no route for ..."}`（包括没有对应路由的 `/admin/xxx`）。

---

### 管理台：权限门与「不把自己锁死」

四条 `/admin/*` 端点全部取 `AuthedUser`（所以**没带 token 是 401**，来自 extractor），再由
`routes::admin::require_admin` 重读那一行 users：账号必须仍在、未被停用、且 `users.role = 'admin'`，
否则 **403** `{"detail":{"code":"forbidden","message":...}}`。判定只用 `users.role` 这一个字段，
**没有** `is_admin` 之类的布尔副本，所以不存在「半个 admin」。

「不锁死自己」的判定只有一个地方：`crates/farwork-hub/src/routes/admin.rs` 的
`lockout_reason(...)`（纯函数，单测覆盖）。命中任一条即 **409**
`{"detail":{"code":"admin_lockout","reason":...,"message":...}}`：

| reason | 规则 |
| --- | --- |
| `self_disable` | admin 不能把自己 `disabled = true` |
| `last_admin` | 谁都不能让**最后一个能登录的 admin** 被降级或停用 |
| `self_demote` | admin 不能把自己降成 `member`（只剩自己时先命中 `last_admin`，那是更强的说法） |

活跃 admin 的计数在同一事务里用 `select id from users where role = 'admin' and disabled = false
for update` 取，行锁让两个并发的降级不可能都读到 2 然后都成功 —— 这条不变量由 PG 兜底，不是进程内
的 check-then-write。

---

### 分享：免鉴权读端点的边界

`GET /api/v2/public/shares/{shareId}` 挂在 `routes::shares::public_router()`，**不取
`AuthedUser`**（其余 `/api/v2` handler 全部取），所以「带不带 token」都不影响它：有票的人和无票的
游客看到的是同一份快照。它只读 `shares.snapshot_json` 这一列，不回查 `sessions`/`connectors`/
`projects`/`timeline_items`，因此不可能顺带带出 `connectorId`、`workspacePath`、`cwd`、
`sessionId`、`source`、token 或邮箱。条目字段是**白名单**（`id/type/status/role/content/orderSeq/
itemTime`）：以后给 HTTP timeline 加字段，不会自动出现在公开页上。

---

## 5. 数据模型

`migrations/0001_init.sql`：`users`、`app_settings`、`connectors`、`projects`、
`sessions`、`session_runtime_states`、`timeline_items`、`ws_tickets`。列名对齐上游
`infra/db/schema.py`（HTTP/WS 契约才是硬约束，表结构允许不同）：

* `users.password_hash` = `pbkdf2_sha256$<salt>$<verifier>`，服务端从不存明文；
* `connectors.token_prefix` = 令牌前 12 字符，`token_hash` = SHA-256 hex；
* `timeline_items.payload_json` = `{"item": <farwork-timeline item>, "source": <上游 source>}`，
  同时把 `type/status/role/order_seq/updated_seq/revision/content_hash` 摊成列，
  `client_message_id` 上有 `(session_id, client_message_id)` 唯一索引做幂等；
* `sessions.seq` 镜像 timeline 头部 sequence。

`migrations/0004_shares.sql`：`shares`（`id / user_id / session_id / scope / message_id /
snapshot_json / allowed_file_ids_json / created_at / deleted_at`）。`snapshot_json` 是
`{"version":1, "sessionTitle", "eventCursor", "items":[{id,type,status,role,content,orderSeq,itemTime}]}`
的**一次性副本**：公开读只读这一列，不回查 `sessions`/`timeline_items`，所以原会话之后怎么变都不会
改动已发布的分享；`deleted_at` 是软删，撤销后的 id 与从未存在过一样回 404。

---

## 6. 与 `farwork-timeline` 的分工

* **域语义（该 crate 负责，本 crate 不重写）**：sequence 分配与稠密性、`contentHash` 判等去重、
  `orderSeq` 分配、`seq:N` 游标与 `Recovery`（不能给部分增量时必须 `RefetchRequired`）、
  `EventEnvelope`/`EventType`、`SessionFence`。
* **本 crate 负责**：把引擎投影落 PG（`timeline_items` + `sessions.seq`）、跨进程重启时按
  `updated_seq` 顺序**回放**重建引擎、wire 适配（上游比该 crate 多 `source`、`completedAt`、
  `protocolVersion`，且 `contentHash` 带 `sha256:` 前缀 —— 都在 `src/timeline.rs` 的
  `wire_item`/`wire_event` 里补，域侧哈希保持裸 hex）、鉴权、`broadcast` 扇出。
* **持久化状态**：PG 是唯一真相；引擎是**按会话懒加载的进程内缓存**。每次提交在
  `persist_item` 中与 `sessions.seq` 同事务写入，因此重启不丢数据。
  重启回放是**精确的**，因为本 build 的每次提交都是一次 item 提交（`updated_seq` 稠密）。
  若某行 `revision > 1`（历史上有更新，而回放只能重建当前状态于 revision 1），
  回放后立刻 `request_refetch(Requested)`，让客户端整块重拉而不是打补丁 —— 不假装无损。
* `POST /sessions/{id}/runtime/messages` 的 `clientMessageId` 幂等是**存储层**判断
  （唯一索引 + 先查后写），域层只负责"同状态不产生事件"。

---

## 7. 与上游的刻意简化（及与 Web 契约的差异）

已核对 Web 侧 6 条契约，全部满足：响应都是对象包数组；登录真校验（未知邮箱/错密码 401，
盐按用户区分，PBKDF2-SHA256/120000/32B/base64url）；snapshot 信封 10 个必填字段齐全且
`eventCursor` 是顶层 `"seq:N"`；事件信封 8 个必填字段；`events` 恢复在游标超前/非法/
历史被重置时真的返回 `snapshotRequired:true`（并由域 crate 的 `CursorAhead`/`TimelineReset`
决定）；`runtime/messages` 落库项回显 `source.clientMessageId`；WS 都在 `/api/v2/` 前缀下。

刻意简化（与上游不同的地方）：

1. **没有 connector ingress**（`/ws/connector` 不存在），所以：
   * `connectors.status` 恒为 `offline`（真实值，不是占位）；
   * `runtime/capabilities` 的 `capabilitySet` 现在来自 agent 的 `session.capabilities`
     帧（v1.3），没收到过就是全 `false`；`snapshot` 的 `state.status` 来自会话行；
   * `create-and-start` 不向 runtime 派发，响应显式给出
     `connectorResult:{ok:false, code:"connector_offline"}` 与 `runtimeDispatched:false`，
     但平台会话与首条消息是真落库的。
2. **权限门**：`runtime/messages` 要求 `takeover=true`（上游一致，否则 409
   `session is read-only until takeover is enabled`）。导入的会话（`POST /sessions`）是只读的，
   `takeover` 端点未实现（501），因此可写路径只有 `create-and-start`。
3. **meta 变更不消耗 timeline sequence**：上游 `_bump_session` 会占一个 revision；这里
   `PATCH /sessions/{id}/meta` 只更新行并广播 `session.meta.updated`（sequence = 当前头部）。
   代价：meta 变更**不进入** `events?after=` 的回放，重连后靠 snapshot 拿最新 meta。
4. `GET /sessions/list` 上游是"全部会话清单"，这里加了 `limit ≤ 100` 上限防止单账号拉爆。
5. `connections.token_prefix`、`installationId` 幂等注册、`/connectors/{id}/revoke` 已实现；
   但 `connector.user_preferences` 未实现（永远 `{}`）。
6. 审批/终端/文件/OAuth/扫码、账号级分享列表（`/shares/**`）全部 501，不返回假成功；
   会话/消息分享已实现，但**只有**快照里白名单字段会出现在公开响应里（见 §5 的 `shares`
   说明，白名单实现是 `src/routes/shares.rs` 的 `shared_item`）。
7. `snapshot` 的 `catalogs` 为空、`notices`/`approvals` 为空数组（runtime 侧数据本 build 不存在）。
8. 事件里的 `session.refetch_required` 由 hub 在 WS 订阅者 lag 时发出；HTTP `events` 的
   refetch 分支不额外附带 reason 字段（保持 `ProtocolEventRecoveryResponse` 的字段集合不变，
   reason 只进 tracing 日志）。
9. **管理台的 `emailVerificationRequired` 只落库、只上报，不拦截注册**：本 build 没有任何邮件
   投递通道，也没有验证码端点（`/auth/email-code` 仍是 501），所以把它打开不会让注册要求验证
   邮箱 —— 那会是假成功。`registrationOpen` 是真的行为开关（关掉后 `POST /auth/register` 回
   403），两者都写 `app_settings`，`GET /auth/config` 与 `GET /admin/settings` 读同一行。

---

## 8. 质量门槛

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

本 crate 单测覆盖：PBKDF2 verifier/JWT 往返与过期、游标解析、contentHash 稳定性、事件信封、
wire 适配（`source`/`completedAt`/`contentHash` 前缀/`protocolVersion`）、会话分页游标、
邮箱与显示名规范化、工作区 key 规范化，以及管理台的 `lockout_reason`（自锁三条规则 +
「另一个 admin 可降级」「只剩一个 admin 时不可降级」两条例外）与 role 校验。
