# Farwork 架构与线协议

本文是**当前实现（M3：connector ingress + agent）**的架构与 wire 契约。`farwork-hub`、`apps/web`、`farwork-agent` 三侧必须严格一致；M1 的终端协议保留为附录 A（遗留）。

改协议的唯一顺序：域类型（`crates/farwork-timeline` / `crates/farwork-protocol`）→ 本文 → hub 与 web 两侧实现。

## 1. 当前架构（M3：connector ingress + agent）

```
 Browser (apps/web :5173, 静态 SPA)
        |   HTTP  /api/v2/*           (REST, Bearer token)
        |   WS    /api/v2/dashboard/ws
        |   WS    /api/v2/sessions/{id}/ws?ticket=...
        v
 farwork-hub (:7300, axum)  --->  PostgreSQL (唯一状态来源)
        |                          migrations/0001_init.sql … 0004_shares.sql，启动时自动应用
        ^
        |   WS  /api/v2/connector/ws?token=...    (Ingress Protocol v1，见 §5)
        |   连接由设备侧主动发起（出站），hub 从不反向拨号
        |
 farwork-agent（被接管的设备，常驻）  ---stdio JSON-RPC-->  codex app-server（`codex app-server --stdio`）
```

| 组件 | 位置 | 职责 | 对外 |
| --- | --- | --- | --- |
| `farwork-hub` | `crates/farwork-hub` | `/api/v2` 服务端：REST + 4 个 WS（dashboard / session / **connector** / **设备终端流**）+ `POST /ws-ticket`、认证、PostgreSQL 持久化、connector ingress 的 hub 半边、文件浏览中继（`routes/fs.rs` + `fsrelay.rs`，按 `requestId` 配对）、设备终端中继（`routes/terminals.rs` + `termrelay.rs`，按 `terminalId` 配对）、`501` 兜底 | 端口 7300 |
| `farwork-timeline` | `crates/farwork-timeline` | timeline **纯域逻辑**：seq 分配、`content_hash` 去重、`order_seq`、游标恢复、每会话写栅栏。无 IO、无时钟 | 库 |
| `farwork-protocol` | `crates/farwork-protocol` | **M1 遗留**：M1 线协议类型，仅 server/connector 使用 | 库 |
| `farwork-server` | `crates/farwork-server` | **M1 遗留**：`/api/health`、`/api/devices`、`/ws/connector`、`/ws/client` 终端骨架 | 端口 7300（与 hub 冲突） |
| `farwork-agent` | `crates/farwork-agent` | **M3 真正的 connector**：主动出站连 hub 的 `/connector/ws`，按 `session.start` 拉起本地 `codex app-server`（或 `--runtime echo` 的假后端），把 codex 的 JSON-RPC 翻成 Ingress Protocol v1 上行；另应答只读 `fs.request`、按 `terminal.open` 起 PTY 并双向转发（`src/term.rs`）——两者的沙箱根都是 `--workdir`（`src/fsops.rs`） | 无监听端口（只出站） |
| `farwork-connector` | `crates/farwork-connector` | **M1 遗留**：按 `terminal.open` 起 PTY 并双向转发，只连 `farwork-server` 的 `/ws/connector`；**没有 ingress 协议，也没接 hub** | 出站连接 |
| `apps/web` | `apps/web` | SvelteKit 静态 SPA：登录、connectors/项目/会话、timeline、发消息、WS 增量、审批卡、新建会话表单（`#/new-session`）、会话中断、文件面板（只读浏览 + 文本预览）、会话页终端面板（xterm.js，`src/components/TerminalPanel.svelte`） | dev 端口 5173 |

> **`farwork-agent` 与 `farwork-connector` 是两件不同的东西，不是一次重命名。** `farwork-connector`（M1）说的是终端帧协议（`terminal.open` / `terminal.data` / `terminal.exit`），只跟 `farwork-server` 讲话，设备 id 每次启动重新生成；`farwork-agent`（M3）说的是 Ingress Protocol v1（`hello` / `session.*` / `item.upsert` / `notice.*`），只跟 `farwork-hub` 讲话，身份落盘在 `$FARWORK_HOME/agent.json`。两者不共享线协议也不共享代码路径；M3 没有删除 `farwork-connector`，它仍是 M1 终端探针。

**关键分工：timeline 的语义不在 hub 里。** `farwork-timeline` 拥有语义（序列分配、按内容哈希去重、`order_seq`、游标恢复、以及「宁可拒绝也不发部分 delta」的规则），hub 只拥有**持久化与传输**——从 PostgreSQL 水合引擎、回写已提交条目、wire 适配（上游多出的 `source` / `completedAt` / `protocolVersion` 由 hub 补）、以及进程内每会话引擎缓存。出处：`crates/farwork-hub/src/timeline.rs:1-20`。

由此产生两条可验证的性质：

- **进程重启不改变语义**：已提交条目与水头（`timeline_items` + `sessions.seq`）都在 PostgreSQL，重放顺序即 `updated_seq` 顺序。
- **无法重放就明说**：存了 `revision > 1` 的条目无法按重放复原（重放会退化成 revision 1），水合时直接给出 refetch 信号，而不是假装一致（`crates/farwork-hub/src/timeline.rs:18-20`）。

## 2. 端口与环境变量

| 端口 | 用途 | 说明 |
| --- | --- | --- |
| `7300` | `farwork-hub` | `/api/v2` REST + 4 个 WS；`GET /healthz` 也在此 |
| `5173` | web dev server | Vite 代理 `/api` 与 `/ws` → `127.0.0.1:7300`，`ws: true`（`apps/web/vite.config.ts`） |
| `7300` | `farwork-server`（M1 遗留） | **与 hub 默认同端口，不能同时起**；要跑探针用 `--port` |
| — | `farwork-agent`（M3） | 不监听端口：只向 `--hub` 指定的 hub **主动出站**（默认 `ws://127.0.0.1:7300`）。设备因此不需要公网 IP，也不需要端口映射 |

| 环境变量 | 默认 | 行为 | 源码 |
| --- | --- | --- | --- |
| `DATABASE_URL` | `postgres://tianjun@127.0.0.1:5432/farwork` | hub 的 PostgreSQL 连接串。**库必须已存在**，表由迁移创建；连接失败则启动失败 | `farwork-hub/src/config.rs:6,27` |
| `FARWORK_HUB_PORT` / `PORT` | `7300` | 监听端口，`FARWORK_HUB_PORT` 优先，其次 `PORT`；`--port <n>` 覆盖两者 | `config.rs:7,28-33`；`main.rs:32-34` |
| `FARWORK_HUB_SECRET` | `agent-server-dev-secret` | **未设置时打印 warn**（`FARWORK_HUB_SECRET is not set; falling back to the development secret`）并回退到开发密钥 | `config.rs:9,34-37` |
| `FARWORK_HUB_SETUP_TOKEN` | 未设置 | 首个 admin 注册令牌。未设置时按需生成随机值并 `warn!` 打到日志（30 分钟 TTL，过期轮换），从不出现在 HTTP 响应里 | `config.rs:10,38-40`；`state.rs:55-100` |
| `FARWORK_HUB_DB_POOL` | `10` | sqlx 连接池上限；非法值静默回退 | `config.rs:41-44` |
| `TEST_DATABASE_URL` | 未设置 | **仅测试**：`tests/e2e.rs` 的目标库。缺失时打印 SKIP 并直接返回（见 §8） | `tests/e2e.rs:125-138` |
| `PUBLIC_FARWORK_API` | 空 | 仅 web 构建期：API base。留空 = 同源（dev 走 Vite 代理） | `apps/web/.env.example`；`src/lib/api/client.ts:3` |
| `FARWORK_HUB_DEV_RUNTIMES` | 空 | **dev-only，默认关闭**：逗号分隔的额外 runtime 名，追加到 hub 的允许集合（默认集合是协议规定的 `codex/claude/opencode/acp/dsh`，**不含 `echo`**）。非空时启动打一条 WARN：`dev-only runtimes enabled: <列表> (never set FARWORK_HUB_DEV_RUNTIMES in production)`；不设置时行为完全不变 | `routes/sessions.rs:62,84-120`；`main.rs:36` |
| `FARWORK_CONNECTOR_TOKEN` | 未设置 | **仅 agent**：`--token` 的等价环境变量（`--token` 优先）。两者都没有时 agent 退出码 1，报 `no connector token` | `farwork-agent/src/cli.rs:8,127-134`；`main.rs:43-45` |
| `FARWORK_CODEX_BIN` | `codex` | **仅 agent**：codex 可执行文件路径（`--codex-bin` 覆盖）。agent 以 `app-server --stdio` 拉起它 | `cli.rs:9,135-143`；`hub.rs:375` |
| `FARWORK_HOME` | `~/.farwork` | **仅 agent**：身份文件目录；文件是 `$FARWORK_HOME/agent.json`，未设置时 `~/.farwork/agent.json` | `identity.rs:37-47` |

## 3. `/api/v2` 端点（已实现）

`/api/v2` 下共 **46 个已实现的 method+path 组合**（**含** §5 单独记录的 `GET /connector/ws`；只数 §3 这几节是 45），由 **38 次** `.route(...)` 注册（auth 5 + connectors 5 + fs 2 + projects 3 + sessions 11 + notices 3 + shares 3 + connector_ws 1 + terminals 2 + ws 3 = 38；`routes/mod.rs:20-36` 合并 11 个模块、12 次 `.merge`——`shares` 贡献两个 router：认证面与公开面；其中 `unsupported` 子 router 用宏注册 47 条 `501` 路径，见 §4）。另有 `GET /healthz` 挂在根（`lib.rs:33,35`）。

### 3.1 认证 —— `routes/auth.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/auth/config` | 注册开关、口令盐/参数等前端引导信息 | `auth.rs:28` |
| `POST` | `/auth/password-salt` | 取该邮箱的口令派生盐 | `auth.rs:29` |
| `POST` | `/auth/register` | 注册。**首个账号**需 setup token 且成为 `admin`；之后受 `registration_open` 控制 | `auth.rs:30`；`auth.rs:187-209` |
| `POST` | `/auth/login` | 校验 PBKDF2 派生值，返回 Bearer token | `auth.rs:31` |
| `GET` | `/auth/me` | 当前用户 | `auth.rs:32` |

### 3.2 connectors（设备） —— `routes/connectors.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/connectors` | 列表 | `connectors.rs:25` |
| `POST` | `/connectors` | 手工登记 connector，返回 `connectorToken`（仅此一次明文）。带 `installationId` 时按 `(user, installationId)` 派生 id，重试即重新签发凭证（幂等） | `connectors.rs:25,114-181` |
| `GET` | `/connectors/{connector_id}` | 详情 | `connectors.rs:26-31` |
| `PATCH` | `/connectors/{connector_id}` | 改名等 | `connectors.rs:26-31` |
| `DELETE` | `/connectors/{connector_id}` | 删除 | `connectors.rs:26-31` |
| `POST` | `/connectors/{connector_id}/revoke` | 吊销 token | `connectors.rs:32` |
| `GET` | `/connectors/{connector_id}/preferences` | 偏好设置 | `connectors.rs:33-36` |
| `POST` | `/connectors/{connector_id}/sessions/archive-all` | 批量归档该设备下会话 | `connectors.rs:37-40` |

> `connectors.status` 现在有真实值（M3）：握手成功时写 `online`，socket 结束**且该连接仍是当前路由**时写 `offline` —— 被顶替的旧连接不能把新连接标成离线（`ingress.rs:137-160`；`connector_ws.rs:118-132,193-200`）。M2 时代「恒为 `offline`」的说法已失效。

### 3.3 projects —— `routes/projects.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/projects` | 列表，每个项目带 `active_count` / `archived_count` / `last_activity_at` | `projects.rs:18,92-101` |
| `POST` | `/projects` | 创建 | `projects.rs:18` |
| `GET` | `/projects/{project_id}` | 详情 | `projects.rs:19-22` |
| `PATCH` | `/projects/{project_id}` | 改名/置顶 | `projects.rs:19-22` |
| `DELETE` | `/projects/{project_id}` | 删除 | `projects.rs:19-22` |
| `GET` | `/projects/{project_id}/sessions` | 该项目下的会话分页 | `projects.rs:23-26` |

### 3.4 sessions —— `routes/sessions.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/sessions` | 会话分页（不透明 `cursor`，按 `pinned/sort_at/updated_seq/id` 排序） | `sessions.rs:28,649-665` |
| `POST` | `/sessions` | **导入**一个已存在于运行时的会话：必须带 `externalSessionId`（否则 422 提示改用 `create-and-start`）。导入会话只读（`takeover:false`） | `sessions.rs:28,464-520` |
| `GET` | `/sessions/list` | 不分页的 dashboard 清单（行数上限 100，取 `pinned/sort_at/updated_seq/id` 序） | `sessions.rs:29,667-693` |
| `POST` | `/sessions/create-and-start` | 建会话 + 落首条消息 + **下发 `session.start`**。首条用户消息随 `session.start` 的 `prompt` 走（此刻还没有流可以 `session.message`）；`runtime` 必须在允许集合内（`echo` 只在 `FARWORK_HUB_DEV_RUNTIMES` 里才放行，否则 422）。connector 不在线时仍回 201/200，但 `runtimeDispatched:false` + `connectorResult.code = connector_offline` | `sessions.rs:30,522-647` |
| `GET` | `/sessions/{session_id}/meta` | 会话元数据 | `sessions.rs:31-34` |
| `PATCH` | `/sessions/{session_id}/meta` | 改标题/置顶/归档/takeover | `sessions.rs:31-34` |
| `GET` | `/sessions/{session_id}/timeline` | 条目读接口，`mode=latest\|changes\|history` + `limit/afterSeq/beforeOrderSeq`。**客户端不得轮询**（见 AGENTS.md 4.3） | `sessions.rs:35,775-812` |
| `GET` | `/sessions/{session_id}/snapshot` | 一次拿齐：会话 + runtime state + timeline 首页 + **真实 notices**（来自 `session_notices`）+ `eventCursor`。`approvals` 仍是空数组、`effectiveCapabilities`/`runtimeCapabilities` 仍是空集 `revision:0`（理由见 §5.10） | `sessions.rs:36,814-852` |
| `GET` | `/sessions/{session_id}/events` | 事件补拉，`after=seq:N`；无法无缝续接时回 `snapshotRequired:true` | `sessions.rs:37,866-920` |
| `GET` | `/sessions/{session_id}/runtime/state` | 运行时状态（无则 404）。M3 起由 connector 的 `session.state` / `session.exit` 真实驱动 | `sessions.rs:38-41,922-935` |
| `GET` | `/sessions/{session_id}/runtime/capabilities` | 仍是空集 `revision:0`：Ingress Protocol v1 没有 capability 帧，没协商过就不编 | `sessions.rs:42-45,937-950` |
| `POST` | `/sessions/{session_id}/runtime/messages` | 发消息：先落 timeline（`clientMessageId` 幂等，重复不重放），**提交之后**才向活跃流下发 `session.message`。要求会话可写：非归档且 `takeover`，否则 409。响应里的 `runtimeDispatched` 才是「是否真的投给了 agent」 | `sessions.rs:46-49,952-1044` |
| `POST` | `/sessions/{session_id}/runtime/interrupt` | 下发 `session.interrupt`。没有活跃流 → 409 `session_not_running`；connector 不在线或 socket 已断 → 409 `connector_offline`。拿到 200 就意味着帧真的进了 socket | `sessions.rs:50-53,1050-1093` |

### 3.5 文件浏览（connector 中继，只读） —— `routes/fs.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/connectors/{connector_id}/fs/list` | 列**一层**目录：body `{"path":"<绝对路径>"}`；成功 `200` 回 `{path, entries:[{name,path,kind,sizeBytes,modifiedAt}], serverTime}`，其中 `path` 是**规范化之后**的目录路径（要往下钻就用它） | `fs.rs:29,39-60`；`fsops.rs:97-139` |
| `POST` | `/connectors/{connector_id}/fs/readText` | 读整个文本文件：body 同上；成功 `200` 回 `{path, content, sizeBytes, truncated, serverTime}`。`truncated` 恒为 `false` —— 超过 1 MiB 直接 `413 too_large`，不返回截断内容冒充完整 | `fs.rs:30,62-85`；`fsops.rs:142-168` |

- **hub 不碰设备的文件系统**：它铸 `requestId`、下发 `fs.request`、按 `requestId` 等 `fs.response`（帧定义与配对见 §5.5）。10 秒（`FS_RESPONSE_TIMEOUT`，`fs.rs:24-25`）等不到回应 → `504 code=fs_timeout`（`fs.rs:134-145`）。
- **错误映射**（`fs.rs:96-181`）：`path` 缺失或不是绝对路径 → `422 code=invalid_path`；connector 不存在 / 已吊销 / 不属于当前用户 → `404 code=connector_not_found`；该 connector 没有活跃 socket → `503 code=connector_offline`；connector 自己报的失败按 `not_found`→`404`、`permission_denied`→`403`、`too_large`→`413`、其余（含 `io_error`）→`502` 映射，并把 connector 的 `code` 与 `message` **原样**透传给浏览器（不改成 hub 的措辞）。
- **只读**：创建 / 改名 / 删除都没有端点，也没有对应的 ingress 帧。

### 3.6 设备终端（connector 中继） —— `routes/terminals.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/connectors/{connector_id}/terminals` | 在设备上开一个终端：body `{cwd?, cols?, rows?}`（cols/rows 缺省 80×24）。成功 `201` 回 `{terminal:{id, connectorId, cols, rows, createdAt}, serverTime}` —— 终端对象**恰好五个键**，没有 `cwd`、没有 `status` | `terminals.rs:45,85-173`；`view()` `:73-83` |
| `GET` | `/connectors/{connector_id}/terminals/{terminal_id}/stream?ticket=...` | 浏览器侧 PTY 流（WebSocket）：上行 `terminal.input` / `terminal.resize` / `terminal.close`，下行 `terminal.opened` / `terminal.data` / `terminal.exit` / `terminal.error`（+ 15s `keepalive`） | `terminals.rs:46-49,224-383` |

- **错误映射**（`terminals.rs:91-115,205-222`）：connector 不存在 / 已吊销 / 不属于当前用户 → `404 code=connector_not_found`；该 connector 没有活跃 socket → `503 code=connector_offline`；`cols`/`rows` 不在 `1..=2000` → `422 code=invalid_size`；`cwd` 不是绝对路径 → `422 code=invalid_path`。
- **`201` 只说明「已落库并已下发」**：写进 `terminals` 表且 `terminal.open` 已进 connector socket，**不代表 PTY 已起**。agent 仍可能拒绝（cwd 越界 → `permission_denied`、PTY 起不来 → `spawn_failed`），拒绝以 `terminal.error` 回来，并落进 `error_code`/`error_message`（§5.12）。
- 这两条入口**不在 `501` 表里**：它们是 connector 命名空间的新端点，表里那两条 `/sessions/{id}/terminal*` 是会话命名空间的另一组，仍回 `501`（§4）。
- 帧定义、浏览器侧帧形状与握手语义见 §5.11–§5.12；三处同改纪律见 AGENTS.md 4.9。

### 3.7 审批（notices） —— `routes/notices.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/sessions/{session_id}/runtime/notices` | 该会话的审批列表，字段 `id/kind/status/title/body/options/decision/createdAt/updatedAt`。`decision` 是 hub 侧的**附加**字段：契约的 notice 形状没有表达「怎么关掉的」的位置，而只看 `status` 会让浏览器分不清 approve 与 reject | `notices.rs:25,253-277` |
| `POST` | `/sessions/{session_id}/notices/{notice_id}/respond` | 浏览器契约里的拼法；body `{"decision":"approve"\|"approve_for_session"\|"reject"}` | `notices.rs:29-32,279-341` |
| `POST` | `/sessions/{session_id}/runtime/notices/{notice_id}/respond` | 同一 handler 的别名（原 `501` 表用的就是 runtime 命名空间，为兼容保留） | `notices.rs:33-36` |

三条行为契约（M3 时定死，别改成「看起来更快」的版本）：

- **只下发，不乐观 resolve**：respond 成功时响应里的 `notice.status` 仍是 `open`；`resolved` 只有 connector 的 `notice.resolved` 能写（`notices.rs:330-341`）。
- **投递失败就报错**：connector 不在线或 socket 已断 → `409 connector_offline`，审批保持 `open`。浏览器点了按钮不等于 agent 收到了决定。
- **状态机**：非 `open` 的 notice 再 respond → `409 notice_already_resolved`；未知 notice → `409 notice_not_found`；`decision` 不在三值内 → 422。

### 3.8 分享 —— `routes/shares.rs`

| 方法 | 路径 | 鉴权 | 说明 | 源码 |
| --- | --- | --- | --- | --- |
| `POST` | `/sessions/{session_id}/shares` | Bearer | body `{"scope":"session"\|"message", "messageId"?, "allowedFileIds"?}`。成功 `201` 回 `{share:{id, scope, urlPath, createdAt}, serverTime}`；`urlPath` 是 `/api/v2/public/shares/{id}`——**能真正取到内容的那个路径**，不是前端页面路由 | `shares.rs:47,110-186` |
| `GET` | `/public/shares/{share_id}` | **无** | 公开读：`{share:{id, scope, sessionTitle, createdAt, timeline:{items}}, serverTime}`。行不存在或已软删 → `404 share not found` | `shares.rs:57,191-220` |
| `DELETE` | `/sessions/{session_id}/shares/{share_id}` | Bearer | 软删（写 `deleted_at`），成功 `204` 且**无 body**；重复删除、别人的 share、别人的会话 → 一律 `404`（不是 `403`：不让调用者探测别人的会话存在） | `shares.rs:48-51,222-246` |

四条语义（这张表的全部理由）：

- **① 快照在 per-session engine mutex 内拍，所以永远是已提交 head**：`capture_items` 直接调 `TimelineHub::snapshot`（`shares.rs:256-275`），它走 `locked()` 拿**同一把**每会话 `SessionEngine` 互斥锁（`timeline.rs:68,84-94,103-113`）——`write_message` / `apply_runtime_item` 的「apply + persist」握着同一把锁。所以分享里不可能出现「已进 engine 但还没写进 `timeline_items`」的条目，也不会拍到半截的写入。
- **② 公开读只 `SELECT snapshot_json` 一列，条目走白名单**：`SHARE_COLUMNS = "id, scope, snapshot_json, created_at"`（`shares.rs:42`），`user_id`/`session_id`/`message_id` **根本不出库**；条目由 `shared_item` 按白名单组装成七个键 `id`/`type`/`status`/`role`/`content`/`orderSeq`/`itemTime`（`shares.rs:282-312`）。**白名单而不是删字段**：`sessionId`/`source`/`contentHash`/`updatedSeq`/`revision` 从来没被复制进 `snapshot_json`，`connectorId`/`workspacePath`/`cwd` 也从来没进过，所以泄露在**结构上**不可能发生，而不是「记得别写进去」。键集合被单测钉死：`a_shared_item_carries_only_the_public_whitelist`（`shares.rs:350-376`）断言恰好这七个键。
- **③ 免鉴权路由是单独 mount 的，不在 `AuthedUser` 里**：`public_router()` 只注册 `GET /public/shares/{share_id}`（`shares.rs:56-58`），handler 的提取器里**没有** `AuthedUser`（`shares.rs:191-194`），由 `routes/mod.rs:30-32` 单独 `.merge` 进来。公开面不是「容忍错误的 token」，而是根本没有鉴权提取器——所以 `Authorization: not-a-real-token` 的公开读照样 `200`，同一串垃圾 token 打 `/sessions/{id}/snapshot` 仍然 `401`（`tests/shares.rs:494-523`）。
- **④ 删除是软删（`deleted_at`），公开读对已删的返回 `404`**：`DELETE` 只 `update shares set deleted_at = ...`（`shares.rs:231-244`），公开读的查询恒带 `and deleted_at is null`（`shares.rs:314-321`）。于是「撤销过的链接」与「从未存在的 id」回**同一个 404**，不给「这个 id 曾经有效过」这条探测面。

**空 timeline 的分享按上游口径 `422 there is no content to share`**（`shares.rs:136-140`）：宁可拒绝，也不发一个空页面——对访客来说那看起来就是 bug。配套两条：`scope:"message"` 必须给 `messageId`（缺失 `422`、不在本会话 `404`，`shares.rs:120-133,266-273`），`scope` 是闭集 `session|message`（其他值 `422`，`shares.rs:66-75,119`）。快照自带 `version`/`sessionTitle`/`eventCursor`（`shares.rs:142-147`），所以公开读不必回表查会话。

### 3.9 WebSocket —— `routes/ws.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/ws-ticket` | 换取一次性票据（60s TTL，`scope` 必须是 `{sessionId}` / `{terminalId}` / `{dashboard:true}` **之一**；`{terminalId}` 用于设备终端流，铸票前校验归属） | `ws.rs:33,112-171` |
| `GET` | `/dashboard/ws` | 总览流：connectors / projects / 会话状态 | `ws.rs:34,292-320`（socket 循环 `398-420`） |
| `GET` | `/sessions/{session_id}/ws` | 会话事件流：先发 `session.subscribed`，随后增量；15s keepalive | `ws.rs:35,159-291` |

### 3.10 `GET /healthz`（不在 `/api/v2` 下）

```json
{"status":"ok","version":"0.1.0"}
```

源码：`crates/farwork-hub/src/lib.rs:33,45-48`。

## 4. `501 not_implemented`：**47 条路径**

`crates/farwork-hub/src/routes/unsupported.rs:19-78` 用一个宏注册了 47 条路径，全部 `any(not_implemented)`，返回 `501` 且 body 里带 `"METHOD /path is not implemented in this build"`（`unsupported.rs:82-90`）；未注册的其余路径落到 `unknown_path` → `404`（`unsupported.rs:79,92-100`）。

M3 从这张表里**移出了 3 条**并改为真实实现：`/sessions/{id}/runtime/interrupt`（§3.4）、`/sessions/{id}/runtime/notices` 与 `/sessions/{id}/runtime/notices/{notice_id}/respond`（§3.7）。移出这 3 条不等于那组都补齐了：steer / commands / selections / catalogs / sync / takeover 仍在表里。

**分享又移出 2 条，49 → 47**：`/sessions/{id}/shares`（`POST`）与 `/sessions/{id}/shares/{*rest}`（其中 `DELETE .../{share_id}`）已由 `routes/shares.rs` 真实实现（§3.8），所以表里只剩账号级的 `/shares` 与 `/shares/{*rest}`。公开读 `GET /public/shares/{id}` **从来不在本表**——它的前缀是 `/public/...` 而不是 `/shares*`，是这一批新加的一条路径，不是从 501 里移出来的。

**文件浏览的真实端点不在本表**：交付的是 connector 命名空间下的 `POST /connectors/{id}/fs/list` 与 `/fs/readText`（§3.5，`routes/fs.rs`）；本表里的 `/sessions/{id}/files*` 是另一套（会话命名空间下的文件接口，写操作归它，尚未实现），仍回 `501`。文件浏览的交付因此没有动过这个数字：它的入口从一开始就是另一组路径（这几轮里真正从这个数字上移走的只有 interrupt / notices / notices respond 三条，以及分享的两条）。

**设备终端同理**：交付的是 connector 命名空间的 `POST /connectors/{id}/terminals` 与 `GET /connectors/{id}/terminals/{terminal_id}/stream`（§3.6、§5.11–§5.12）；本表里的 `/sessions/{id}/terminal` 与 `/sessions/{id}/terminal/{*rest}` 是会话命名空间的另一组，**仍回 `501`**。所以终端交付同样没有改这个数字。

| 分组 | 路径（方法不限） |
| --- | --- |
| 会话运行时控制（steer / commands / selections / catalogs / sync / takeover） | `/sessions/{id}/runtime/steer`、`/runtime/commands`、`/runtime/selections`、`/runtime/catalogs/{catalog}`、`/sessions/{id}/sync`、`/sessions/{id}/takeover` |
| 终端 / 会话文件接口（**与已交付的 connector 文件浏览不是同一组**，见上方说明） | `/sessions/{id}/terminal`、`/sessions/{id}/terminal/{*rest}`、`/sessions/{id}/files`、`/sessions/{id}/files/{*rest}` |
| 分享（**只剩账号级清单**：会话级 create/delete 与公开读已实现，见 §3.8） | `/shares`、`/shares/{*rest}` |
| 批量会话维护 | `/sessions/archive`、`/sessions/unarchive`、`/sessions/read` |
| 设备运行时管理 | `/connectors/{id}/runtimes`、`/connectors/{id}/runtimes/{*rest}`、`/connectors/{id}/protocol/capabilities`、`/connectors/{id}/pairing`、`/runtimes`、`/runtimes/{*rest}`、`/devices`、`/devices/{*rest}` |
| OAuth / 邮件码 / 移动端扫码登录 | `/auth/oauth/start|callback|finalize`、`/auth/mobile-login/{qr,exchange,refresh,request,status,confirm}`、`/auth/email-code`、`/auth/change-password`、`/auth/me/email|profile|avatar` |
| 实例管理 / 公告 / agents / usage / 配对 | `/admin`、`/admin/{*rest}`、`/announcements`、`/announcements/{*rest}`、`/agents`、`/agents/{*rest}`、`/pairing`、`/pairing/{*rest}`、`/usage`、`/usage/{*rest}` |

另有一处**带内** `501`：`POST /sessions/{id}/runtime/messages` 携带非空 `attachments` 时返回 `501 attachments are not implemented in this build`（`sessions.rs:971-979`）——拒绝好过静默丢弃附件。

## 5. Connector Ingress（M3）：Ingress Protocol v1

一个端点、两个方向、一份契约。hub 半边是 `crates/farwork-hub/src/ingress.rs`（连接表 + 帧构造 + 入站解析）与 `crates/farwork-hub/src/routes/connector_ws.rs`（socket 循环）；agent 半边是 `crates/farwork-agent/src/protocol.rs`（`ClientFrame` / `ServerFrame`）与 `crates/farwork-agent/src/hub.rs`（连接循环）。设备终端另有中继半边 `crates/farwork-hub/src/routes/terminals.rs` + `crates/farwork-hub/src/termrelay.rs` 与 agent 的 PTY 宿主 `crates/farwork-agent/src/term.rs`，见 §5.11–§5.12。**两侧字段必须同改**（hub `ingress.rs` + agent `protocol.rs` + 本文），见 AGENTS.md。

### 5.1 握手与心跳

| 步骤 | 谁 | 细节 | 源码 |
| --- | --- | --- | --- |
| 1 | agent | `GET /api/v2/connector/ws?token=<connectorToken>`：token 百分号编码后拼进 query；`--hub` 已含该路径时不重复拼 | `farwork-agent/src/cli.rs:185-200` |
| 2 | hub | 明文 token **只在这一处出现**：`sha256_hex(token)` 查 `connectors.token_hash`。查不到直接 `401` + `{"detail":...}`，**不 upgrade** | `connector_ws.rs:52-70,79-81` |
| 3 | agent | 首帧必须是 `hello`；10 秒内没等到就断开 | `connector_ws.rs:41,204-224` |
| 4 | hub | 登记进进程内 `ConnectorRegistry`（`connectorId -> ConnectionHandle`）；被顶替的旧 socket 收到 `error: connection replaced by a newer connector connection` | `ingress.rs:61-130`；`connector_ws.rs:104-118` |
| 5 | hub | **先**写 `connectors.status='online'` + `last_seen_at`（并广播 dashboard 变更），**再**发 `welcome` —— 见过 welcome 的 connector 一定已经可被路由 | `connector_ws.rs:118-132`；`ingress.rs:137-160` |
| 6 | 双向 | agent 按 `welcome.heartbeatSecs`（15s）发 `heartbeat`；hub 侧 45s 内收不到任何帧即判死，发 `error: heartbeat timeout` 后断开 | `ingress.rs:23-28`；`connector_ws.rs:132-180`；`farwork-agent/src/hub.rs:137-170` |
| 7 | hub | socket 结束（含握手失败之后的全部退出路径）：关掉该 connector 所有未关流、把对应会话标 `error`，最后只在「本连接仍持有路由」时写 `offline` | `connector_ws.rs:167-201` |

### 5.2 上行（connector → hub）

hub 侧类型是 `crates/farwork-hub/src/ingress.rs:460-547` 的 `ConnectorFrame`；agent 侧类型是 `crates/farwork-agent/src/protocol.rs:245-328` 的 `ClientFrame`。两侧的 `type` 判别值逐字相同。

| `type` | wire 字段（camelCase） | hub 侧行为 |
| --- | --- | --- |
| `hello` | `agent: {name, platform, version}` | 只在首帧有意义；重复 `hello` 记 debug 后忽略 |
| `heartbeat` | `ts` | 只 `touch_last_seen`。**故意不广播 dashboard**：每 15s 重建一次总览是纯噪声 |
| `session.started` | `streamId`, `externalSessionId` | 把 codex thread id 写进 `sessions.external_session_id` 与 `session_runtime_states.external_session_id`，广播会话 meta 与 dashboard |
| `session.state` | `streamId`, `state` | 闭集校验 `running\|idle\|waiting_approval\|error`；写 `session_runtime_states.status` 与 `sessions.status`，广播运行时状态 |
| `session.exit` | `streamId`, `code` | 关流（`closed_at`）；`code == 0` → `idle`，非 0 → `error`。流是单次性的：退出后 `session.message` 无处可投 |
| `item.upsert` | `streamId`, `item: {id, type, status, role?, content, itemTime}` | 交给 timeline 的 `apply_runtime_item`：`{type,status,role,content}` 哈希去重，未变更不提交也不广播；未知 `type`/`status`/`role` 报错，**不强转** |
| `notice.upsert` | `streamId`, `notice: {id, kind, status?, title?, body?, options?}` | upsert 进 `session_notices`（同 id 可被细化 title/options），广播 `runtime.notice.updated` |
| `notice.resolved` | `noticeId`, `decision`（**无** `streamId`） | 按 `id` + 「该 connector 拥有的会话」定位，写 `status='resolved'` 与 `decision`，广播 `runtime.notice.updated` 和整份 `runtime.notice.snapshot` |
| `fs.response` | `requestId`, `ok`, `op?`, `path?`, `entries?`, `sizeBytes?`, `content?`, `truncated?`, `error?` | 交给 `FsRelay` 按 `requestId` 配给等待中的 REST 请求，**不走流路由**（`ConnectorFrame::stream_id()` 对它返回 `None`，`ingress.rs:562`）。三种形态（list 成功 / readText 成功 / 失败）见 §5.5 |
| `terminal.opened` | `terminalId` | 按 `terminalId` 路由（**不是** `streamId`）：`termrelay.mark_opened` 记住「这个终端已经起过」，并向已挂着的浏览器推 `{"type":"terminal.opened"}`；没人订阅时只记 debug（`connector_ws.rs:500-514`） |
| `terminal.data` | `terminalId`, `data`（standard padded base64） | 原样转发给该终端的浏览器 socket，hub **不解码不重编码**；没有订阅者就丢弃并记 debug（`connector_ws.rs:516-529`） |
| `terminal.exit` | `terminalId`, `code`（**键恒在，可为 `null`**） | 转发 `{type:"terminal.exit", code}` 后关通道并 `close_terminal` 落 `closed_at`（`connector_ws.rs:531-543`） |
| `terminal.error` | `terminalId`, `message`, `code` | 转发给浏览器后关通道，并 `fail_terminal` 把 `code`/`message` 落进 `terminals`：晚到的浏览器仍能看到拒绝原因（`connector_ws.rs:545-560`） |
| `error` | `message`, `streamId?` | 只记 warn（带 `streamId`），不改变会话状态、不断连 |

### 5.3 下行（hub → connector）

hub 用 `ingress.rs:308-440` 的**十一个**构造函数产出帧（设备终端四个：`terminal_open_frame` / `terminal_data_frame` / `terminal_resize_frame` / `terminal_close_frame`，`ingress.rs:388-432`）；agent 用 `protocol.rs:333-422` 的 `ServerFrame` 解析（解码入口 `parse_server_frame`，`protocol.rs:448`；未知 `type` 与畸形 payload 的区分在 `protocol.rs:432-474`）。

| `type` | wire 字段 | agent 侧行为 | hub 何时发 |
| --- | --- | --- | --- |
| `welcome` | `serverVersion`, `heartbeatSecs` | 握手完成；心跳间隔取 `max(heartbeatSecs,1)` | 握手成功后 |
| `session.start` | `streamId`, `sessionId`, `runtime`, `cwd`, `prompt` | 校验 `runtime` 与本进程 `--runtime` 一致（不一致 → `error` + `session.exit{code:2}`，绝不冒充别的运行时）；一致则 spawn `codex app-server --stdio` 并回 `session.started` | `POST /sessions/create-and-start`（`sessions.rs:621-647`） |
| `session.message` | `streamId`, `text`, `clientMessageId`（**可选**：hub 无值时不发该键；agent 必须能解析缺失与 `null`） | 追加到该流的 codex 线程（`turn/start`），`clientMessageId` 缺失时本地生成 `cmid_<uuid>` 当幂等键；无该流 → `error` | `POST /sessions/{id}/runtime/messages`，且仅当该会话有活跃流 |
| `session.interrupt` | `streamId` | 映射成 codex `turn/interrupt`；`turn/completed{status:"interrupted"}` 把在跑条目收成 `interrupted` | `POST /sessions/{id}/runtime/interrupt` |
| `notice.respond` | `noticeId`, `decision`（**无** `streamId`，与上行同形） | 在所有 live worker 里按 noticeId 找提示并回 JSON-RPC result；找不到只 warn | `POST .../notices/{id}/respond` |
| `fs.request` | `requestId`, `op`, `path` | 按 `op`（`list` / `readText`）在**阻塞线程池**里应答，回一条同 `requestId` 的 `fs.response`；沙箱外一律 `permission_denied`（§5.6），没有沙箱时也 `permission_denied`（`hub.rs:453-480`） | `POST /connectors/{id}/fs/list`、`POST /connectors/{id}/fs/readText`（`routes/fs.rs:120-124`） |
| `terminal.open` | `terminalId`, `cols`, `rows`, `cwd?`, `shell?` | 用 `--workdir` 沙箱校验 `cwd` 后起 shell（`term.rs:148-190,275-287`）；成功回 `terminal.opened`，失败回 `terminal.error`。`cwd`/`shell` 缺省时 hub **不发这两个键**（`ingress.rs:388-411`），agent 缺省语义是 `--workdir` / `--shell` / `$SHELL` | `POST /connectors/{id}/terminals`（`terminals.rs:133-139`） |
| `terminal.data` | `terminalId`, `data` | base64 解码后写进 PTY master（按字节，不断言 UTF-8）；不是合法 base64 就丢帧并 warn，未知 terminalId 只记 debug（`hub.rs:415-427`） | 浏览器 `terminal.input` |
| `terminal.resize` | `terminalId`, `cols`, `rows` | `pty.resize`；越界在 hub 侧已拒，未知 terminalId 只记 debug（`hub.rs:429-437`；`term.rs:310-314`） | 浏览器 `terminal.resize` |
| `terminal.close` | `terminalId` | 杀掉该 shell 并回收其线程/资源（`hub.rs:439-443`） | 浏览器 `terminal.close`；以及浏览器 socket 结束时 hub 补发一发，免得 PTY 变孤儿（`terminals.rs:372-378`） |
| `error` | `message`, `streamId?` | 只记日志，**不断连** | 无法路由的入站帧、心跳超时、被新连接顶替 |

**未知 `type` 必须丢帧不断连**：agent 的 `parse_server_frame` 把「未知类型」与「已知类型但 payload 不合法」分开（`protocol.rs:443-474`），前者 debug 后丢弃、后者 warn 后丢弃，socket 都不关。

### 5.4 `clientMessageId` 是可选字段（曾静默丢整轮，故两侧都必须宽容）

**当前契约**（三处已同改，见 `ingress.rs:346-368`、`protocol.rs:352-364`、`hub.rs:337-353`）：

- **hub 侧：可选字段**。`session_message_frame(stream_id, text, client_message_id: Option<&str>)` 在无值时**不输出 `clientMessageId` 键**，而不是序列化成 `null`（`ingress.rs:356-368`）。测试 `ingress::tests::session_message_frame_omits_an_absent_client_message_id`（`ingress.rs:774-793`）断言该键不存在、编码结果里不含这串字面量、键数恰为 `type`/`streamId`/`text` 三个。
- **agent 侧：`Option<String>`**。`ServerFrame::SessionMessage.client_message_id` 带 `#[serde(default, rename = "clientMessageId", skip_serializing_if = "Option::is_none")]`，因此**缺失、`null`、字符串三种都能解析**（`protocol.rs:358-364`）；测试 `protocol::tests::session_message_parses_without_a_client_message_id`（`protocol.rs:582-597`）以及 `session_message_parses_a_present_client_message_id`（含 `null` 分支，`protocol.rs:599-618`）。
- **agent 自造本地 id**：hub 未提供 id 时，agent 生成 `cmid_<uuid>` 交给 codex 的 `turn/start` 当它自己的幂等键；该 id **不回落 hub**，timeline 幂等仍只由 `unique (session_id, client_message_id)` 保证（§4.5；`hub.rs:337-353`）。
- **契约结论**：`clientMessageId` 在 ingress 上是**可选**字段；hub 无值时不发该键；agent **必须**同时能解析「缺失」与 `null`。

**为什么两侧都要宽容（踩过的坑，别改回去）**：这个缺陷真的发生过 —— hub 曾序列化 `"clientMessageId": null`，agent 曾要求必填 `String`，serde 报错后这一帧被 `parse_server_frame` 归入 `Malformed` 并丢弃（`hub.rs:268-277` 只 warn），而 hub 已经把这一轮落库并回了 `runtimeDispatched:true` —— 界面显示「已发送」，运行时毫无动静。省略该字段的 REST 调用者必踩；Web UI 恒发 `cmid-<uuid>`（`apps/web/src/lib/session.svelte.ts:194`），所以页面当时没暴露。经验：**下游可能不传的字段，上游必须能省、下游必须能缺；任一侧收紧成必填都会让整轮静默消失**。

其余差异都是「hub 比 agent 宽松」，不构成破坏，但说明「hub 接受」不等于「契约要求」：

| 字段 | agent（发送方） | hub（接收方） |
| --- | --- | --- |
| `hello.agent.platform` / `.version` | 必填 `String` | `Option<String>`，缺失也接受 |
| `heartbeat.ts` | 必填 `String` | `Option<String>` |
| `session.started.externalSessionId` | 必填 `String` | `Option<String>` |
| `session.exit.code` | 必填 `i32` | `Option<i64>`，缺失按 0 处理 |
| `item.itemTime` | 必发（agent 自己的钟） | `Option<String>` 且**只收不用**：排序一律用 hub 的 `orderSeq`，绝不用它排序或盖时间戳 |
| `notice.resolved.streamId` | 不发 | 接受并忽略（`ingress.rs:500-507`），因为该帧契约上无 `streamId` |
| `notice.upsert.status/title/body/options` | 必发（`status` 恒为 `open`） | 全部可选，缺省 `open`/空串/`null`/`[]` |

### 5.5 文件浏览：`fs.request` / `fs.response`

hub 侧：构造 `ingress.rs:376-383`、解析 `ingress.rs:448-456,583-638`、配对 `crates/farwork-hub/src/fsrelay.rs`；agent 侧：类型 `protocol.rs:110-240`、下行解析 `protocol.rs:377-383`、应答 `crates/farwork-agent/src/hub.rs:453-480`、沙箱 `crates/farwork-agent/src/fsops.rs`。

`op` 是**闭集两个**：`list` / `readText`（`ingress.rs:35-36,53-55`；`protocol.rs:110-117`）。第三种拼写是坏帧，agent 拒绝而不是猜——加第三个 op 是契约变更，必须三处同改。

**下行 `fs.request`（hub → connector）**

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `type` | `"fs.request"` | 判别值 |
| `requestId` | string | hub 铸的 **UUID v4 裸串（无前缀）**：`uuid::Uuid::new_v4().to_string()`（`ids.rs:49-51`）。connector 必须原样回填 |
| `op` | `"list"` \| `"readText"` | 闭集 |
| `path` | string | 绝对路径。hub 先 `trim()`；空串或相对路径在 hub 侧就 `422 invalid_path`，不会下发（`fs.rs:108-114`） |

```json
{"type":"fs.request","requestId":"3f1c…","op":"readText","path":"/srv/farwork/README.md"}
```

**上行 `fs.response`（connector → hub）**：三种形态共用 `type: "fs.response"`，所以 wire 上**不是一个 tagged enum**，而是同一个结构里缺省若干字段（agent 侧 `FsResponse` 全字段 `Option` + `skip_serializing_if`，`protocol.rs:155-178`）。

形态 1 · `list` 成功：

```json
{"type":"fs.response","requestId":"3f1c…","ok":true,"op":"list","path":"/srv/farwork",
 "entries":[{"name":"src","path":"/srv/farwork/src","kind":"dir","sizeBytes":0,"modifiedAt":"2026-01-02T03:04:05Z"},
            {"name":"README.md","path":"/srv/farwork/README.md","kind":"file","sizeBytes":42,"modifiedAt":"2026-01-02T03:04:05Z"}]}
```

形态 2 · `readText` 成功：

```json
{"type":"fs.response","requestId":"3f1c…","ok":true,"op":"readText","path":"/srv/farwork/README.md","content":"# farwork\n","sizeBytes":10,"truncated":false}
```

形态 3 · 失败：

```json
{"type":"fs.response","requestId":"3f1c…","ok":false,"error":{"code":"permission_denied","message":"\"/etc/passwd\" is outside the agent workdir /srv/farwork"}}
```

| 字段 | 出现在 | 说明 |
| --- | --- | --- |
| `requestId` / `ok` | 三种形态 | `ok` 是成功与失败的唯一判别 |
| `op` / `path` | 两个成功形态 | `path` 是**规范化之后**的路径；list 返回的 `path` 就是下一层的入参 |
| `entries[]` | 仅 list 成功 | 每项 `{name, path, kind, sizeBytes, modifiedAt}`；`kind` 闭集 `file`/`dir`（`protocol.rs:119-125`）且**必填**——浏览器按它决定能不能点进去，缺了算坏帧而不是「默认是文件」；`sizeBytes`/`modifiedAt` 在 hub 侧可缺省（按 `0`/`""` 兜底，`ingress.rs:591-596`），agent 恒发全字段 |
| `content` / `sizeBytes` / `truncated` | 仅 readText 成功 | `truncated` **实际恒为 `false`**（`protocol.rs:213-215`）：超限一律 `too_large`，不给「短了却自称完整」的内容；`sizeBytes` 是字节数，不是字符数 |
| `error{code,message}` | 仅失败 | `code` 闭集 `not_found` / `permission_denied` / `io_error` / `too_large`（`protocol.rs:138-147`）；失败帧不带 `op`/`path`/`entries`/`content` |

**配对、超时与错误映射**（hub 侧）：

- **按 `requestId` 配对，不按流**：`fs.response` 没有 `streamId`，`ConnectorFrame::stream_id()` 对它返回 `None`（`ingress.rs:562`）；回包被交到 `FsRelay` 里挂着的那条 oneshot（`fsrelay.rs:47-62`）。正因如此，多个文件请求可以共用同一条 socket。
- **只有收到请求的那台设备能回**：`deliver` 校验条目上的 `connector_id`，不符返回 `WrongConnector` 并只 warn（`fsrelay.rs:64-83`；`connector_ws.rs:470`）。契约不依赖「UUID 猜不到」——一台设备不能替另一台回包。
- **三条退出路径都清条目**：回包、超时、断连，谁先到谁删；没人回答的请求不会泄漏（`fsrelay.rs:1-6,87-100`）。
- **10 秒超时 → `504 code=fs_timeout`**：`FS_RESPONSE_TIMEOUT = Duration::from_secs(10)`（`fs.rs:24-25`）。超时先 `abandon` 条目再回 504，迟到的回包被当 `Unknown` 丢掉（`fs.rs:134-145`；`fsrelay.rs:22-28`）。设备卡住不能把浏览器一起卡住。
- **在途断连 → `503 code=connector_offline`**：socket 结束时 `abandon_connector` 立刻让所有等待者失败，而不是陪跑满 10 秒（`connector_ws.rs:168`；`fs.rs:126-133`）。
- **归属不符 → `404 code=connector_not_found`**：connector 不存在、已吊销、或不属于当前用户，一律 404（`fs.rs:96-106`）。**用 404 而不是 403 是故意的**：403 会告诉调用者「这个 id 存在，只是不归你」，等于把别人设备的存在性泄露出去；404 让「不存在」与「不是你的」不可区分。

### 5.6 文件访问的沙箱边界（agent 侧唯一防线）

hub 只转发路径，**边界全部在 agent 侧**（模块头注释 `crates/farwork-agent/src/fsops.rs:1-8`）：

- **根 = `--workdir`**：连接前 `Sandbox::new(opts.workdir)` 对根 `canonicalize()` 一次（`hub.rs:69-80`；`fsops.rs:61-66`）。`--workdir` 缺省取进程当前目录，随后同样 `canonicalize`；非绝对路径直接启动失败（退出码 1，`main.rs:54-65`）。
- **判定方式**：请求路径必须非空且为绝对路径（否则 `permission_denied`）→ `fs::canonicalize`（**跟符号链接**）→ 对**路径组件**做 `starts_with(root)`（`fsops.rs:73-92`）。三类逃逸因此全部被拒：
  - **符号链接**指向根外：解析后落在根外 → `permission_denied`（单测 `a_symlink_pointing_outside_the_root_is_refused`，`fsops.rs:403-425`）
  - **`../` 上跳**：同理（`fsops.rs:337-380`）
  - **前缀陷阱**：`/srv/work-evil` 不是 `/srv/work` 的下级——比较按组件而不是字符串前缀（`fsops.rs:84-90,381-402`）
- **workdir 不可解析 → 一律拒绝，绝不退化**：`canonicalize` 失败时 agent 不建沙箱、只记一条 warn（`hub.rs:70-80`），此后每个 `fs.request` 都回 `permission_denied`（`fsops.rs:187-191`；`hub.rs:465-471`）。宁可整个文件浏览不可用，也不放开成「读这台机器的一切」。
- **单文件 1 MiB 上限**：`MAX_READ_BYTES = 1024 * 1024`（`fsops.rs:17-20`）；读前按 `metadata.len()` 判、读完再按实际字节数复判（文件可能在两次检查之间变大），超限都是 `too_large` 而不是截断（`fsops.rs:144-158`）。目录当文件读 → `io_error`；非 UTF-8 → `io_error`（`fsops.rs:145-166`）。
- **列表只列一层**：目录在前、文件在后，各按名字排序（`fsops.rs:133-137`）；单个条目 stat / 读取失败只 warn 跳过，不让整次 list 崩掉。条目按 OS 报告的目录项元数据填，**不额外做越界过滤**——真正的边界在每次打开时重跑 `resolve`，所以列表里看得见的符号链接，进去照样被拒。

### 5.7 为什么 ingress 有三张必须落库的表

- **`session_streams` 让 `streamId` 跨 hub 重启存活**：`streamId` 由 hub 生成，connector 之后每条会话级帧**只**回这个 id，没有第二个会话标识。映射若只在内存，hub 重启后仍在跑的 turn 的所有 `item.upsert` 就无处路由、只能丢（`migrations/0002_connector_ingress.sql:1-12`）。
- **`session_notices` 让审批跨刷新存活**：审批是人的节奏，比浏览器会话长。待办审批必须在刷新、换设备、断线重连之后仍然是待办，所以它在表里而不在内存里。
- **`terminals` 让「终端已结束」与「终端从未存在」可区分，并让拒绝原因活过它被报告的那一刻**：浏览器要凭一次性票据才加入流，而 agent 的拒绝经常早于取票（`migrations/0003_terminals.sql:1-29`）。`closed_at` 区分已结束/不存在，`error_code`/`error_message` 让凌晨才连上来的浏览器仍能看到 `permission_denied`（重放逻辑 `terminals.rs:266-279`；理由展开在 §5.12）。

### 5.8 hub 不做乐观 resolve；断连必须收敛

- `POST .../notices/{id}/respond` 只下发 `notice.respond`，响应里 `status` 仍是 `open`；`resolved` 只由 connector 的 `notice.resolved` 写入（§3.7）。hub 抢先置 `resolved` 的话，一旦投递失败就会留下「已批准但没人执行」的假状态。
- socket 一结束就 `abandon_connector_streams`：关掉该 connector 全部未关流，把对应会话的 `session_runtime_states.status` 与 `sessions.status` 一起标 `error`（`status_reason = "connector disconnected"`）再广播（`ingress.rs:229-268`；`connector_ws.rs:167-193`）。**在途流收敛成显式 error，而不是静默丢弃。**
- 同理 `POST runtime/messages`：没有活跃流就只落库并回 `runtimeDispatched:false`，不会假装派发。

### 5.9 codex 集成的真实边界

agent 不实现 agent 逻辑，只做协议翻译：一个 `session.start` = 一个 `codex app-server --stdio` 子进程，线上是 newline-delimited JSON-RPC（`crates/farwork-agent/src/codex/transport.rs:1-40`）。method 与字段名取自本机 `codex-cli 0.162.1` 生成的 schema，不是凭记忆（`codex/mapper.rs:8-18`）。

- **审批词表映射（本仓 3 值 ↔ codex 两套 family）**：`approve` / `approve_for_session` / `reject` 在下行映射为
  - v2 family（`item/commandExecution/requestApproval`、`item/fileChange/requestApproval`）：`accept` / `acceptForSession` / `decline`
  - legacy family（`execCommandApproval`、`applyPatchApproval`）：`approved` / `approved_for_session` / `denied`

  出处 `codex/mapper.rs:1058-1070`；上行时 notice 的 `options` 恒为这三个本仓词（`mapper.rs:568-700`）。
- **`item/permissions/requestApproval` 被拒绝**：v2 的响应要求一个 `GrantedPermissionProfile`，凭空构造等于替用户授予他没看过的权限，所以回 JSON-RPC `-32601` 并上抛一条 error（`mapper.rs:652-668`）。宁可失败，也不伪造授权。
- **codex 的 `userMessage` 不复述**：用户消息已由 hub 按 REST payload 落库（带 `clientMessageId` 幂等），再上行 codex 的副本会让同一句提示在 timeline 出现两次（`mapper.rs:712-718`）。
- **失败要如实落 timeline**：codex 的 `error` 通知（例如凭据失效的 401）生成一条 `system` + `failed` 条目，而不是只写日志（`mapper.rs:536-560`）；标了 `willRetry` 的瞬时错误不落条目。
- **不给会话挂死**：审批默认 600s 超时，超时后 agent 取消该审批并说明原因（`mapper.rs:42`）；流式文本攒够 256 字符才 upsert（`mapper.rs:36`）；命令输出超过 20000 字符保留尾部并标记截断（`mapper.rs:39`）。

### 5.10 尚未接通的部分（别当成已支持）

- `snapshot.approvals` 仍是 `[]`：可消费的审批形状走 `notices`，`approvals` 这个字段没有实现（§3.4）。
- `runtime/capabilities` 仍是空集：Ingress Protocol v1 没有 capability 帧，没协商过就不编。
- steer / takeover 仍然没有 ingress 帧；文件只有**只读**的 `fs.request` / `fs.response`（§5.5），写操作没有帧。这些 REST 仍在 `501` 表里（§4）。
- 设备终端**已有**真实帧（§5.11–§5.12），入口是 connector 命名空间的 `/connectors/{id}/terminals*`；`501` 表里的 `/sessions/{id}/terminal*` 是会话命名空间的另一组，仍然 `501`（§4）。
- **分享（§3.8）没有新增任何 ingress 帧**：快照读的是 timeline 已有的投影，出口只有 REST 与那条免鉴权的公开读。所以它既不依赖 connector 在线，也不受 `snapshot.approvals` / capabilities 这两个空缺影响。
- codex 的**成功路径**（assistant 文本增量、工具执行、真实审批往返）在本机未验证：没有 Codex 凭据。见 `docs/roadmap.md` 的 M3。

### 5.11 设备终端帧（Ingress Protocol v1.2，本仓实现）

设备终端是 ingress v1 之上**新增的一组帧**（迁移头注释写的版本就是 v1.2：`migrations/0003_terminals.sql:1`）。hub 半边：构造 `ingress.rs:388-432`、入站解析 `ingress.rs:510-547`、按 `terminalId` 路由 `ingress.rs:551-581`、socket 循环里分发 `routes/connector_ws.rs:497-560`；agent 半边：类型 `protocol.rs:289-327,384-421`、PTY 宿主 `crates/farwork-agent/src/term.rs`、帧分发 `hub.rs:279-451`。

**`terminalId` 由 hub 铸**：裸 UUID v4、无前缀（`ids.rs:53-57`），浏览器 / hub / agent 全程只用这一个 id 寻址。**帧按 `terminalId` 路由，不按 `streamId`**：`ConnectorFrame::stream_id()` 对这四帧返回 `None`，`terminal_id()` 才是路由键（`ingress.rs:551-581`）；一条 connector socket 同时承载这台设备的所有终端。

**下行 hub → agent**（hub 构造 `ingress.rs:388-432`；agent 解析 `protocol.rs:384-421`）

| `type` | wire 字段 | 必填 | 说明 |
| --- | --- | --- | --- |
| `terminal.open` | `terminalId`, `cols`, `rows`, `cwd?`, `shell?` | 前三个 | `cwd` 缺省 = agent 的 `--workdir`；`shell` 缺省 = `--shell`，再退 `$SHELL`（`term.rs:86-94,275-287`；`cli.rs:45,92,124`） |
| `terminal.data` | `terminalId`, `data` | 是 | standard base64 带 padding，按字节；hub 只转发，不解码不重编码 |
| `terminal.resize` | `terminalId`, `cols`, `rows` | 是 | `1..=2000`，越界不是 clamp |
| `terminal.close` | `terminalId` | 是 | 杀掉 shell 并回收 |

```json
{"type":"terminal.open","terminalId":"1f0c…","cols":80,"rows":24}
{"type":"terminal.open","terminalId":"1f0c…","cols":120,"rows":40,"cwd":"/srv/work/sub","shell":"/bin/bash"}
```

**上行 agent → hub**（agent 构造 `protocol.rs:289-327` / `term.rs:68-74,319-334`；hub 解析 `ingress.rs:510-547`）

| `type` | wire 字段 | 必填 | 说明 |
| --- | --- | --- | --- |
| `terminal.opened` | `terminalId` | 是 | PTY 就绪。body 里**只有** `terminalId` |
| `terminal.data` | `terminalId`, `data` | 是 | PTY 输出，standard base64 带 padding，按字节 |
| `terminal.exit` | `terminalId`, `code` | 是 | `code` 键**恒在**，可为 `null`（信号终止 / 拿不到状态） |
| `terminal.error` | `terminalId`, `message`, `code` | 是 | 终端没起来：cwd 被沙箱拒、PTY 起不来等 |

```json
{"type":"terminal.opened","terminalId":"1f0c…"}
{"type":"terminal.data","terminalId":"1f0c…","data":"AAEC/w=="}
{"type":"terminal.exit","terminalId":"1f0c…","code":null}
{"type":"terminal.error","terminalId":"1f0c…","message":"\"/etc\" is outside the agent workdir /srv/work","code":"permission_denied"}
```

四条约定（都踩过或差点踩，别改成「看起来更顺手」的版本）：

- **`terminal.open` 的 `cwd` / `shell` 缺省时 hub 不发键，不是发 `null`**：`terminal_open_frame` 只在 `Some` 时插入这两个键（`ingress.rs:388-411`），agent 侧是 `Option<String>` + `#[serde(default)]`（`protocol.rs:384-397`），所以「缺失」与 `null` 都能收。这和 §5.4 的 `clientMessageId` 是同一条教训：**下游可能不传的字段，上游必须能省**；把缺省序列化成 `null` 只是换一种拼法，任何一侧的严格实现都会把它当畸形。
- **`terminal.exit.code` 恒带键、可为 `null`**：agent 侧是 `Option<i32>` 且**不加** `skip_serializing_if`（`protocol.rs:302-314`；`None` 来自信号终止或 `wait()` 失败，`term.rs:319-334`）；hub 侧收 `Option<i64>` + `#[serde(default)]`，对缺失也宽容（`ingress.rs:522-529`）——**发的时候严格，收的时候宽容**。省略 `code` 会让「信号终止」有第二种拼法，浏览器就得多判一次。
- **`data` 一律 standard 字母表 + `=` padding 的 base64，按字节**：agent 出站直接从读缓冲编码、入站直接解码进 master，整条路径不假设 UTF-8（`term.rs:10-11`；`READ_CHUNK = 8192`，`term.rs:36-38`）；hub 只校验、不重写（`terminals.rs:487-492`）。一次「hub 顺手解码再编码」就是第二份独立实现的 bug 源头。
- **尺寸界两侧同一个上界**：`MIN_TERMINAL_DIMENSION = 1`、`MAX_TERMINAL_DIMENSION = 2000`、判定 `is_valid_terminal_size`（`ingress.rs:41-47`）；agent 侧 `MAX_DIMENSION = 2000` + `checked_size`（`term.rs:32-34,387-392`）。越界一律拒绝（`invalid_size`），不 clamp：60000 列的屏幕没人能渲染。

**`terminal.error` 是为 ingress 新增的一帧（原契约没有它）**：M1 的 connector 链路（附录 A.2）只有 `open/opened/data/resize/exit/close`，没有 error 帧——只有浏览器侧的 `/ws/client`（附录 A.3）才有 `terminal.error`。ingress 若照抄 M1 的 connector 帧集合，「cwd 越界」「PTY 起不来」就只能表现为静默或裸断连：浏览器分不清「终端没起来」和「设备掉了」，而这正是最需要说清楚的一件事。所以 agent 侧补了同名帧并如实上报（`protocol.rs:315-327`、`term.rs:67-74`），hub 反过来也解析它（`ingress.rs:530-540`）。agent 的错误码是**闭集**：`permission_denied` / `invalid_size` / `invalid_terminal` / `already_open` / `spawn_failed`（`term.rs:40-46`），hub 原样透传（`connector_ws.rs:545-556`）；乱造一个码，浏览器无法据以行动（重开？放弃？）。

### 5.12 浏览器侧：终端 REST + WS

浏览器不直接说 ingress，它用 REST 开终端、用一条 WS 接 PTY 流（`apps/web/src/lib/api/workspace.ts:191-238`、`apps/web/src/lib/terminal.ts`、`apps/web/src/components/TerminalPanel.svelte`）。

**开终端：`POST /api/v2/connectors/{connector_id}/terminals`**（`terminals.rs:45,85-173`）

body `{cwd?, cols?, rows?}`（`terminals.rs:175-183`）；`cols`/`rows` 缺省 80×24（`:33-35,103-104`）。

成功 `201`，**恰好这五个键**（`view()`，`terminals.rs:73-83`；集成测试断言键集合 `terminal_bridge.rs:439-446`）：

```json
{"terminal":{"id":"1f0c…","connectorId":"c_…","cols":80,"rows":24,"createdAt":"2026-01-02T03:04:05Z"},
 "serverTime":"2026-01-02T03:04:05Z"}
```

| 情形 | 响应 | 源码 |
| --- | --- | --- |
| connector 不存在 / 已吊销 / 不属于当前用户 | `404 code=connector_not_found`（用 404 不用 403：不让别人设备的「存在」可被探测） | `terminals.rs:91-101` |
| 该 connector 没有活跃 socket | `503 code=connector_offline` | `terminals.rs:117-118,205-210` |
| `cols`/`rows` 不在 `1..=2000` | `422 code=invalid_size` | `terminals.rs:105-114` |
| `cwd` 不是绝对路径 | `422 code=invalid_path` | `terminals.rs:188-203` |

- **`cwd` 规范化**：`trim()` 后空串视为「没传」→ 不下发 `cwd` 键；非绝对路径 `422 invalid_path`；其余原样下发（`terminals.rs:188-203`）。**hub 不校验沙箱**——沙箱根是 agent 的 `--workdir`，hub 从不知道它（`terminals.rs:185-187`；`0003_terminals.sql:9-11`）。
- **`201` 的含义要说准**：它表示「已写 `terminals` 表、且 `terminal.open` 已进 connector socket」（顺序：先落库 → 注册 relay → 再 send，避免快 agent 抢在建表前回 `terminal.opened`，`terminals.rs:119-151`；日志也明写 dispatched, not proven，`:156-165`）。**PTY 是否起来了，只有 `terminal.opened` 能证明**；agent 的拒绝以 `terminal.error` 从流里回来。

**取票：`POST /api/v2/ws-ticket`**（`ws.rs:33,116-171`）

- `scope` **恰好三选一**：`{sessionId}` / `{terminalId}` / `{dashboard:true}`；0 个或 2 个以上 → 422（`ws.rs:38-48,125-145`）。
- `{terminalId}` 分支在铸票前校验该 terminal 属于当前用户，不属于 → `404 terminal not found`（`ws.rs:146-164`）。
- 票据 60s TTL、**一次性**：消费是 `delete … returning`，读即删，重放与过期都拿不到行（`ws.rs:87-114`）；存进票据行的 `scope` 就是这个 `terminalId`，握手时与 URL 里的 id 逐字比较（`ws.rs:104-108`）。**一张票只能换一条流，且只能换它自己那条终端的流。**

**接流：`GET /api/v2/connectors/{connector_id}/terminals/{terminal_id}/stream?ticket=...`**（`terminals.rs:224-285` 握手，`287-383` socket 循环）

| 方向 | `type` | 字段 | hub 行为 |
| --- | --- | --- | --- |
| 浏览器 → hub | `terminal.input` | `data`（base64） | 先校验是合法 base64（不合法 → `invalid_frame`，**在 hub 就拒**，不把垃圾发给设备），再原样下发 `terminal.data`（`terminals.rs:400-432`） |
| 浏览器 → hub | `terminal.resize` | `cols`, `rows` | 缺字段 → `invalid_frame`；越界 → `invalid_size`；通过则下发（`terminals.rs:433-465`） |
| 浏览器 → hub | `terminal.close` | 无 | 下发 `terminal.close`、`close_terminal`，但 socket **保持到 `terminal.exit` 为止**——否则浏览器分不清「我关了」与「设备没了」（`terminals.rs:466-474`） |
| hub → 浏览器 | `terminal.opened` | 无 | 起过就告诉你；晚挂上来的浏览器也能立刻拿到（`terminals.rs:303-307`；`termrelay.rs:71-90`） |
| hub → 浏览器 | `terminal.data` | `data`（base64） | 逐字节转发 |
| hub → 浏览器 | `terminal.exit` | `code`（可 `null`） | 转发后关通道、落 `closed_at` |
| hub → 浏览器 | `terminal.error` | `message`, `code` | 拒绝/结束原因；`code` 是**闭集**：`invalid_frame` / `invalid_size` / `connector_offline` / `stream_lagged` / `terminal_not_found`（以及 agent 上报的 `permission_denied` / `spawn_failed` 等，原样透传）（`terminals.rs:501-509`） |
| hub → 浏览器 | `keepalive` | `serverTime` | 15s 一次，和其他 hub socket 同一套词汇（`terminals.rs:320-335`） |

- **握手拒绝条件（顺序即契约）**：`ticket` 缺失/空 → `401`，**不 upgrade**（`terminals.rs:230-236`）；票据无效、已用过、或 `scope` 不是这个 terminalId → `401 invalid ticket`（`:237-243`）；connector 离线 → `503 connector_offline`，也在 upgrade 之前拒（`:245-251`；理由：先建 socket 再报 offline 看起来就像「一个活的终端但没输出」）；终端不归这个用户 / 不归这个 connector → **允许 upgrade**，然后在 socket 里发 `terminal.error{code:"terminal_not_found"}` 再关（`:253-265`；集成测试 `terminal_bridge.rs:642-667`）。
- **失败要留得住**：拒绝只在流里说一次，而浏览器往往还没取到票，所以 agent 的 `terminal.error` 会 `fail_terminal` 写进表（`terminals.rs:569-590`；`connector_ws.rs:545-560`），之后才取票的浏览器拿到的还是同一条原因（`terminals.rs:266-279`；集成测试 `terminal_bridge.rs:669-706` 断言 `permission_denied` 与设备原话都在）。
- **背压不静默**：广播积压 → `terminal.error{code:"stream_lagged"}` 后断开，绝不跳帧——PTY 字节流从中途截断只会让屏幕悄悄错位（`terminals.rs:343-352`；`termrelay.rs:176`）。
- **谁先走都得收尾**：浏览器断开 → 关 relay 并向设备补发 `terminal.close`，再落 `closed_at`（`terminals.rs:369-381`）；connector 断连 → `abandon_connector_terminals` 给每个未关终端推 `terminal.error{code:"connector_offline"}` 并关行（`terminals.rs:592-614`；`connector_ws.rs:176-193`）。**没有孤儿 PTY**：一条没人读的 shell 是设备上的进程泄漏。

**两条最容易写错的边界（别越界承诺）**

1. **`cwd` 的沙箱只约束「起始目录」，不约束 shell 之后能去哪**：agent 用与文件浏览同一套 `canonicalize` + 路径组件前缀判定（`term.rs:13-16,275-287`；`fsops.rs:73-92`）确认起点在 `--workdir` 内，然后**照常 exec 一个普通 shell**。shell 起来之后 `cd /`、`cat /etc/passwd` 都是它自己的能力，跟文件浏览的只读沙箱**不是一回事**。文档和 UI 都不许写「终端被关在 workdir 里」——那是做不到的承诺；能承诺的只有「终端的**起始目录**必须落在 `--workdir` 内」。
2. **`201` 只代表「已落库 + 已下发」，不代表 PTY 已经起来**（`terminals.rs:130-151,156-165`）。真正的成功信号是流上的 `terminal.opened`；失败信号是 `terminal.error`，而它同时被写进 `terminals.error_code`/`error_message`，所以**晚到的浏览器仍然看得到原因**，不会看到「终端凭空消失」（`terminals.rs:266-279`；`0003_terminals.sql:22-28`）。因此`terminals` 表存在不是为了缓存会话状态，而是为了让这三件事跨刷新、跨进程仍然成立：`closed_at` 区分「已结束」与「从未存在」、`error_code`/`error_message` 让拒绝原因活过被报告的那一刻、`user_id`/`connector_id` 让「这张票是不是你的」可判定（`0003_terminals.sql:1-29`）。

## 6. timeline 语义（由 `farwork-timeline` 定义）

契约原文见 `crates/farwork-timeline/src/lib.rs:13-27`，要点如下。

### 6.1 事件日志是密集的 `1..=cursor`

每提交一次变更恰好消耗一个事件序号，所以序列 `1..=cursor` 必然存在且不重复（`lib.rs:15-17`）。`order_seq` 是**另一条**单调分配的展示顺序，不要与事件序号混用。

### 6.2 游标是 `seq:N`

`Cursor` 的 wire 形式是 `seq:N`，`seq:0` 表示「还没看过任何事件」；上界是 `2^53-1`（`PROTOCOL_MAX_REVISION`），因为浏览器用 IEEE-754 双精度保存（`lib.rs:69-74`；`cursor.rs:10-42`）。上游 Python 服务端签发的 `seq:N` 同样可读——这是刻意保持的兼容点。

### 6.3 `content_hash` 覆盖 `{type,status,role,content}`

`state_hash` = SHA-256 over canonical JSON of `{type, status, role, content}`（`hash.rs:28-49`）。canonical JSON 规则：对象键排序、分隔符最小化、数字保留 JSON 拼写、非 ASCII 不转义、**数组顺序保留**——与 Python 的 `json.dumps(..., ensure_ascii=False, sort_keys=True, separators=(",",":"))` 对齐，因此同一 payload 在两侧得到同一哈希。

为什么不是 payload-only：一次只把 `running` 翻成 `done` 的写入 payload 完全不变，payload-only 哈希会把它判成「未变更」并静默丢弃——状态迁移就这么消失了（`hash.rs:28-35` 的注释即为此）。

### 6.4 `RefetchRequired`：宁可拒绝，不发部分 delta

`Timeline::events_after(cursor)` 要么返回**无缝后缀**，要么显式拒绝（`timeline.rs:396-435`）。`RefetchRequired` 的四种触发条件（`timeline.rs:36-69`）：

| 原因 | `reason` | 触发条件 |
| --- | --- | --- |
| `CursorAhead` | `cursor_ahead` | 客户端游标 > 已提交水头（换了实例/脏游标） |
| `TimelineReset` | `timeline_reset` | 游标 < `refetch_floor`（日志已重置或裁剪） |
| `IncompatibleItemChange` | `incompatible_item_change` | 条目 `type` 变了，delta 无法表达；upsert 时检测（`timeline.rs:743-757`） |
| `Requested` | `requested` | 运行时/运维显式要求重建 |

hub 侧的对应行为：`routes/sessions.rs:766-812` —— `after` 不可解析时同样回 `snapshotRequired:true`（不返回 422 让客户端特判）；`Recovery::RefetchRequired` 时返回 `events: []` + `snapshotRequired: true`，而**不是**空的「你已是最新」。

参考客户端就是这么做的：`apps/web/src/lib/session.svelte.ts:189-200` 用 `GET /events?after=<cursor>` 补拉，看到 `snapshotRequired` 立刻改走 `refreshSnapshot()`；WS 断开只做指数退避重连（`178-187`），**没有**任何 `/timeline` 轮询。

### 6.5 `SessionFence`：每会话独占写锁

`farwork-timeline/src/fence.rs:9-33`：异步、**按 session 分桶**的独占锁，包住「分配序号 → 落库 → 发布 → 推进水头」这一段，保证同一会话不会交叉写入。语义要点：

- **不可重入**：同一 task 里对同一会话取两次 fence 会死锁；组合写入要用 store 自己的提交锁（是另一把锁），而不是嵌套 fence。
- **按会话隔离**：慢会话不会阻塞别的会话。
- **只保证互斥，不保证公平**：waiters 的唤醒顺序不构成契约。

## 7. 持久化

- 迁移文件 **4 个**：`crates/farwork-hub/migrations/0001_init.sql` + `0002_connector_ingress.sql`（M3 ingress）+ `0003_terminals.sql`（设备终端）+ `0004_shares.sql`（分享）。启动时由 `db::migrate` 通过 `sqlx::migrate!("./migrations")` 应用，sqlx 自带 ledger，重复启动是 no-op（`db.rs:17-22`；`main.rs` 的启动序列）。
- **12 张表**（`create table` 计数）：0001 的 `users`、`app_settings`、`connectors`、`projects`、`sessions`、`session_runtime_states`、`timeline_items`、`ws_tickets`，加 0002 的 `session_streams`、`session_notices`，加 0003 的 `terminals`，加 0004 的 `shares`（`snapshot_json` 存一次写死的快照、`deleted_at` 是软删，见 §3.8）。后三张的存在理由见 §5.7：一张让 `streamId` 跨 hub 重启存活，一张让审批跨刷新存活，`terminals` 让「终端已结束 / 从未存在」可区分并让 agent 的拒绝原因活过它被报告的那一刻（列含 `closed_at`、`error_code`、`error_message`，`0003_terminals.sql:13-29`）。
- **15 个索引**（0001 十个 + 0002 两个 + 0003 两个 + 0004 一个），其中四条是语义承载点：
  - `uq_timeline_items_client_message`：`unique (session_id, client_message_id)`（`0001_init.sql:132`）——`clientMessageId` 幂等的**唯一**保证。
  - `idx_timeline_items_session_updated_seq` / `_session_order_seq`：游标补拉与展示排序的查询路径（`0001_init.sql:129-130`）。
  - `idx_session_notices_id`：`notice.resolved` 只带 notice id，解析时先按 id 找行、再校验它属于发帧的 connector（`0002_connector_ingress.sql:37-39`）。
  - `idx_terminals_connector`：connector 断连时扫「这个设备还有哪些终端没关」（`0003_terminals.sql:31-32`；`terminals.rs:594-614`）；`idx_terminals_user` 服务取票时的归属校验（`0003_terminals.sql:34-35`；`ws.rs:154-162`）。
- `ws_tickets` 走 `delete ... returning` 消费，行即删，靠 `idx_ws_tickets_expires` 清理（`0001_init.sql:144`；`ws.rs:83-108`）。

## 8. 测试的前置条件（`crates/farwork-hub/tests/`）

`crates/farwork-hub/tests/` 下五个集成测试都跑真实 PostgreSQL：`e2e.rs`（M2 纵向切片）、`connector_ingress.rs`（ingress 上下行）、`fs_bridge.rs`（文件浏览中继）、`terminal_bridge.rs`（设备终端中继）、`shares.rs`（分享快照与公开读）。下面以 `e2e.rs` 为例：

- `TEST_DATABASE_URL` **未设置** → 打印 `SKIP farwork-hub integration test: TEST_DATABASE_URL is not set.` 并附上怎么设置，然后 `return`（`e2e.rs:127-134`）。测试仍然是「通过」的——这就是 CI 必须提供数据库的原因。
- `TEST_DATABASE_URL` 为空串 → 打印 `SKIP farwork-hub integration test: TEST_DATABASE_URL is empty.` 并返回（`e2e.rs:135-138`）。
- 有值 → `bootstrap()` 连库、`db::migrate()` 建表、再 `truncate` 清空，然后跑「登录 → connectors/项目/会话 → timeline → 发消息 → WS 增量」整条链路（`e2e.rs:62-72`）。**目标库会被清空，只能指向一次性库。**

## 附录 A · M1 遗留：终端线协议

> 本节内容仍由 `crates/farwork-server` + `crates/farwork-connector` 实现（M1 骨架），**hub 不提供这些端点**。`/devices` 与终端相关路径在 hub 里一律 `501`。保留此附录是为了探针可维护，不代表现行契约。

### A.1 M1 REST（`farwork-server`）

| 方法 | 路径 | 鉴权 | 成功响应 |
| --- | --- | --- | --- |
| `GET` | `/api/health` | 设置了 `FARWORK_TOKEN` 时需 Bearer | `200` + `{"status":"ok","version":"0.1.0"}` |
| `GET` | `/api/devices` | 同上 | `200` + `{"devices":[{id,name,platform,connected,last_seen_ms}]}` |

### A.2 `/ws/connector?token=...`

| 方向 | `type` | 字段 | 说明 |
| --- | --- | --- | --- |
| C→S | `hello` | `device`: {`id`: uuid, `name`: string, `platform`: `macos`\|`linux`\|`windows`} | 首帧，完成设备注册 |
| S→C | `welcome` | `server_version`: string | 握手回执 |
| S→C | `terminal.open` | `stream_id`, `cols`, `rows` | 要求 connector 开 PTY |
| C→S | `terminal.opened` | `stream_id` | PTY 就绪 |
| C→S / S→C | `terminal.data` | `stream_id`, `data`（standard padded base64） | PTY 输出上行 / 键入下行 |
| S→C | `terminal.resize` | `stream_id`, `cols`, `rows` | **内部帧**：把浏览器 resize 转达 connector |
| C→S | `terminal.exit` | `stream_id`, `code` | PTY 退出码 |
| S→C | `terminal.close` | `stream_id` | 通知清理 |
| S→C / C→S | `ping` / `pong` | 无 | 心跳 |

### A.3 `/ws/client?token=...`（浏览器）

| 方向 | `type` | 字段 | 说明 |
| --- | --- | --- | --- |
| C→S | `devices.watch` | 无 | 订阅设备变更 |
| S→C | `devices` | `devices`: array | 订阅后立即推一次，之后变更推送 |
| C→S | `terminal.open` | `device_id`, `stream_id`, `cols`, `rows` | 请求开终端 |
| S→C | `terminal.opened` / `terminal.error` | `stream_id`（+`message`） | 开流成功 / 失败 |
| C→S | `terminal.input` / `terminal.resize` / `terminal.close` | `stream_id`（+ payload） | 键入 / 尺寸 / 主动关闭 |
| S→C | `terminal.data` / `terminal.exit` | `stream_id`（+ payload / `code`） | 输出 / 结束；`code = -1` 由 server 在 connector 断线时为在途流补发 |

### A.4 M1 的编码与生命周期约定（仅对上述帧有效）

- **base64**：`terminal.data` / `terminal.input` 的 `data` 恒为 standard 字母表、带 `=` padding；PTY 输出是任意字节，两端都按**字节**处理，禁止假定 UTF-8。
- **心跳**：任意一侧收到 `ping` 都必须回 `pong`。
- **stream 生命周期**：`stream_id` 由浏览器生成并全程复用；未知 `stream_id` 的帧丢弃即可，不要因此断开连接。
- **退出码**：`code >= 0` 为远端进程自身退出；`code = -1` 是设备断连导致的强制结束，UI 必须区分显示。
- **设备身份**：`farwork-connector` 的设备 id 每次进程启动重新生成（`Uuid::new_v4()`，未持久化），因此重启后列表里会多一条新记录，旧记录留在列表里 `connected: false`。

### A.5 历史取舍：M1 为什么用内存注册表

M1 的可验证目标是「链路是否走得通」，设备表与 stream 映射放在进程内存（`HashMap` + 一把异步锁）。代价：重启丢设备记录与 `last_seen_ms`、无法水平扩展、没有历史数据。M2 起这些状态改由 PostgreSQL 承载（见 §7），`connected` 类易失字段不进表、由活跃连接推导的原则保持不变。
