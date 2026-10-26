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
        |                          migrations/0001_init.sql … 0006_notice_interaction_type.sql，启动时自动应用
        ^
        |   WS  /api/v2/connector/ws?token=...    (Ingress Protocol v1，见 §5)
        |   连接由设备侧主动发起（出站），hub 从不反向拨号
        |
 farwork-agent（被接管的设备，常驻）
        |--- stdio JSON-RPC ----------->  codex app-server（`codex app-server --stdio`，agent 拉起子进程）
        |--- TCP 127.0.0.1 JSON-RPC --->  DSH bridge（本机已在运行，agent 只是客户端；见 §10）
```

| 组件 | 位置 | 职责 | 对外 |
| --- | --- | --- | --- |
| `farwork-hub` | `crates/farwork-hub` | `/api/v2` 服务端：REST + 4 个 WS（dashboard / session / **connector** / **设备终端流**）+ `POST /ws-ticket`、认证、PostgreSQL 持久化、connector ingress 的 hub 半边、文件浏览中继（`routes/fs.rs` + `fsrelay.rs`，按 `requestId` 配对）、设备终端中继（`routes/terminals.rs` + `termrelay.rs`，按 `terminalId` 配对）、`501` 兜底 | 端口 7300 |
| `farwork-timeline` | `crates/farwork-timeline` | timeline **纯域逻辑**：seq 分配、`content_hash` 去重、`order_seq`、游标恢复、每会话写栅栏。无 IO、无时钟 | 库 |
| `farwork-protocol` | `crates/farwork-protocol` | **M1 遗留**：M1 线协议类型，仅 server/connector 使用 | 库 |
| `farwork-server` | `crates/farwork-server` | **M1 遗留**：`/api/health`、`/api/devices`、`/ws/connector`、`/ws/client` 终端骨架 | 端口 7300（与 hub 冲突） |
| `farwork-agent` | `crates/farwork-agent` | **M3 真正的 connector**：主动出站连 hub 的 `/connector/ws`，按 `session.start` 拉起本地 `codex app-server`（或 `--runtime echo` 的假后端），把 codex 的 JSON-RPC 翻成 Ingress Protocol v1 上行；另应答只读 `fs.request`、按 `terminal.open` 起 PTY 并双向转发（`src/term.rs`）——两者的沙箱根都是 `--workdir`（`src/fsops.rs`）；`--runtime dsh` 时改为**连**一个本机已在运行的 DSH bridge（`src/dsh/`，生命周期与 codex 相反，见 §10） | 无监听端口（只出站） |
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
| `DATABASE_URL` | `postgres://tianjun@127.0.0.1:5432/farwork` | hub 的 PostgreSQL 连接串。**库必须已存在**，表由迁移创建；连接失败则启动失败 | `farwork-hub/src/config.rs:9,39-40` |
| `FARWORK_HUB_PORT` / `PORT` | `7300` | 监听端口，`FARWORK_HUB_PORT` 优先，其次 `PORT`；`--port <n>` 覆盖两者 | `config.rs:10,41-46`；`main.rs:32-34` |
| `FARWORK_HUB_SECRET` | `agent-server-dev-secret` | **未设置时打印 warn**（`FARWORK_HUB_SECRET is not set; falling back to the development secret`）并回退到开发密钥 | `config.rs:12,47-51` |
| `FARWORK_HUB_SETUP_TOKEN` | 未设置 | 首个 admin 注册令牌。未设置时按需生成随机值并 `warn!` 打到日志（30 分钟 TTL，过期轮换），从不出现在 HTTP 响应里 | `config.rs:13,51-53`；`state.rs:55-100` |
| `FARWORK_HUB_DB_POOL` | `10` | sqlx 连接池上限；非法值静默回退 | `config.rs:54-57` |
| `FARWORK_HUB_PUBLIC_URL` | 未设置 | hub 对外自报的基地址（`GET /meta` 的 `hubUrl`），规范化为 `scheme://authority`、去尾斜杠；**优先级最高**——设了它，`/meta` 不再看 `Host`/`X-Forwarded-Proto`。**非法值让 hub 启动失败**（不是回退默认），因为这个串会被逐字复制进每条配对命令 | `config.rs:25-29,58-62,91-108`；`routes/meta.rs:63-73` |
| `FARWORK_HUB_STATIC_DIR` | 未设置 | **前端静态产物的根**（`apps/web/build`）：设了 hub 就自己托管 SPA——根路径给 `index.html`，未知深链接回退 `index.html`（hash 路由），`_app/immutable/*` 发一年长缓存、`index.html` 发 `no-cache`；**未设置则完全不挂载**（`/` 仍是 404，测试与 `pnpm dev` 依赖这一点）。挂载方式是根路由的 **fallback**，所以 `/api/v2/*`、`/connector/ws`、`/sessions/{id}/ws`、`/dashboard/ws`、`/healthz`、`/install.sh` 全部优先匹配；**`/api/*` 的未知路径绝不回退成 HTML**（仍是 404 JSON）。**目录不存在或缺 `index.html` 让 hub 启动失败**（不是静默起一个没有前端的服务） | `config.rs:34,63-65,113-145`；`static_site.rs:28-71`；`lib.rs:38-56` |
| `TEST_DATABASE_URL` | 未设置 | **仅测试**：`tests/e2e.rs` 的目标库。缺失时打印 SKIP 并直接返回（见 §8） | `tests/e2e.rs:125-138` |
| `PUBLIC_FARWORK_API` | 空 | 仅 web 构建期：API base。留空 = 同源（dev 走 Vite 代理） | `apps/web/.env.example`；`src/lib/api/client.ts:3` |
| `FARWORK_HUB_DEV_RUNTIMES` | 空 | **dev-only，默认关闭**：逗号分隔的额外 runtime 名，追加到 hub 的允许集合（默认集合是协议规定的 `codex/claude/opencode/acp/dsh`，**不含 `echo`**）。非空时启动打一条 WARN：`dev-only runtimes enabled: <列表> (never set FARWORK_HUB_DEV_RUNTIMES in production)`；不设置时行为完全不变 | `routes/sessions.rs:62,84-120`；`main.rs:36` |
| `FARWORK_CONNECTOR_TOKEN` | 未设置 | **仅 agent**：`--token` 的等价环境变量（`--token` 优先）。两者都没有时 agent 退出码 1，报 `no connector token` | `farwork-agent/src/cli.rs:8,127-134`；`main.rs:43-45` |
| `FARWORK_CODEX_BIN` | `codex` | **仅 agent**：codex 可执行文件路径（`--codex-bin` 覆盖）。agent 以 `app-server --stdio` 拉起它 | `cli.rs:9,135-143`；`hub.rs:375` |
| `FARWORK_HOME` | `~/.farwork` | **仅 agent**：状态根。身份+凭证在 `$FARWORK_HOME/agent.json`（未设置时 `~/.farwork/agent.json`），DSH checkpoint 在 `$FARWORK_HOME/dsh/sync/checkpoints/`（§10.4） | `identity.rs:22-36,71-89`；`dsh/sync.rs:304-311` |
| `DSH_HOME` | 未设置时回退 `$HOME/.dsh` | **仅 agent 的 `--runtime dsh`**：bridge 端点的**回退**查找根，读 `$DSH_HOME/agents-anywhere/bridge/endpoint.json`。主路径是 `$HOME/.agents-anywhere/dsh-bridge/endpoint.json`，两者都被 `--dsh-endpoint <path>` 覆盖（§10.1） | `dsh/discovery.rs:15-22,90-108` |

## 3. `/api/v2` 端点（已实现）

`/api/v2` 下共 **63 个已实现的 method+path 组合**（**含** §5 单独记录的 `GET /connector/ws`；只数 §3 这几节是 62），由 **53 次** `.route(...)` 注册（admin 3 + auth 5 + connectors 5 + fs 4 + **meta 1** + notices 3 + **pairing 3** + projects 3 + runtime 4 + sessions 11 + shares 3 + connector_ws 1 + terminals 4 + ws 3 = 53；`routes/mod.rs:24-49` 合并 15 个带 router 的模块、17 次 `.merge`——`extract` 只是提取器工具模块（没有 router），`shares` 与 `pairing` 各贡献两个 router：认证面与公开面；其中 `unsupported` 子 router 用宏注册 **35 条** `501` 路径，见 §4）。另有两条挂在根（不属 `/api/v2`）：`GET /healthz`（`lib.rs:38`）与 `GET /install.sh`（`lib.rs:42` → `src/install.rs`，见 §3.14）。53 次注册展开成 63 个 method+path，差额来自 8 条多方法路由：`/connectors/{id}` 与 `/projects/{id}` 各挂 3 个方法（各多 2），`/admin/users`、`/admin/settings`、`/connectors`、`/projects`、`/sessions`、`/sessions/{id}/meta` 各挂 2 个（各多 1），合计 +10。

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
| `GET` | `/sessions/{session_id}/runtime/capabilities` | 会话运行时能力：`{connectorId, capabilitySet:{steer,interrupt,selections,commands}, revision, reportedAt, serverTime}`。**四个布尔全部来自 agent 上报**，hub 不推断（§5.13、§3.10）；没收到过报告、或运行结束/断连把缓存清掉之后，回**全 `false` + `revision:0` + `reportedAt:null`**——「还没有能力信息」与「能力被收回」是同一个诚实答案 | `sessions.rs:42-45,937-951`；视图 `runtime.rs:45-62` |
| `POST` | `/sessions/{session_id}/runtime/messages` | 发消息：先落 timeline（`clientMessageId` 幂等，重复不重放），**提交之后**才向活跃流下发 `session.message`。要求会话可写：非归档且 `takeover`，否则 409。响应里的 `runtimeDispatched` 才是「是否真的投给了 agent」 | `sessions.rs:46-49,952-1044` |
| `POST` | `/sessions/{session_id}/runtime/interrupt` | 下发 `session.interrupt`。没有活跃流 → 409 `session_not_running`；connector 不在线或 socket 已断 → 409 `connector_offline`。拿到 200 就意味着帧真的进了 socket | `sessions.rs:50-53,1050-1093` |

### 3.5 文件浏览（connector 中继，只读） —— `routes/fs.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/connectors/{connector_id}/fs/list` | 列**一层**目录：body `{"path":"<绝对路径>"}`；成功 `200` 回 `{path, entries:[{name,path,kind,sizeBytes,modifiedAt}], serverTime}`，其中 `path` 是**规范化之后**的目录路径（要往下钻就用它） | `fs.rs:29,39-60`；`fsops.rs:97-139` |
| `POST` | `/connectors/{connector_id}/fs/readText` | 读整个文本文件：body 同上；成功 `200` 回 `{path, content, sizeBytes, truncated, serverTime}`。`truncated` 恒为 `false` —— 超过 1 MiB 直接 `413 too_large`，不返回截断内容冒充完整 | `fs.rs:30,62-85`；`fsops.rs:142-168` |
| `POST` | `/sessions/{session_id}/files/list` | **会话命名空间**的同一件事：设备从会话解析（`session.connector_id`），相对 `path` 锚在会话自己的 `cwd` 上（绝对路径原样下发；相对却没有 `cwd` → `422 invalid_path`）。响应形状与上面那条**完全一致** | `fs.rs:33,57-67,133-166` |
| `POST` | `/sessions/{session_id}/files/readText` | 会话命名空间的 `readText`，同样锚在会话 `cwd` | `fs.rs:34-37,96-106` |

- **hub 不碰设备的文件系统**：它铸 `requestId`、下发 `fs.request`、按 `requestId` 等 `fs.response`（帧定义与配对见 §5.5）。10 秒（`FS_RESPONSE_TIMEOUT`，`fs.rs:24-25`）等不到回应 → `504 code=fs_timeout`（`fs.rs:134-145`）。
- **错误映射**（`fs.rs:96-181`）：`path` 缺失或不是绝对路径 → `422 code=invalid_path`；connector 不存在 / 已吊销 / 不属于当前用户 → `404 code=connector_not_found`；该 connector 没有活跃 socket → `503 code=connector_offline`；connector 自己报的失败按 `not_found`→`404`、`permission_denied`→`403`、`too_large`→`413`、其余（含 `io_error`）→`502` 映射，并把 connector 的 `code` 与 `message` **原样**透传给浏览器（不改成 hub 的措辞）。
- **只读**：创建 / 改名 / 删除都没有端点，也没有对应的 ingress 帧。
- **两个命名空间共用同一条中继，没有第二套**：会话级两条先 `owned_session` 解析出 `connector_id` 与相对路径锚点（`fs.rs:133-166`），之后走的还是**同一个** `relay()`（`fs.rs:169-…`）、同一个 `FsRelay` 与同一条 `fs.request` 帧——只是少让浏览器自己指定设备。差别只有两处：**别人的会话一律 `404 session not found`**（`fs.rs:139-140` → `sessions.rs:168-179`，不是 `403`：不泄露存在性），以及相对路径有了锚点。

### 3.6 设备终端（connector 中继） —— `routes/terminals.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/connectors/{connector_id}/terminals` | 在设备上开一个终端：body `{cwd?, cols?, rows?}`（cols/rows 缺省 80×24）。成功 `201` 回 `{terminal:{id, connectorId, cols, rows, createdAt}, serverTime}` —— 终端对象**恰好五个键**，没有 `cwd`、没有 `status` | `terminals.rs:45,85-173`；`view()` `:73-83` |
| `GET` | `/connectors/{connector_id}/terminals/{terminal_id}/stream?ticket=...` | 浏览器侧 PTY 流（WebSocket）：上行 `terminal.input` / `terminal.resize` / `terminal.close`，下行 `terminal.opened` / `terminal.data` / `terminal.exit` / `terminal.error`（+ 15s `keepalive`） | `terminals.rs:46-49,224-383` |
| `POST` | `/sessions/{session_id}/terminal` | **会话命名空间**开终端：设备取自会话，`cwd` 缺省取会话自己的 `cwd`，并把 `session_id` 写进 `terminals` 行。回 `201 {terminal, serverTime}`，视图与 connector 命名空间同一套五键 | `terminals.rs:53,108-130`；列 `:41`；列加在 `migrations/0005_session_terminal_scope.sql:10-11` |
| `GET` | `/sessions/{session_id}/terminal/{terminal_id}/stream?ticket=...` | 会话命名空间的 PTY 流：握手除了「票属于这个 `terminalId` 且是这个用户的」以外，还要求**这行终端是这条会话开的**——同设备但别处开的终端按「未知终端」处理 | `terminals.rs:54-57,288-295,300-391` |

- **错误映射**（`terminals.rs:91-115,205-222`）：connector 不存在 / 已吊销 / 不属于当前用户 → `404 code=connector_not_found`；该 connector 没有活跃 socket → `503 code=connector_offline`；`cols`/`rows` 不在 `1..=2000` → `422 code=invalid_size`；`cwd` 不是绝对路径 → `422 code=invalid_path`。
- **`201` 只说明「已落库并已下发」**：写进 `terminals` 表且 `terminal.open` 已进 connector socket，**不代表 PTY 已起**。agent 仍可能拒绝（cwd 越界 → `permission_denied`、PTY 起不来 → `spawn_failed`），拒绝以 `terminal.error` 回来，并落进 `error_code`/`error_message`（§5.12）。
- **两个命名空间共用同一条 `termrelay`，也没有第二张表**：会话级的两条只是先 `owned_session` 解析出设备（`terminals.rs:114`）、把 `session_id` 一起落库（`terminals.rs:126,171-181`），之后走的还是同一个 `termrelay`、同一条 `terminal.open` 帧、同一条 connector socket 与同一套 socket 循环（`attach_stream`，`terminals.rs:300-391`）。**`0005` 只在既有 `terminals` 表上加了一个可空列**（`0005_session_terminal_scope.sql:10-11`）：设备命名空间的行没有会话，硬编一个会谎报「谁开的」；PTY、几何、关闭原因、归属在两个命名空间里没有一处不同，所以不建第二张表。
- **别人的会话一律 `404`**：`/sessions/{id}/terminal*` 两条入口先过 `owned_session`（`terminals.rs:114` → `sessions.rs:168-179`），不属于当前用户的会话与不存在的会话回**同一个 404**。**两关都要过，但判的不是同一件事**：`POST /ws-ticket` 的 `{terminalId}` 分支只给「这个用户拥有的 terminal」发票（`ws.rs:154-162`），握手时再从 URL 里的 `session_id` 查出会话并校验 `user_id`，不符就与未知终端一样处理（回 `terminal.error{code:"terminal_not_found"}` 后关，`terminals.rs:326-343`）——**铸票时 URL 里还没有 session id**，所以会话归属只能在握手这一关判；两关合起来才等价于「不能拿别人的会话 id 去骑自己的票」。
- 帧定义、浏览器侧帧形状与握手语义见 §5.11–§5.12；三处同改纪律见 AGENTS.md 4.9。

### 3.7 审批（notices） —— `routes/notices.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/sessions/{session_id}/runtime/notices` | 该会话的审批列表，字段 `id/kind/status/title/body/options/interactionType/decision/createdAt/updatedAt`。`interactionType`（`approval` / `question` / … ，可空）是**运行时自己声明的交互种类**，单独一列（`session_notices.interaction_type`，`0006_notice_interaction_type.sql`；读写在 `notices.rs:46-67,93,145-171`）。**为什么不复用 `kind`**：`kind` 那三个值（`command_execution` / `file_change` / `permissions`）是 codex 的词汇，DSH 的一个 approval 是通用的 runtime interaction——把 `kind` 改义会让浏览器已渲染的 codex 审批变样。`decision` 是 hub 侧的**附加**字段：契约的 notice 形状没有表达「怎么关掉的」的位置，而只看 `status` 会让浏览器分不清 approve 与 reject | `notices.rs:25,57-90` |
| `POST` | `/sessions/{session_id}/notices/{notice_id}/respond` | 浏览器契约里的拼法；body 三选一（见下）：`{"actionId":"<运行时按钮 id>"}`、`{"decision":"approve"\|"approve_for_session"\|"reject"}`、`{"input":<结构化答案>}`（别名 `inputData`） | `notices.rs:29-32,342-470` |
| `POST` | `/sessions/{session_id}/runtime/notices/{notice_id}/respond` | 同一 handler 的别名（原 `501` 表用的就是 runtime 命名空间，为兼容保留） | `notices.rs:33-36` |

三条行为契约（M3 时定死，别改成「看起来更快」的版本）：

- **只下发，不乐观 resolve**：respond 成功时响应里的 `notice.status` 仍是 `open`；`resolved` 只有 connector 的 `notice.resolved` 能写（`notices.rs:459-470`）。
- **投递失败就报错**：connector 不在线或 socket 已断 → `409 connector_offline`，审批保持 `open`。浏览器点了按钮不等于 agent 收到了决定。
- **状态机**：非 `open` 的 notice 再 respond → `409 notice_already_resolved`；未知 notice → `409 notice_not_found`；`decision` 不在三值内 → 422。

**`options` 的契约扩展：对象是规范形态，旧字符串数组照收**

wire 上现在是 `[{"id":"allow-once","label":"允许一次","style":"primary"}, …]`，而 v1.3 的 `["approve","reject"]` **仍然合法**：`NoticeOption` 是 `#[serde(untagged)]` 的 `Action | Legacy(String)`（`ingress.rs:785-799`），一个裸字符串按「id 就是那个词」读成对象，`label` 由 `default_label` 补中文默认值（`approve`/`allow`/`allow-once` → 批准；`approve_for_session`/`approve-for-session`/`allow-always` → 本会话内批准；`reject`/`deny` → 拒绝；`cancel` → 取消；`submit` → 提交；其余原样返回；`ingress.rs:824-836`）。`to_wire` 出来的一律是对象、空 label 也补默认（`ingress.rs:801-822`），落库与 REST 都只有对象形态——**旧卡片渲染的仍是同样的三个按钮**，多出来的是 `id`/`label`/`style` 三个位置。

**`actionId` 与 `decision` 都带，也都可缺**

- 下行 `notice.respond` 与上行 `notice.resolved` 都可带 `actionId`（运行时的按钮 id）。**缺键就不发该键**，不发 `null`：`notice_respond_frame` 只在 `Some` 时插入（`ingress.rs:472-499`），`NoticeResolved` 的 `decision`/`actionId` 都是 `Option` + `#[serde(default)]`（`ingress.rs:570-585`）。
- **为什么保留 `decision`**：升级期的 Web 会**同时**发 `actionId` 和镜像过来的 `decision`，这样「只认识旧三值」的一侧仍然读得懂。hub 因此只把**不一致**当歧义（`notices.rs:389-414`）：值相同就是正常答案，值不同才是 422。保留它不是历史包袱，是升级期的兼容面。

**四种 422（都是 `unprocessable`，不是 409）**

| 情况 | 结果 | 源码 |
| --- | --- | --- |
| 两个字段都没给（空 body / 空串） | `422 {"code":"empty_answer"}` | `notices.rs:389-394` |
| `decision` 与 `actionId` 同时给出但**不相等** | `422 {"code":"ambiguous_answer"}`（带两个值，便于定位） | `notices.rs:395-405` |
| `actionId` 不在这条 notice 真正给出的 options 里 | `422 {"code":"unknown_action"}` + `expected` 列表 | `notices.rs:437-448` |
| `input`/`inputData` 都接受 | `inputData` 是 `input` 的 `serde(alias)`；转发给设备时键名恒为 `input` | `notices.rs:349-355`；`ingress.rs:488-496` |

`actionId` **两处都校验**：respond 时对着 `session_notices.options_json` 查（`notices.rs:437-448`），connector 回报 `notice.resolved` 时再查一次（`notices.rs:218-228`）。一个晚到的标签页、或桥在期间换掉的按钮，宁可 422 也不要落一个没人按过的答案。另外：只给 `decision` 而不给 `actionId` 时，仍按老规则要求它属于三值闭集（`notices.rs:407-411`）。

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

### 3.9 管理台（实例管理） —— `routes/admin.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/admin/users` | 用户目录，按 `created_at desc, id desc` 分页（`limit` 缺省 50、夹在 1..=100；`cursor` 是 base64url 的 `{created_at,id}`，坏游标 `400`）；回 `{users, nextCursor, serverTime}`，每项是七键投影 | `admin.rs:63,193-234` |
| `POST` | `/admin/users` | 建号（`email`/`password`/`role`/`displayName`）：`201` 回 `{user, serverTime}`。邮箱重复 `409`（唯一索引说了算，不先查后写）；`role` 只收 `admin|member`（其他 `422`）、`password` 必填（`422`）。口令走 `password_hash_from_plaintext`，与注册路径同一套 verifier 派生 | `admin.rs:64,248-298` |
| `PATCH` | `/admin/users/{user_id}` | 改 `role`/`disabled`/`displayName`（没给的键不动，`200` 回 `{user, serverTime}`）；未知 id `404`；会让实例失去最后一个 admin 的改动 `409 admin_lockout`（见下） | `admin.rs:65,310-379` |
| `GET` | `/admin/settings` | `{settings:{registrationOpen, emailVerificationRequired}, serverTime}`——读的就是 `/auth/config` 读的那两行 `app_settings` | `admin.rs:65,391-409` |
| `PATCH` | `/admin/settings` | 部分更新同一组开关（缺省的键不动，空 body 是 no-op），回更新后的值 | `admin.rs:65,411-425` |

三条语义（这张表的全部理由）：

- **① 权限门重读 `users` 行，不信任 token**：每个 handler 先过 `AuthedUser`（无 token 由提取器直接 `401`），再 `require_admin` 用 `find_user_by_id` **重新查一遍行**（`admin.rs:74-85`）：行不在 `401`、`disabled = true` `403 forbidden`（**被停用的 admin 同样吃 403，不是放行**）、`role != "admin"` `403 forbidden`。于是「停用某个 admin」在**下一次请求**就生效，不必等 token 过期；反向也没有 `is_admin` 列可供半真半假——`users.role` 是唯一真相（`admin.rs:9-10`）。集成测试钉死这条：`admin_endpoints_reject_anonymous_and_members`（`tests/admin.rs:246-341`，其中 `:311-341` 就是「停用后立刻 403」）。
- **② 响应是七键白名单投影，哈希材料永不出现在响应里**：`user_view` 手写 `{id, email, displayName, role, disabled, createdAt, emailVerified}`（`admin.rs:102-112`）。**白名单而不是给 `UserRow` derive `Serialize`**：`USER_COLUMNS` 里含 `password_hash`（`admin.rs:57-59`），一旦 derive，PBKDF2 的 salt 与 verifier 会随下一次加字段一起被发出去。列表、建号、改号三个响应全部只走 `user_view`；测试对**裸 body** 断言不含 `pbkdf2`/`password`/`salt`/`hash`（`tests/admin.rs:439-534`，词表在 `:461-463`）。
- **③ 不锁死自己**：`lockout_reason` 是纯函数（`admin.rs:144-167`），按优先级给三种 `409`，body 恒为 `{"detail":{"code":"admin_lockout","reason":...,"message":...}}`（`admin.rs:169-181`）：`self_disable`（admin 不能停用自己）、`last_admin`（谁都不能降级或停用**最后一个还能登录的** admin）、`self_demote`（admin 不能把自己降成 `member`；只剩一个 admin 时报 `last_admin`，因为它更强）。**并发下也成立**：`update_user` 在**同一个事务**里先 `select id from users where role = 'admin' and disabled = false for update`（`admin.rs:337-344`）拿到行锁，再用这把锁读到的行数判 `active_admin_count`（`admin.rs:345-355`），最后 `update ... returning` 并提交（`admin.rs:357-373`）。两个并发降级不可能都读到「2 个 admin」然后都成功：第二个阻塞在行锁上，醒来时那一行已不是 admin，计数变 1，于是 `last_admin` 409。单测 `self_changes_and_the_last_admin_are_refused`（`admin.rs:433-467`）+ 集成测试 `admin_cannot_lock_itself_out`（`tests/admin.rs:548-645`）。

**两个开关不等价，别写成"两个都能用"**：

- `emailVerificationRequired` **只落库并如实上报，不拦注册**：`/admin/settings` 写的行（`admin.rs:421-423`）与 `/auth/config` 读的是同一行（`auth.rs:122-124,192`），但本 build **没有邮件通道**、`/auth/email-code` 仍在 `501` 表里（`unsupported.rs:62`），`register` 也根本不看这个开关（`auth.rs:232-240` 只看 bootstrap 与 `registration_open`）。因此它今天是「存了、报了、没人执行」的诚实值；配套事实：`email_verified_at` 全仓**只有读没有写**（`migrations/0001_init.sql:8` 建的可空列），所以 `user_view` 的 `emailVerified` 今天恒为 `false`（`tests/admin.rs:363,491`）。**不要说它开启了邮箱验证。**
- `registrationOpen` 是**真开关**：非 bootstrap 时 `register` 直接 `!registration_open(pool) → 403 registration is closed`（`auth.rs:239-240`）；bootstrap（库里 0 个用户）走 setup token、不受它约束（`auth.rs:232-238`）。所以关掉后 `POST /auth/register` 真的 `403`，`PATCH /admin/settings` 的改动立刻反映在 `/auth/config`（`tests/admin.rs:700-757`）。

### 3.10 会话运行时控制 —— `routes/runtime.rs`（+ `capabilities` 在 `routes/sessions.rs`）

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/sessions/{session_id}/runtime/capabilities` | 四个能力布尔 + `revision` + `reportedAt`（行号见 §3.4）。**全部来自 agent 的 `session.capabilities` 上报**（§5.13） | `sessions.rs:937-951`；视图 `runtime.rs:45-62` |
| `GET` | `/sessions/{session_id}/runtime/catalogs/{catalog}` | `catalog` 是闭集 `model` / `permission`；回 `{catalog, items:[{id,name,description?}], revision, serverTime}`（`description` 缺省**不发键**）。未知 catalog → `422 code=unknown_catalog`，**不是**空列表——「这个运行时没有目录」与「你要的目录不存在」是两件事 | `runtime.rs:25-28,66-98`；`Catalog::parse` `runtime_caps.rs:107-116` |
| `GET` | `/sessions/{session_id}/runtime/commands` | 斜杠命令清单：`{commands:[{name,description?}], serverTime}`，`description` 缺省不发键。**没有 `?query=` / `?limit=` 过滤**：模糊匹配由浏览器在本地做（`apps/web/src/lib/commands.ts`） | `runtime.rs:29-32,100-121` |
| `POST` | `/sessions/{session_id}/runtime/steer` | 给**正在跑的回合**追加一条指令：body `{content}`（`trim()` 后为空 → `422 code=empty_content`）。成功 **`202`** 回 `{delivered:true, streamId, serverTime}`；没有活跃流 → `409 code=session_not_running`；运行时报过 `steer:false` → `501 code=steer_unsupported`；connector socket 已断 → `409 code=connector_offline`；归档或未 takeover → `409` | `runtime.rs:33,137-203` |
| `PATCH` | `/sessions/{session_id}/runtime/selections` | 改 `{model?, permissionMode?}`（至少给一个，否则 `422 code=no_selection`；空串按「没给」处理）。**只有帧真的进了 connector socket 才回 `200`** `{selections, serverTime}`，并把接受的键并进 `session_runtime_states.selections_json`；`409 session_not_running` / `409 connector_offline` / `501 code=selections_unsupported` / `422` | `runtime.rs:34-37,226-334` |

五条共同的语义（这几条是这一节的**全部**理由）：

- **读接口只读缓存，不现查设备**：catalog / commands / capabilities 三处都只 `runtime_caps().get(session_id)`（`runtime.rs:46,80,106`），缓存为空就回「空清单 / 全 false / `revision:0`」。hub 不会为了填一个列表去现问设备，也**不会编**：编一个模型列表等于给浏览器一个选项点了没用的下拉框。
- **`revision` 是 hub 自己的缓存版本**：每次接受一帧 `session.capabilities` 就 `+1`（`runtime_caps.rs:163-187`），不是运行时给的数字（运行时根本不发版本号）。会话结束、connector 断连都会把这条缓存删掉（`connector_ws.rs:379-388,196-207`），于是「能力被收回」与「从未上报」对读者是同一个答案。
- **写接口按三档拒绝，顺序即契约**：先看会话可写（归档 / takeover）→ 再看有没有活跃流（`409 session_not_running`）→ 最后才看能力（`501 *_unsupported`）。没有流时先报「没有可干预的回合」，因为那才是可行动的那个答案（`runtime.rs:160-182`）。
- **`202` / `200` 的差别是有意的**：steer 是「已投递给设备」，agent 会在 timeline 里写出这条指令（echo 后端就是这么做的，`echo.rs:179-186`），浏览器看 timeline 才知道运行时有没有理它；selections 是「帧已进 socket 才回 200」，因为一个没投出去的选择等于让用户看到运行时根本没见过的选项。
- **codex 后端会明确拒绝**：codex app-server 没有 steer / 选择模型的方法，所以 agent 对这两帧回 `error`（「this runtime cannot steer…」，`codex/session.rs:110-139`），而 codex 的 `capabilities` 帧本来就把 `steer`/`selections`/`commands` 报成 `false`（`codex/mod.rs:20-38`）——即 hub 在正常路径下不会发出这两帧。

### 3.11 WebSocket —— `routes/ws.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/ws-ticket` | 换取一次性票据（60s TTL，`scope` 必须是 `{sessionId}` / `{terminalId}` / `{dashboard:true}` **之一**；`{terminalId}` 用于终端流，铸票前校验「这个 terminal 是这个用户的」，不属于 → `404`。**会话归属只能在握手时判**：铸票时 URL 里还没有 session id，握手处才拿 `session_id` 校验 `user_id`，见 §3.6） | `ws.rs:33,112-171` |
| `GET` | `/dashboard/ws` | 总览流：connectors / projects / 会话状态 | `ws.rs:34,292-320`（socket 循环 `398-420`） |
| `GET` | `/sessions/{session_id}/ws` | 会话事件流：先发 `session.subscribed`，随后增量；15s keepalive | `ws.rs:35,159-291` |

### 3.12 配对码：`/pairing*`（hub）+ `farwork-agent pair`（agent） —— `routes/pairing.rs`、`pairing.rs`

设备不必由人手抄一个 `cxt_` token：浏览器铸一个 8 位一次性码，设备拿它去换 token。

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/pairing/start` | **需登录**。body `{ttlSeconds?}`（缺省 600，闭区间 60..=3600，越界 `422`）。`201` 回 `{pairing:{id,code,expiresAt}, serverTime}`——**`code` 只在这一个响应里出现**，库里只有摘要 | `pairing.rs:145-205` |
| `POST` | `/pairing/claim` | **免鉴权**（码就是凭证）。body `{code,name,platform?}`，`200` 回 `{connector:{id,name,deviceOs,status}, connectorToken, serverTime}`——`connectorToken` 与 `POST /connectors` 返回的是同一个对象，所以设备拿到之后的行为完全不变 | `pairing.rs:209-294` |
| `GET` | `/pairing/{pairing_id}` | 需登录，浏览器轮询用。`status` ∈ `pending`/`claimed`/`expired`，仅在已 claim 时多一个 `connectorId` | `pairing.rs:296-345` |

码的规则（纯逻辑在 `crates/farwork-hub/src/pairing.rs`）：

- **8 位、`XXXX-XXXX`，字母表 30 个符号** `23456789ABCDEFGHJKMNPQRSTVWXYZ`——Crockford 去掉易混字符，**没有 I/L/O/U/0/1**（`pairing.rs:21-28`）。归一化去连字符与空白并转大写，所以 `4f2k-9qxm` 与 ` 4F2K9QXM ` 是同一个码（`pairing.rs:60-67`）；归一化后不是 8 个合法符号就按「码无效」回答，而不是「查不到」（`pairing.rs:69-74`）。
- **只存带密钥的摘要**：`sha256("farwork-pairing-v1:{FARWORK_HUB_SECRET}:{规范化码}")`（`pairing.rs:76-79`）。8 位码只有约 39 bit，数据库里一份裸摘要能离线反推，反推出来就能抢走一个还没被 claim 的配对；把 hub secret 当 key material 之后，单看数据库拿到的东西没用（`pairing.rs:7-14`；`migrations/0007_pairings.sql:1-6`）。
- **一次性**：claim 用 `update … where claimed_at is null` 抢（`pairing.rs:267-281`），`rows_affected == 0` 就回 `409 code_already_claimed` 并连同新建的 connector 一起回滚（`pairing.rs:255-282`）。**「已用」优先于「已过期」**：码一旦花掉再报过期，会把人引导去为一个已经存在的设备重新铸码（`pairing.rs:246-253`）。
- **TTL 默认 600s，可配 60..=3600**（`pairing.rs:30-32,150-157`）。
- **按来源地址每分钟 10 次**，超了 `429 too_many_attempts`（`pairing.rs:34-36,214-219`）。键是 TCP peer 地址，**刻意不看 `X-Forwarded-For`**：本 build 没有「可信代理」概念，跟着头走等于让直连调用者换 key 绕过限额；代价是反代后面所有 claim 共用一个桶，这是「太严」而不是「没限额」（`pairing.rs:19-25`）。限流器在进程内（`ClaimLimiter`，最多跟踪 4096 个来源，重启即清，`pairing.rs:81-120`）。
- **状态码即语义**：错码 `404 invalid_code`、过期 `410 code_expired`、已用 `409 code_already_claimed`、限流 `429 too_many_attempts`、`name` 空白 `422`（`pairing.rs:108-143,225-228`）。
- **claim 与 start 是两个 router**：`public_router()` 只注册 `/pairing/claim` 且**没有 `AuthedUser`**（`pairing.rs:53-56`；`routes/mod.rs:36-38`），与 `/public/shares` 同一套做法——响应里只有它刚创建的那台设备，看不到账号。
- **`GET {id}` 不泄露存在性**：未知 id 与「别人的 id」都回 `status:"unknown"`（`expiresAt:null`）而不是 `404`，于是轮询只有一条代码路径，也无法用它探测别人的配对（`pairing.rs:310-322`）。
- **表与索引**：`pairings`（`0007_pairings.sql:19-30`），`uq_pairings_code_hash` 唯一（摘要撞车时换码重试，而不是遮蔽一个活着的码，`pairing.rs:161-188`），`idx_pairings_user` 服务「我的配对码」列表（`0007_pairings.sql:35-38`）。浏览器侧是 `apps/web/src/components/ConnectDevicePage.svelte`（`#/connect`）+ `src/lib/api/pairing.ts`。

agent 侧（`crates/farwork-agent/src/pair.rs` + `credentials.rs` + `identity.rs` + `cli.rs`）：

```bash
farwork-agent pair --hub <url> --code <code> [--name/--runtime/--workdir/--shell/--dsh-*]
```

- **子命令是裸词**：`pair` 只能出现在 argv 首位（`cli.rs:160-163`）；`--code` 只在这个子命令下有意义（否则 `CodeOutsidePair`），`pair` **必须显式给 `--hub`**——默认值是 `ws://127.0.0.1:7300` 的开发回退，拿它去兑码只会以「码无效」的样子失败（`cli.rs:113-118,228-246`）。码会被 `trim` + 转大写（Crockford 本就大小写无关，`cli.rs:228-237`）。
- **`--hub` 接受四种形式**：`http(s)://` 与 `ws(s)://` 都行，且允许已经带着 `/api/v2/connector/ws`（文档就是那么发出来的），拼接前先剥掉（`pair.rs:79-104`）。无 scheme 或其它 scheme → `UnsupportedHub`，错误文本直接给一个 `--hub https://…` 的例子（`pair.rs:29-30`）。
- **成功即写三键**：`agent.json` 多出 `hub` / `connectorId` / `connectorToken`（`identity.rs:22-36,50-57`），文件 0600、目录 0700（`identity.rs:114-137`）。**写完之后裸 `farwork-agent` 直接复用**：凭证解析顺序是「`--hub`/`--token`（含 `FARWORK_CONNECTOR_TOKEN`）→ agent.json」，显式旗标永远赢，且文件里的 token 只配它旁边的 hub（`credentials.rs:43-86`）。只有 token 没有 hub、或只有 hub 没有 token，都会指名缺哪一半并提示去 `pair`（`credentials.rs:21-41,58-79`）。
- **claim 失败 → 可操作文案**（`pair.rs:106-134`，每条单测钉住 `pair.rs:354-380`）：`404 invalid_code` → 回 Web 重新铸码；`409` → 已被别的设备用掉，重新铸码；`410` → 已过期，重新铸码；`429` → 一分钟内最多 10 次，等一分钟；**裸 `404` 且 body 里没有 `invalid_code`** → 那台 hub 没有 pairing API（旧版本），升级它或改用 `--token`；连不上/超时 → 报出试过的 URL 与原因（`pair.rs:199-209`）；`200` 但没有 `connectorToken` → 那不是一个 pairing API（`pair.rs:177-184`）。**状态码不够、还要看 body 里的 `detail.code`**：两种 404 要给的忠告正好相反（`pair.rs:106-110`）。超时 15s（`pair.rs:16`）。
- 真二进制烟测：`crates/farwork-agent/tests/pair_smoke.rs`（对着桩 hub 跑 `pair`，断言 `agent.json` 的内容与权限、claim body、以及 connector 实际用来拨号的 token；`pair_smoke.rs:22-27`）。

### 3.13 `GET /meta`（免鉴权） —— `routes/meta.rs`

```json
{"hubUrl":"https://hub.example.com","version":"0.1.0","serverTime":"…","scheme":"https","port":443}
```

`hubUrl` 的优先级只有两级（`meta.rs:63-82`）：

1. `FARWORK_HUB_PUBLIC_URL`（启动时规范化，见 §2）——设了就**完全不看请求头**；`scheme`/`port` 从它身上解析，没写端口就按 scheme 默认（`https`→443、`http`→80，`meta.rs:84-101`）；
2. 否则：scheme 取 `X-Forwarded-Proto` 的**第一个值**（只认 `http`/`https`，其它忽略，`meta.rs:125-133`），主机名取 `Host` 头**去掉端口**（IPv6 字面量 `[::1]:5173` → `[::1]`，`meta.rs:103-123`），端口取 **hub 自己的监听端口** `config.port`（`meta.rs:74-81`），并且只在不是 scheme 默认端口时才写进 URL（`meta.rs:135-142`）。

两条推导都直接来自一次事故：

- **端口绝不取 `Host`**：反代下这个头带的是代理的地址，那个数字正是不能复制的——复制过去就是把「设备永远连不上」写成配置（`meta.rs:11-16`）。
- **`X-Forwarded-Proto` 只定 scheme**：它除非有可信代理重写，否则是调用方可控的；除了「这个 URL 长什么样」，它不许影响任何东西（尤其不能用来判断调用者是谁，`meta.rs:19-25`）。

**为什么需要这个端点（真实事故）**：生产把静态产物交给 hub 托管，页面 origin 就是 hub，一切正常；但 `vite dev`（5173 代理到 7300）和反代下不是。用 `location.origin` 拼出来的命令会让 `farwork-agent pair --hub http://localhost:5173` 被原样写进 `agent.json`，之后 agent 再也连不上，而唯一症状是「设备离线」（`meta.rs:1-9`）。所以 hub 自报基地址、web 照抄，而不是猜。

**`FARWORK_HUB_PUBLIC_URL` 非法 → hub 启动失败**（`config.rs:71-93`）：这个串会被逐字复制进每条配对命令，写错必须在启动时炸，而不是交给用户一个连不上的地址。规范化规则：trim、去尾斜杠、scheme 小写，只接受 `http`/`https` 且有非空 authority、不含空白；空值等于「没设置」（`config.rs:78-93`；单测 `config.rs:132-147`）。

未经 `AuthedUser`，和 `/healthz` 同级：它只说 hub 自己的地址与版本，不说任何账号信息（`meta.rs:24-25`）。测试 `crates/farwork-hub/tests/meta.rs`（**不碰数据库**：懒连接池）：无凭证可读、回 public url、忽略 `Host` 里的端口、只取 `X-Forwarded-Proto` 的 scheme、public url 的端口与路径保留（`meta.rs:50-119`）。

### 3.14 根上的两条路由（不在 `/api/v2` 下）：`GET /healthz` 与 `GET /install.sh`

`GET /healthz` 只回版本号：

```json
{"status":"ok","version":"0.1.0"}
```

源码：`crates/farwork-hub/src/lib.rs:38,50-52`。

`GET /install.sh` 把仓库根的 `install.sh` **原样**发出去（`include_str!` 编进二进制，handler 只加 `content-type: text/x-shellscript` 与 `content-disposition`，不改一个字节；`src/install.rs:1-40`）。**它必须在根上**：配对页把 `GET /meta` 的 `hubUrl` 拼成 `curl -fsSL <hubUrl>/install.sh | sh`（`apps/web/src/components/ConnectDevicePage.svelte:73`），而 hub 在这里回 `404` 就等于把页面上唯一那条安装指令变成用户 shell 里的一次失败。免鉴权，与 `/healthz` 同级：它是公开的安装物料，不含任何账号信息。测试 `crates/farwork-hub/tests/install_sh.rs`（与 `meta.rs` 一样**不碰数据库**，懒连接池）。

## 4. `501 not_implemented`：**35 条路径**

`crates/farwork-hub/src/routes/unsupported.rs:18-79` 用一个宏注册了 **35 条**路径（宏体 `unimplemented_paths!` 的 `"/..."` 字面量在 `unsupported.rs:19-76`，逐条数出正好 35），全部 `any(not_implemented)`，返回 `501` 且 body 里带 `"METHOD /path is not implemented in this build"`（`unsupported.rs:81-89`）；未注册的其余路径落到 `unknown_path` → `404`（`unsupported.rs:78,91-99`）。

**账要从 52 条倒着算**：M3 移出 3 条（`/sessions/{id}/runtime/interrupt`、`/sessions/{id}/runtime/notices`、`/sessions/{id}/runtime/notices/{notice_id}/respond`，§3.4/§3.7）→ 49；分享移出 2 条 → 47；管理台移出 2 条（`/admin`、`/admin/{*rest}`）→ 45；**会话命名空间与运行时控制再移出 8 条**（`/sessions/{id}/runtime/steer`、`/runtime/commands`、`/runtime/selections`、`/runtime/catalogs/{catalog}`、`/sessions/{id}/terminal`、`/sessions/{id}/terminal/{*rest}`、`/sessions/{id}/files`、`/sessions/{id}/files/{*rest}`）→ **37**。**配对码再移出 2 条**（`/pairing`、`/pairing/{*rest}`，已由 `routes/pairing.rs` 实现，§3.12）→ **35**。移出这 8 条不等于那组都补齐了：`/sessions/{id}/sync` 与 `/sessions/{id}/takeover` 仍在表里。`/connectors/{id}/pairing` **仍在表里**——那是每台设备的运行时开关，与配对码不是同一件事。

**分享又移出 2 条，49 → 47**：`/sessions/{id}/shares`（`POST`）与 `/sessions/{id}/shares/{*rest}`（其中 `DELETE .../{share_id}`）已由 `routes/shares.rs` 真实实现（§3.8），所以表里只剩账号级的 `/shares` 与 `/shares/{*rest}`。公开读 `GET /public/shares/{id}` **从来不在本表**——它的前缀是 `/public/...` 而不是 `/shares*`，是这一批新加的一条路径，不是从 501 里移出来的。

**管理台再移出 2 条，47 → 45**：`/admin` 与 `/admin/{*rest}` 已由 `routes/admin.rs` 真实实现（§3.9；3 次 `.route(...)`、5 个 method+path），所以这一整组从表里消失，宏里当时剩 45 条字面量。它移走的是**实例管理**这一块，不是 `/announcements*`、`/agents*`、`/pairing*`、`/usage*` 四个通配——管理台交付时那些仍在表里、仍回 `501`（其中 `/pairing*` 后来由 §3.12 那一批移出，见上）。

**文件与终端的会话命名空间是后补的，现在也移出去了**：第一批交付的是 connector 命名空间（`/connectors/{id}/fs/*`、`/connectors/{id}/terminals*`，§3.5/§3.6），当时 `/sessions/{id}/files*` 和 `/sessions/{id}/terminal*` 仍在表里、仍回 `501`；这一轮把这两组连同 `runtime/{steer,commands,selections,catalogs/{catalog}}` 一起真正实现了（会话级**共用同一条 `fsrelay`/`termrelay`**，不是第二套中继，见 §3.5/§3.6），于是表里少掉整整 8 条。

| 分组 | 路径（方法不限） | 条数 |
| --- | --- | --- |
| 会话同步与接管（**只剩这两条**：`runtime/{steer,commands,selections,catalogs/{catalog}}` 已实现，见 §3.10） | `/sessions/{id}/sync`、`/sessions/{id}/takeover` | 2 |
| 分享（**只剩账号级清单**：会话级 create/delete 与公开读已实现，见 §3.8） | `/shares`、`/shares/{*rest}` | 2 |
| 批量会话维护 | `/sessions/archive`、`/sessions/unarchive`、`/sessions/read` | 3 |
| 设备运行时管理 | `/connectors/{id}/runtimes`、`/connectors/{id}/runtimes/{*rest}`、`/connectors/{id}/protocol/capabilities`、`/connectors/{id}/pairing`、`/runtimes`、`/runtimes/{*rest}`、`/devices`、`/devices/{*rest}` | 8 |
| OAuth / 邮件码 / 移动端扫码登录 | `/auth/oauth/{start,callback,finalize}`、`/auth/mobile-login/{qr,exchange,refresh,request,status,confirm}`、`/auth/email-code`、`/auth/change-password`、`/auth/me/{email,profile,avatar}` | 14 |
| 公告 / agents / usage（**实例管理已实现**，`/admin*` 已从本表移出，见 §3.9；**配对码已实现**，`/pairing*` 已移出，见 §3.12） | `/announcements`、`/announcements/{*rest}`、`/agents`、`/agents/{*rest}`、`/usage`、`/usage/{*rest}` | 6 |

**2 + 2 + 3 + 8 + 14 + 6 = 35**，与宏里的字面量条数一致（`unsupported.rs:19-76`）。分组只是读起来方便：`any(...)` 是按**路径**匹配的，没有方法粒度，所以同一条 `*rest` 通配把它下面所有子路径一起收进 `501`。

另有一处**带内** `501`：`POST /sessions/{id}/runtime/messages` 携带非空 `attachments` 时返回 `501 attachments are not implemented in this build`（`sessions.rs:971-979`）——拒绝好过静默丢弃附件。

## 5. Connector Ingress（M3）：Ingress Protocol v1

一个端点、两个方向、一份契约。hub 半边是 `crates/farwork-hub/src/ingress.rs`（连接表 + 帧构造 + 入站解析）与 `crates/farwork-hub/src/routes/connector_ws.rs`（socket 循环）；agent 半边是 `crates/farwork-agent/src/protocol.rs`（`ClientFrame` / `ServerFrame`）与 `crates/farwork-agent/src/hub.rs`（连接循环）。设备终端另有中继半边 `crates/farwork-hub/src/routes/terminals.rs` + `crates/farwork-hub/src/termrelay.rs` 与 agent 的 PTY 宿主 `crates/farwork-agent/src/term.rs`，见 §5.11–§5.12；Ingress Protocol v1.3 的三帧（`session.capabilities` / `session.steer` / `session.selections`）见 §5.13。**两侧字段必须同改**（hub `ingress.rs` + agent `protocol.rs` + 本文），见 AGENTS.md。

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
| `session.capabilities` | `streamId`, `capabilities: {steer?, interrupt?, selections?, commands?}`, `catalogs: {models?, permissions?}`, `commands?` | Ingress Protocol v1.3 的能力上报：按 `streamId` 定位会话，整份写进进程内缓存（`runtime_caps`），`revision` 由 hub 自增；**不落库**（它描述的是一个活着的进程）。hub 侧三个子键都是 `#[serde(default)]`：缺键不断连、缺省即 `false` / 空表。四布尔与两份 catalog 的含义、清除时机见 §5.13 |
| `item.upsert` | `streamId`, `item: {id, type, status, role?, content, itemTime}` | 交给 timeline 的 `apply_runtime_item`：`{type,status,role,content}` 哈希去重，未变更不提交也不广播；未知 `type`/`status`/`role` 报错，**不强转** |
| `notice.upsert` | `streamId`, `notice: {id, kind, status?, title?, body?, options?, interactionType?}` | upsert 进 `session_notices`（同 id 可被细化 title/options/interactionType），广播 `runtime.notice.updated`。`options` 每项是 `{id,label,style?}`，旧的字符串数组**仍合法**并被归一化成对象（§3.7）；`interactionType` 落 `interaction_type` 列（`ingress.rs:838-849`；`notices.rs:145-171`） |
| `notice.resolved` | `noticeId`, `decision?`, `actionId?`（**无** `streamId`） | 按 `id` + 「该 connector 拥有的会话」定位，写 `status='resolved'` 与答案，广播 `runtime.notice.updated` 和整份 `runtime.notice.snapshot`。两个字段都可缺（别的客户端答的、或运行时撤回了提示）：`(None,None)` 记为「已关闭，无答案」而不是替谁编一个决定（`notices.rs:230-241`）；带 `actionId` 时校验它在这条 notice 给出的 options 里，否则 `422 unknown_action`（`notices.rs:218-228`） |
| `fs.response` | `requestId`, `ok`, `op?`, `path?`, `entries?`, `sizeBytes?`, `content?`, `truncated?`, `error?` | 交给 `FsRelay` 按 `requestId` 配给等待中的 REST 请求，**不走流路由**（`ConnectorFrame::stream_id()` 对它返回 `None`，`ingress.rs:562`）。三种形态（list 成功 / readText 成功 / 失败）见 §5.5 |
| `terminal.opened` | `terminalId` | 按 `terminalId` 路由（**不是** `streamId`）：`termrelay.mark_opened` 记住「这个终端已经起过」，并向已挂着的浏览器推 `{"type":"terminal.opened"}`；没人订阅时只记 debug（`connector_ws.rs:500-514`） |
| `terminal.data` | `terminalId`, `data`（standard padded base64） | 原样转发给该终端的浏览器 socket，hub **不解码不重编码**；没有订阅者就丢弃并记 debug（`connector_ws.rs:516-529`） |
| `terminal.exit` | `terminalId`, `code`（**键恒在，可为 `null`**） | 转发 `{type:"terminal.exit", code}` 后关通道并 `close_terminal` 落 `closed_at`（`connector_ws.rs:531-543`） |
| `terminal.error` | `terminalId`, `message`, `code` | 转发给浏览器后关通道，并 `fail_terminal` 把 `code`/`message` 落进 `terminals`：晚到的浏览器仍能看到拒绝原因（`connector_ws.rs:545-560`） |
| `error` | `message`, `streamId?` | 只记 warn（带 `streamId`），不改变会话状态、不断连 |

### 5.3 下行（hub → connector）

hub 用 `ingress.rs:308-466` 的**十三个**构造函数产出帧（终端四个 `terminal_*_frame` 在 `ingress.rs:420-465`；v1.3 新增 `session_steer_frame` / `session_selections_frame`，`ingress.rs:376-404`）；agent 用 `protocol.rs:333-422` 的 `ServerFrame` 解析（解码入口 `parse_server_frame`，`protocol.rs:448`；未知 `type` 与畸形 payload 的区分在 `protocol.rs:432-474`）。

| `type` | wire 字段 | agent 侧行为 | hub 何时发 |
| --- | --- | --- | --- |
| `welcome` | `serverVersion`, `heartbeatSecs` | 握手完成；心跳间隔取 `max(heartbeatSecs,1)` | 握手成功后 |
| `session.start` | `streamId`, `sessionId`, `runtime`, `cwd`, `prompt` | 校验 `runtime` 与本进程 `--runtime` 一致（不一致 → `error` + `session.exit{code:2}`，绝不冒充别的运行时）；一致则 spawn `codex app-server --stdio` 并回 `session.started` | `POST /sessions/create-and-start`（`sessions.rs:621-647`） |
| `session.message` | `streamId`, `text`, `clientMessageId`（**可选**：hub 无值时不发该键；agent 必须能解析缺失与 `null`） | 追加到该流的 codex 线程（`turn/start`），`clientMessageId` 缺失时本地生成 `cmid_<uuid>` 当幂等键；无该流 → `error` | `POST /sessions/{id}/runtime/messages`，且仅当该会话有活跃流 |
| `session.interrupt` | `streamId` | 映射成 codex `turn/interrupt`；`turn/completed{status:"interrupted"}` 把在跑条目收成 `interrupted` | `POST /sessions/{id}/runtime/interrupt` |
| `session.steer` | `streamId`, `text` | 给正在跑的回合再补一条指令。`text` 缺失或 `trim()` 后为空 → **丢弃并 warn，不断连**（`hub.rs:357-373`）；echo 后端把这条指令写成一条 user 条目 + 一条 assistant 回执（`echo.rs:179-186`），codex 没有 steer 方法，回 `error` 明说没应用（`codex/session.rs:110-121`） | `POST /sessions/{id}/runtime/steer`（`runtime.rs:188`） |
| `session.selections` | `streamId`, `model?`, `permissionMode?` | 改模型 / 权限模式。**缺省不发键**（不是发 `null`，`ingress.rs:387-404`）：agent 把缺失读成「保持当前值」，两个键都没有时丢帧并 warn（`hub.rs:374-391`）。echo 记下并回同状态（`echo.rs:187-202`）；codex 无奈拒绝并如实说明（`codex/session.rs:122-139`） | `PATCH /sessions/{id}/runtime/selections`（`runtime.rs:273`） |
| `notice.respond` | `noticeId`, `decision?`, `actionId?`, `input?`（**无** `streamId`，与上行同形；缺键不发键） | 在所有 live worker 里按 noticeId 找提示并回 JSON-RPC result（带 `actionId` 时先按该 notice 记录过的 action 列表**重新校验**，不合法回 error 帧并把提示放回队列：`interaction.rs:95-126`；`mod.rs:683-706`）；找不到只 warn | `POST .../notices/{id}/respond` |
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
- `runtime/capabilities` **已有真实来源**：`session.capabilities`（v1.3，§5.13）。但「有帧」不等于「有值」——codex 后端如实上报 `steer/selections/commands = false` 与两份空 catalog（`codex/mod.rs:20-38`），所以对着真 codex 看到的仍然是「只有 interrupt 可用」。
- **takeover 仍然没有 ingress 帧**（`/sessions/{id}/takeover` 在 `501` 表里，§4）；文件仍只有**只读**的 `fs.request` / `fs.response`（§5.5），写操作没有帧。
- 设备终端**已有**真实帧（§5.11–§5.12）；两个命名空间（`/connectors/{id}/terminals*` 与 `/sessions/{id}/terminal*`）都在这一轮之后可用，共用同一条 `termrelay`（§3.6）。
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

### 5.13 Ingress Protocol v1.3：`session.capabilities` / `session.steer` / `session.selections`

v1.3 只加了三帧，方向不对称：**能力只能由运行时上报**（上行），**干预只能由 hub 下发**（下行）。三处已同改：`crates/farwork-hub/src/ingress.rs`（解析 + 构造）、`crates/farwork-agent/src/protocol.rs`（`ClientFrame` / `ServerFrame`）、本节。

**上行 connector → hub（C→S）**

```json
{"type":"session.capabilities","streamId":"str_1",
 "capabilities":{"steer":true,"interrupt":true,"selections":true,"commands":true},
 "catalogs":{"models":[{"id":"echo-fast","name":"Echo fast","description":"…"}],
             "permissions":[{"id":"default","name":"Default"}]},
 "commands":[{"name":"/help","description":"…"}]}
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `streamId` | 是 | hub 用它反查会话（`connector_ws.rs:415-421`）；查不到只记日志，不断连 |
| `capabilities.{steer,interrupt,selections,commands}` | 否 | **四个子键全部 `#[serde(default)]`**：缺键 → `false`，绝不缺省成 `true`（hub `runtime_caps.rs:16-28`，agent `protocol.rs:242-257`） |
| `catalogs.{models,permissions}` | 否（缺省空数组） | 每项 `{id, name, description?}`。`id` 是选择补丁真正发送的值，`name` 只用于显示——所以 `id` 必填、`name`/`description` 可省（`runtime_caps.rs:41-67`） |
| `commands` | 否（缺省空数组） | 每项 `{name, description?}`，`name` 必填（`runtime_caps.rs:77-98`） |

**所有子键的语义是同一条**：缺键 = 不支持 / 没有。这是本节唯一的安全默认——一个编出来的能力会让浏览器给出点了没用的控件。

**下行 hub → connector（S→C）**

```json
{"type":"session.steer","streamId":"str_1","text":"also run the tests"}
{"type":"session.selections","streamId":"str_1","model":"m1"}
{"type":"session.selections","streamId":"str_1","permissionMode":"read-only"}
```

| 帧 | 字段 | 缺省语义 | agent 行为 |
| --- | --- | --- | --- |
| `session.steer` | `streamId`（必填）, `text` | **缺 `text` 或空白 → 丢弃并 warn，socket 保持**（`hub.rs:357-373`）：空指令投给运行时会变成 timeline 上一条无法解释的条目 | echo 写成一条 user 条目 + 一条 assistant 回执（`echo.rs:179-186`）；codex 回 `error`「this runtime cannot steer a running turn」（`codex/session.rs:110-121`） |
| `session.selections` | `streamId`（必填）, `model?`, `permissionMode?` | **缺省不发键**（`ingress.rs:387-404`）：只带 `model` 就表达「只改模型」，agent 把缺键读成「保持当前值」。两个键都没给 → 丢帧并 warn（`hub.rs:374-391`） | echo 记下并把同一份 state 再发一次（`echo.rs:187-202`）；codex 回 `error`「cannot change model or permission mode at run time」（`codex/session.rs:122-139`） |

**hub 怎么存（这一段决定了 REST 能说什么）**

- **按 session 缓存，进程内、不落库**（`runtime_caps.rs:145-205`）：缓存说明的是一个**活着的进程**，写进 `sessions` 表等于对一条可能已经不存在的进程作断言。
- **`revision` 由 hub 自增**：每接受一帧就 +1，首帧是 `1`（`runtime_caps.rs:163-187`）。运行时并不发版本号，所以这个数字是**缓存自己的**版本。
- **清除时机只有两个**：`session.exit` 收流时 `clear(session)`（`connector_ws.rs:379-388`）；connector socket 结束时 `clear_all(...)`（`connector_ws.rs:196-207`）。于是「能力被收回」与「从未上报」给读者的答案一致。
- **从没收到过帧 → 四个布尔全 `false`**、`revision: 0`、`reportedAt: null`（`runtime.rs:45-62`；单测 `runtime_caps.rs:230-244`）。**不是全 `true`**：那会给用户四个不工作的按钮。
- **写接口对「还没上报」故意乐观**：只有在**上报过** `steer:false` / `selections:false` 时才回 `501`；缓存为空时照常下发（`runtime.rs:171-182,259-267`）——把慢到的第一帧当成「没有这个功能」，会让首帧延迟看起来像缺功能。

**两个后端的真实上报（别把 echo 的能力当成产品能力）**：echo 四项全 `true` 并带占位 catalog，模型 id 就叫 `echo-fast` / `echo-slow`，描述明写「echo placeholder model: it echoes, it does not think」（`echo.rs:25-68`）；codex 只有 `interrupt: true`，其余三项 `false`、两份 catalog 为空（`codex/mod.rs:20-38`）。**这就是「运行中干预」今天的真实覆盖面**：协议与管道通了，真 codex 只支持中断。DSH 是第三个后端：它的实测能力值、`session.commands` 报 `false` 的理由见 §10.5。

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

- 迁移文件 **7 个**：`crates/farwork-hub/migrations/0001_init.sql` + `0002_connector_ingress.sql`（M3 ingress）+ `0003_terminals.sql`（设备终端）+ `0004_shares.sql`（分享）+ `0005_session_terminal_scope.sql`（Ingress Protocol v1.3：给 `terminals` 加可空的 `session_id`，不建第二张表）+ `0006_notice_interaction_type.sql`（给 `session_notices` 加**可空**的 `interaction_type`：DSH 的 approval 是通用 runtime interaction，不是 `kind` 那三个值能表达的，所以加列而不是改义 `kind`；**不建表、不加索引**）+ `0007_pairings.sql`（配对码：`pairings` 表 + 两个索引；只存带密钥的摘要，`claimed_at` 是一次性开关，见 §3.12）。启动时由 `db::migrate` 通过 `sqlx::migrate!("./migrations")` 应用，sqlx 自带 ledger，重复启动是 no-op（`db.rs:17-22`；`main.rs` 的启动序列）。
- **13 张表**（`create table` 计数）：0001 的 `users`、`app_settings`、`connectors`、`projects`、`sessions`、`session_runtime_states`、`timeline_items`、`ws_tickets`，加 0002 的 `session_streams`、`session_notices`，加 0003 的 `terminals`，加 0004 的 `shares`（`snapshot_json` 存一次写死的快照、`deleted_at` 是软删，见 §3.8），加 0007 的 `pairings`（只存 `code_hash`、`expires_at_epoch` 是整数比较、`claimed_at` 与 `connector_id` 记「被谁花掉」，`connector_id` 可空所以删设备不会抹掉「这个码被用过」，`0007_pairings.sql:19-30`）。`0006_notice_interaction_type.sql` 只加列，**表数不变**。后三张的存在理由见 §5.7：一张让 `streamId` 跨 hub 重启存活，一张让审批跨刷新存活，`terminals` 让「终端已结束 / 从未存在」可区分并让 agent 的拒绝原因活过它被报告的那一刻（列含 `closed_at`、`error_code`、`error_message`，`0003_terminals.sql:13-29`）。
- **18 个索引**（`create index` / `create unique index` 计数：0001 十个 + 0002 两个 + 0003 两个 + 0004 一个 + 0005 一个 + 0007 两个；`0006` 一个也没加，**索引数不变**），其中数条是语义承载点：
  - `uq_timeline_items_client_message`：`unique (session_id, client_message_id)`（`0001_init.sql:132`）——`clientMessageId` 幂等的**唯一**保证。
  - `idx_timeline_items_session_updated_seq` / `_session_order_seq`：游标补拉与展示排序的查询路径（`0001_init.sql:129-130`）。
  - `idx_session_notices_id`：`notice.resolved` 只带 notice id，解析时先按 id 找行、再校验它属于发帧的 connector（`0002_connector_ingress.sql:37-39`）。
  - `uq_pairings_code_hash`：**唯一**索引（`0007_pairings.sql:35`）——claim 只按摘要查（匿名调用者说不出这个码属于谁），唯一性让摘要撞车时换码重试而不是让一个活着的码被悄悄遮蔽（`routes/pairing.rs:161-188`）；`idx_pairings_user` 服务「我的配对码」列表（`0007_pairings.sql:38`）。
  - `idx_terminals_connector`：connector 断连时扫「这个设备还有哪些终端没关」（`0003_terminals.sql:31-32`；`terminals.rs:594-614`）；`idx_terminals_user` 服务取票时的归属校验（`0003_terminals.sql:34-35`；`ws.rs:154-162`）；`idx_terminals_session` 服务会话命名空间的读（`0005_session_terminal_scope.sql:13-14`），也是握手时判定「同设备但别处开的终端」的查询路径（`terminals.rs:353-363`）。
- `ws_tickets` 走 `delete ... returning` 消费，行即删，靠 `idx_ws_tickets_expires` 清理（`0001_init.sql:144`；`ws.rs:83-108`）。

## 8. 测试的前置条件（`crates/farwork-hub/tests/`）

`crates/farwork-hub/tests/` 下**十个**集成测试，其中 `meta.rs` 与 `install_sh.rs` 不碰数据库（懒连接池），其余都跑真实 PostgreSQL：`e2e.rs`（M2 纵向切片）、`connector_ingress.rs`（ingress 上下行）、`fs_bridge.rs`（文件浏览中继）、`terminal_bridge.rs`（设备终端中继）、`shares.rs`（分享快照与公开读）、`admin.rs`（管理台：权限门、七键投影、admin_lockout、settings 开关真的生效）、`session_runtime.rs`（运行中干预与会话命名空间，单个测试函数——同一个库里多个测试会互相 truncate）、`pairing.rs`（配对码：免鉴权 claim、一次性、410/404/429、明文码与 token 都不落库；测试内按进程互斥串行）、`meta.rs`（`/meta` 的地址推导，**刻意不碰数据库**：懒连接池，只从配置与请求头回答）、`install_sh.rs`（`GET /install.sh` 与仓库 `install.sh` 逐字节相同、内容类型正确；同样**不碰数据库**）。下面以 `e2e.rs` 为例：

- `TEST_DATABASE_URL` **未设置** → 打印 `SKIP farwork-hub integration test: TEST_DATABASE_URL is not set.` 并附上怎么设置，然后 `return`（`e2e.rs:127-134`）。测试仍然是「通过」的——这就是 CI 必须提供数据库的原因。
- `TEST_DATABASE_URL` 为空串 → 打印 `SKIP farwork-hub integration test: TEST_DATABASE_URL is empty.` 并返回（`e2e.rs:135-138`）。
- 有值 → `bootstrap()` 连库、`db::migrate()` 建表、再 `truncate` 清空，然后跑「登录 → connectors/项目/会话 → timeline → 发消息 → WS 增量」整条链路（`e2e.rs:62-72`）。**目标库会被清空，只能指向一次性库。**

另一类测试在 agent 侧、**不碰数据库**：`crates/farwork-agent/tests/dsh_live.rs` 是对**本机真 DSH bridge** 的只读探测（`initialize` + `session.list` + 两份 catalog，**不建会话、不跑一轮**），因此用 `#[ignore]` 关掉（`dsh_live.rs:21,103-104`）。跑法：`cargo test -p farwork-agent --test dsh_live -- --ignored --nocapture`（`dsh_live.rs:10-11`）。没有桥时它打印 `PROBE SKIP: no DSH bridge endpoint (…)` 然后**正常返回**（`dsh_live.rs:25-31`）——「本机没有 DSH」不算测试失败，但也不算验过。CI 不跑它。

## 9. 与上游的形态差异（本仓不是 byte-compatible）

本仓与上游**共享产品形态、部分路径名与部分字段名**，但 `/api/v2` 的响应形状**不是上游的镜像**。下面逐项对照这一轮新增的五个面。上游源码是**本机只读对照**（`/tmp/anywhere-survey`，不随仓分发、不是构建输入，见 AGENTS.md 4.2），按仓库约定引用它只为核对 wire 字段。

| # | 面 | 上游 | 本仓 |
| --- | --- | --- | --- |
| 1 | `GET /runtime/capabilities` | `{connectorId, capabilitySet:{revision, capabilities:[协议 1.0 capability 对象…]}, serverTime}`，**现问 connector** | `{connectorId, capabilitySet:{steer,interrupt,selections,commands}, revision, reportedAt, serverTime}`，**读缓存**（§9.1） |
| 2 | `GET /runtime/catalogs/{catalog}` | 两条固定路径 `/catalogs/model`、`/catalogs/permission`，回 `{catalog:{runtime, revision, models: […] 或 permissions: […]}}`，**现问 connector** | 一条带参路径，回 `{catalog: "model" 或 "permission", items:[{id,name,description?}], revision}`，**读缓存**（§9.2） |
| 3 | `GET /runtime/commands` | `{commands:[RuntimeCommandView{id,title,description,aliases,category,scope,enabled,acceptsArgs,…}]}`，支持 `?query=&limit=` | `{commands:[{name,description?}]}`，无过滤、无执行端点（§9.3） |
| 4 | `POST /runtime/steer`、`PATCH /runtime/selections` | steer 收 `{content,attachments[],clientMessageId}` 回 `200 RpcResponsePayload`；selections 收 `{selections:{k:v}}` 回 `{ok,state,connectorResult}` | steer 收 `{content}` 回 `202 {delivered,streamId}`；selections 收 `{model?,permissionMode?}` 回 `{selections}`（§9.4） |
| 5 | 会话级文件与终端 | **没有** `/sessions/{id}/files/{list,readText}`；终端是复数 `/sessions/{id}/terminals`（create / list / patch / delete / resize / stream） | 有 `/sessions/{id}/files/{list,readText}`；终端是单数 `/sessions/{id}/terminal`（只 create + stream）（§9.5） |

### 9.1 `/runtime/capabilities`：capability 列表 vs 四个布尔

- **上游**（`server/agent_server/api/sessions.py:697-721`）：回 `{connectorId, capabilitySet, serverTime}`，`capabilitySet` 是 `{revision, capabilities:[ProtocolCapability…]}`（`server/agent_server/core/protocol.py:116-137`），每项至少 `{capabilityId, version, scope, runtime?, sessionId?, supported, available, allowed, unavailableReason?, parameters}`；id 词表是 `session.send_message` / `session.interrupt` / `session.steer` / `session.commands` / `session.interaction.approval` / `runtime.config` / `catalog.model` / `catalog.permission` / `catalog.effort`（`server/agent_server/core/capabilities.py:7-16`）。值是**现问 connector** 得来的（`read_session_capabilities_from_connector` + `derive_session_effective_capabilities`，`sessions.py:708-716`），客户端按 id 查表（`core/capabilities.py:19-40`）。
- **本仓**（`crates/farwork-hub/src/routes/runtime.rs:45-62`）：`capabilitySet` 是**四个布尔** `{steer, interrupt, selections, commands}`，来源是缓存的 `session.capabilities` 帧（§5.13），不现问设备。没有 `supported/available/allowed` 三态，没有 `parameters`，没有 `capabilityId`；`revision` 是 **hub 缓存版本**，`reportedAt` 是本仓独有字段。
- **不可平移的点**：按 capabilityId 查表的上游客户端会一条也查不到（本仓根本没有那个数组）。这是**有意的**：本仓只报「有没有这项能力」，不假装能表达「支持但当前不可用 / 支持但被策略禁止 / 需要参数」。

### 9.2 `/runtime/catalogs/{catalog}`：现查对象 vs 缓存条目

- **上游**：两条**固定路径** `/runtime/catalogs/model`（`sessions.py:724-755`）与 `/runtime/catalogs/permission`（`sessions.py:758-789`）；响应是 `{catalog, serverTime}`，模型目录为 `{runtime, revision, models:[{displayName, id, selectionId, default, description?, reasoningItems[], group?, metadata}]}`（`core/protocol.py:140-181`），权限目录为 `{runtime, revision, permissions:[…]}`（`core/protocol.py:183-200`）。取法是**现问 connector**（RPC `runtime.modelCatalog` / `runtime.permissionCatalog`，`limit=200`），之前还会先 `device_runtimes.ensure_active_running(...)`——即上游为了一个列表**可能去拉起运行时进程**。
- **本仓**（`routes/runtime.rs:25-28,66-98`）：一条带参路径 `/runtime/catalogs/{catalog}`，`catalog` 是闭集 `model|permission`，**未知值 `422 unknown_catalog`**（不是空列表，`runtime.rs:72-79`,`runtime_caps.rs:107-116`）；响应 `{catalog:"model"|"permission", items:[{id, name, description?}], revision, serverTime}`——`catalog` 是**字符串**，条目没有 `displayName`/`selectionId`/`default`/`reasoningItems`，也没有 `runtime`。**读缓存，不现查**（`runtime.rs:80-91`），不会为取列表拉起任何进程。
- **不可平移的点**：上游选择器读 `catalog.models[].selectionId` 与 `default`，本仓没有 `selectionId`（只能拿 `id` 当选择值），也没有可靠的「默认项」——硬编一个 `default:true` 就是编造。

### 9.3 `/runtime/commands`：视图对象 + 过滤 vs `{name, description}`

- **上游**（`sessions.py:1242-1295`）：每项 `RuntimeCommandView{id, title, description, aliases[], category, scope, enabled, disabledReason?, acceptsArgs, argsSchema?, metadata}`（`core/models.py:1191-1202`）；支持 `?query=`（≤4096）与 `?limit=`（默认 100，夹在 1..=1000）；能力不可用时**直接 `409`**（`_require_session_action_capability`，`sessions.py:1221-1239`）；条目来自 connector 的 `session.commands` RPC。另有 `POST /runtime/commands` 执行一条命令（`sessions.py:1298-…`）。
- **本仓**（`routes/runtime.rs:100-121`）：`{commands:[{name, description?}], serverTime}`。**没有** id/title/aliases/category/enabled/disabledReason/acceptsArgs/argsSchema，**没有** `?query=`/`?limit=`（模糊匹配在浏览器本地做，`apps/web/src/lib/commands.ts:13-25`），**没有**执行端点——命令执行仍走 `POST /runtime/messages` 由运行时解释。能力不可用时**不报 `409`**，回空清单（`runtime.rs:106-116`）。

### 9.4 `steer` 与 `selections`：请求体、状态码与响应形状全都不同

- **steer 上游**（`sessions.py:1450-1486`）：body `{content, attachments[]（≤10）, clientMessageId?}`（`core/models.py:1225-1230`，`extra="forbid"`）；成功 **`200`** 回 `RpcResponsePayload`——把连接器 JSON-RPC 的结果原样带回给调用者。
- **steer 本仓**（`routes/runtime.rs:125-203`）：body **只有** `{content}`（多传 `attachments` 会被忽略而不是拒绝）；成功 **`202`** 回 `{delivered:true, streamId, serverTime}`。`202` 的语义是「已投递给设备」，执行结果由 agent 写进 timeline，**不由 HTTP 回答**（§3.10、§5.13）。
- **selections 上游**（`sessions.py:792-…`）：body `{selections: {k: v|null}}`（map，1..=32 项，`extra="forbid"`，`core/models.py:924-928`），回 `{ok, state, connectorResult, serverTime}`（`core/models.py:930-934`）——键是**任意的**，由目录的 `selectionId` 决定。
- **selections 本仓**（`routes/runtime.rs:207-296`）：body `{model?, permissionMode?}`（**固定两个键**，至少给一个，空串按「没给」处理），回 `{selections, serverTime}`；只有帧进了 connector socket 才 `200`（`runtime.rs:273-295`）。没有 `ok`（成功就是 200）、没有 `connectorResult`、也没有 `state`（要看 `GET /runtime/state`）。

### 9.5 会话级文件与终端：本仓多一组、少一半

- **上游没有会话级文件接口**：`server/agent_server/api/` 下 grep 不到 `/sessions/{id}/files/list` 或 `/files/readText`；上游的文件入口是 connector 级的 `ConnectorFsListRequest{root, path?}`（`core/models.py:1239-1241`）与上传/附件体系（`UploadedAttachment` 等）。**本仓**多了 `POST /sessions/{id}/files/list|readText`（§3.5），相对路径锚在会话 `cwd` 上，别人的会话 `404`。这两条是**本仓独有**，不是上游的替代拼法。
- **上游终端是复数**（`server/agent_server/api/sessions_terminal.py`）：`POST /sessions/{id}/terminals`（`:47`）、`GET …/terminals`（列表，`:60`）、`PATCH …/terminals/{terminalId}`（`:72`）、`DELETE …/terminals/{terminalId}`（`:86`）、`POST …/terminals/{terminalId}/resize`（`:99`）、`WS …/terminals/{terminalId}/stream`（`:116`）。
- **本仓终端是单数**：`POST /sessions/{id}/terminal`（create）与 `GET /sessions/{id}/terminal/{terminalId}/stream`（§3.6）。**没有** list / patch / delete / resize 端点：resize 走 WS 帧 `terminal.resize`（§5.12），关闭走 WS 帧 `terminal.close`，会话归属写在 `terminals.session_id`（`migrations/0005_session_terminal_scope.sql:10-11`）——所以「列本会话的终端」在数据层已经可行，只是没暴露成端点。

### 9.6 结论，以及将来需要哪一层 alias

**用户可见行为对齐，但 REST 形状不是上游的镜像。** 上面五处都不是「字段名拼错」，而是**形状不同**：capability 是布尔集合而不是对象数组，catalog 是缓存条目而不是现查对象，command 只有名字，steer/selections 的状态码与请求体都换了，终端是单数、文件多一组。

若将来要用**上游 web 客户端或第三方客户端直连**，需要在 hub 加**一层 alias**（只做形状转换，不复制业务逻辑；alias 里也不许编造数据）：

1. `GET /sessions/{id}/runtime/capabilities`：把四个布尔翻成 `capabilitySet.capabilities:[…]`，id 用上游词表——`steer→session.steer`、`interrupt→session.interrupt`、`commands→session.commands`、`selections→runtime.config`（并视 catalog 是否有内容补 `catalog.model` / `catalog.permission`）；每项 `supported/available/allowed` 取同一个布尔，`scope:"session"`、`version:"1"`、`parameters:{}`。`revision` 可直接透传（它是缓存版本，语义足够）。
2. `GET /sessions/{id}/runtime/catalogs/{catalog}`：包成 `{catalog:{runtime, revision, models|permissions:[…]}}`，条目 `{displayName: name, id, selectionId: id, description}`；**`default` 必须回 `false`**——本仓没有可靠的默认项信息，编一个会让上游客户端默认选中一个用户没选过的模型。
3. `GET /sessions/{id}/runtime/commands`：把 `{name, description}` 抬成 `RuntimeCommandView`（`id/title = name`、`aliases:[]`、`scope:"session"`、`enabled:true`、`acceptsArgs:false`），并接受 `?query=` / `?limit=`（过滤可在 hub 侧做，或忽略 `query` 交给客户端）。
4. `POST /sessions/{id}/runtime/steer`：接受 `{content, attachments, clientMessageId}`（`attachments` 非空直接 `501`，不要静默丢），回 `200` 的 `RpcResponsePayload` 形状（`{ok, code?, message?, result:{streamId}}`）。
5. `PATCH /sessions/{id}/runtime/selections`：把 `{selections:{k:v}}` 映射到 `{model, permissionMode}`，其余键 `422`；回 `{ok, state, connectorResult}`（`state` 取 `GET /runtime/state`）。
6. 终端：加复数 alias —— `POST /sessions/{id}/terminals`（等价 create）、`GET /sessions/{id}/terminals`（按 `terminals.session_id` 列）、`PATCH` / `DELETE`（只能改/关本地行；`DELETE` 等价 WS 的 `terminal.close`）、`POST …/{terminalId}/resize`（映射成 WS 帧 `terminal.resize`，做不到就 `501`）。
7. **反方向不需要 alias**：`/sessions/{id}/files/list|readText` 与单数 `/sessions/{id}/terminal*` 是本仓独有，上游客户端不会调用。别把它们写进「上游兼容」清单。

## 10. agent 的第二个 runtime：DSH（`--runtime dsh`）

`--runtime` 有三个值：`codex`（默认，agent 拉起子进程）、`echo`（假后端）、`dsh`（连一个**已经在跑**的 DSH bridge；`cli.rs:18,26,34`，`--help` 的说明在 `cli.rs:112-117`）。三者共用同一套 ingress，hub 侧看不出区别；区别全在 agent 的 `crates/farwork-agent/src/dsh/`（九个文件：`mod.rs` 主循环、`client.rs` 方法封装、`rpc.rs` 分帧与 id、`discovery.rs` 端点、`handshake.rs` 握手与能力位、`sync.rs` 批次/快照/checkpoint、`projection.rs` 帧投影、`interaction.rs` 待答队列、`selections.rs` 目录短名解析）。

### 10.1 生命周期与 codex 相反

| | codex | DSH |
| --- | --- | --- |
| 谁起谁 | agent `spawn` 子进程 `codex app-server --stdio`（`hub.rs:341`、`hub.rs:634` 起会话 worker） | bridge **本来就在跑**；agent 只是 TCP 客户端。每个会话各连一次：`rpc::dial`（`rpc.rs:206`）在 `mod.rs:132` |
| 进程从哪来 | `$FARWORK_CODEX_BIN`（默认 `codex`） | 用户自己启动 DSH，并在其中**启用 agents-anywhere 桥插件** |
| 端点 | 无此概念 | 读 `endpoint.json`（见下） |
| 缺一个会怎样 | 该会话 `session.exit{code:2}` | 启动阶段就失败：退出码 `1`，connector **根本不上线**（`main.rs:97-99`） |

端点发现（`discovery.rs`）按优先级：① `--dsh-endpoint <path>`（`cli.rs:49-51,150`）；② `$HOME/.farwork/dsh-bridge/endpoint.json`（`FARWORK_SUFFIX`，`discovery.rs:26`）——**自研插件**，见 §11；③ `$HOME/.agents-anywhere/dsh-bridge/endpoint.json`（`UPSTREAM_SUFFIX`，`discovery.rs:28`）——上游插件；④ `$DSH_HOME/agents-anywhere/bridge/endpoint.json`（`DSH_HOME_SUFFIX`，`discovery.rs:30`），`$DSH_HOME` 未设置时回退 `$HOME/.dsh`（候选顺序 `discovery.rs:173-191`，入口 `resolve` 在 `discovery.rs:194`）。解析结果连**来源**一起返回（`Located{endpoint, path, source}`，`discovery.rs:91-96`；`source` 是 `explicit`/`farwork`/`upstream`/`upstream-dsh-home`，`discovery.rs:69-88`），启动日志记 `dsh_endpoint_source`（`main.rs:89-105,163`）——两个插件可以同时装，所以「连上了某个桥」不算信息。**显式 `--dsh-endpoint` 指了文件却不存在是错误，不回退去搜默认位置**：操作者点了名，让另一个桥顶上会把拼写错误藏起来（`discovery.rs:108-121`）；默认位置全空时报错会列出**四个候选**与怎么修（`discovery.rs:125-146`）。

读到的 JSON 必须逐字过四项校验（`parse`，`discovery.rs:252-297`；`pid` 只是记录字段，不参与校验），任何一项不过都是**拒绝拨号**而不是猜一个：
- `version == 1`（`ENDPOINT_VERSION`，`discovery.rs:21,144-152`）——版本变了说明文件布局变了，按旧布局读等于「静默拨错端口」；
- `host == "127.0.0.1"`（`discovery.rs:22,157-163`）——只连 loopback，不把 token 发到别的地址；
- `port` 在 `1..=65535`、`token` 非空白。

失败时的错误文本会列出**试过的路径**并提示「start DSH with the agents-anywhere bridge enabled」（`discovery.rs:50-56`）。**为什么把发现放在启动阶段**：`main.rs:67-72` 的注释就是理由——先连上 hub 再发现没有 DSH，hub 会照常接受 `session.start`，然后这个会话什么也驱动不了。缺端点 = **启动失败或会话失败**，没有「假装可用」这一档。

### 10.2 传输与分帧

- **换行分隔的 JSON-RPC 2.0 over TCP**：一帧一行、以 `\n` 结束；`\r\n` 也接受（行尾 `\r` 先剥掉，`rpc.rs:67-77`，单测 `rpc.rs:405`）。
- **单帧硬上限 8 MiB**（`MAX_FRAME_BYTES`，`rpc.rs:23`）：超限直接报错（`rpc.rs:36-42`），内部缓冲也按同一上限截断（`rpc.rs:79-88`）——不为「读完这一帧」把内存吃光。
- **非 UTF-8 拒绝**：`String::from_utf8` 失败即报错，不做有损解码（`rpc.rs:82-87`；单测 `rpc.rs:447-455`）。
- **请求 id 是 `aa-<n>`**，从 `aa-1` 单调递增（`IdGen`，`rpc.rs:96-114`）：字符串前缀让「这条 id 是本 agent 发的」一眼可辨，也不会跟对端自己的数字 id 相撞。
- **超时发 `$/cancelRequest`**（`CANCEL_METHOD`，`rpc.rs:26`；单测 `rpc.rs:587-616`）：超时后光放弃而不告诉对端，对端会继续算一条已经没人要的结果——所以取消必须发出去。
- **对端发来的请求一律回 `-32601`**（`METHOD_NOT_FOUND`，`rpc.rs:27,280`）：本 agent 不是 bridge 的服务端，只实现自己发起的那些方法；一个明确的「方法不存在」比沉默或断连更容易诊断。

### 10.3 握手：能力位只能来自上报

`initialize` 带 `authToken` / `connectorId` / `sessionNamespace` / `clientVersion`（`InitializeParams`，`handshake.rs:81-98`；调用在 `handshake.rs:129` 起）。回包两件事被硬校验（`handshake.rs:148-158`）：`identity.runtime == "dsh"`（`RUNTIME`，`handshake.rs:13`；不符是 `WrongRuntime`——连上的不是 DSH 桥），以及 `identity.protocolVersion` 的 **major 段**与 `1.0` 相同（`PROTOCOL_VERSION`，`handshake.rs:12,167-169`）。minor 不卡：major 变了才意味着另一套协议。

`features` 的**每一个字段都是 `#[serde(default)]`**（`handshake.rs:32-55`），因此**缺键 = `false`**，这是唯一的默认值：`approval` / `userQuestions` / `readOnly` / `sessionDiscovery` / `timelineSuffixRead` / `snapshotPagination` / `syncMode` / `projectionVersion`。

**门禁只认上报值**，不允许「看起来应该有」：
- 审批通知只在 `features.approval && !features.readOnly` 时转发（`approval_available`，`handshake.rs:71-73`；丢弃点 `projection.rs:180-187`）——只读姿态下桥已经说了「不会执行」，这时给一个点了没用的批准按钮就是骗人。
- 问题通知同理看 `userQuestions && !readOnly`（`handshake.rs:75-77`；`projection.rs:188-195`）。
- `syncMode != "events"` → 该会话直接 `code 2` 失败，不退化成轮询（`mod.rs:183-192`）；轮询会让 timeline 静默变旧。
- `projectionVersion` 默认 `3`（`DEFAULT_PROJECTION_VERSION`，`handshake.rs:17`），`2` 仍接受。

### 10.4 同步语义：subscribe → batch → **ack**，序号严格 +1

1. `runtime.sync.subscribe`：参数是 `{subscriptionId, checkpointVersion:1, uploadProgressVersion:1}`（`client.rs:218-233`；调用 `client.rs:397-414`）。`subscriptionId` 每次订阅都重新生成，桥按它绑定批次/ack/checkpoint（`client.rs:235-241`）——重订阅必须看起来像一次**新**订阅。**`checkpointVersion` 不写 1 等于关掉 durable checkpoint**：桥从此一个 `checkpoint.*` 操作都不发，每次连接都是全量 inventory 重放（`client.rs:218-222` 的注释即此）；
2. bridge 以 `runtime.sync.batch` 通知推事件（分发 `mod.rs:290-303`）；
3. agent 把批次翻成 ingress 帧后**必须**回 `runtime.sync.ack`（`client.rs:416-427`，调用点 `mod.rs:872-890`）。**ack 同时是 checkpoint 的答复通道**：批次里的 `checkpoint.load` 把答案写进待发的 `ack_checkpoint`，没有也**带上 `null`** 这个键（省略会被读成「这个扩展没实现」，`mod.rs:542-558`）；`checkpoint.save` 在 ack 发出**之前**落盘（`mod.rs:559-568`）——先回执后写盘，重连就退化成全量重放。实测口径：`crates/farwork-agent/tests/dsh_live.rs:642` 的 `PROBE SUMMARY … inbound_requests=` 为 **0**，即桥**从不反向发请求**，checkpoint 只能搭 ack 回去（本轮未复跑该探针）。

- **`batchSeq` 必须严格 +1**（`sync.rs:6-8,26,63-66`）：跳号或乱序意味着事件已经丢了，**唯一安全的恢复是重新订阅**，不是「从下一个号接着跑」（`mod.rs:308,322` → `resubscribe`，`mod.rs:774-779`）。重新订阅会重置序号（`sync.rs:89-99`）。
- **快照 `begin → items → commit` 先暂存、后提交**（`sync.rs:152-277`）：`commit` 核对**条数**与 **id 唯一性**，不匹配整份丢弃并报错（`sync.rs:34-42,233-266`）；新的 `begin` 直接替换半份旧快照（`sync.rs:191-210`），`abort` 抛掉它（`sync.rs:268-277`）。理由在 `sync.rs:9-11`：**半份快照比没有快照更糟**。
- **checkpoint 落盘，按 native session 一个文件**：`$FARWORK_HOME/dsh/sync/checkpoints/<externalSessionId>.json`（`sync.rs:304-311`；默认目录函数 `default_dir`），目录 0700、文件 0600，写入是「同目录临时文件 + `rename`」的原子替换（`sync.rs:452-489`）——读到半个 checkpoint，桥就会从一个它从未确认过的边界续传。**坏文件/外来文件一律当「没有 checkpoint」**：读不动或解析失败只 warn 跳过那一份，不连累其余（`sync.rs:412-450`）。它仍然只是加速手段：丢了就是一次全量重放，不会丢事件（`sync.rs:281-291`）。**peer 给的 `externalSessionId` 直接变成文件名**，所以必须是单个普通路径组件——非空、≤200 字节、不以 `.` 开头、不含 `/` `\` `NUL`（`sync.rs:402-410`），不合法就拒绝写并 warn（`sync.rs:343-352`）；临时文件不以 `.json` 结尾，因此半成品永远不会被当成完整的一份（`sync.rs:426-431`）。
- **`checkpoint.*` 由桥驱动，agent 只做三件事**：`load` → 答案随下一次 ack 发出（`null` 表示没有）；`save` → 先落盘再 ack；`delete` → 内存与文件一起删（`sync.rs:369-383`；`mod.rs:542-576`）。一个批次要两份 checkpoint 时只答最后一份并 warn（`mod.rs:551-556`），因为一次 ack 只带得下一个。
- **冷启动首连仍可能失败（桥侧 10000 事件队列）**：桥报 `runtime.error{data.scope:"sync"}` 表示它自己的事件队列（10000 条）在重放整个 inventory 时爆了（`mod.rs:320-328`）。两条路：**有 checkpoint 时最多重订阅 3 次**（`MAX_SYNC_RECOVERIES`，`mod.rs:48-53,356-366`），每次重订阅前已流到的会话 checkpoint 都在盘上，下一次从更后面续传；**没有 checkpoint 时一次性失败**（退出码 1），错误文本直接给可操作动作——归档旧会话（或让桥上少开点会话）再重连，并报出已持久化的 checkpoint 数与目录（`mod.rs:367-382`）。重试只对「能续传」有意义：续不上就又是从头的全量重放，队列会再爆一次。

### 10.5 能力诚实：真机只读探测的实测值

DSH 的四个能力布尔不是常量，而是从 `session.getCapabilities`（`client.rs:257-262`）的条目里查（`capability_flags`，`projection.rs:248-267`）：`steer ← session.steer`、`interrupt ← session.interrupt`、`selections` ← 两份 catalog 非空 且（`catalog.model` 或 `catalog.permission`）（`projection.rs:253-260`）。catalog **只**来自 `catalog.listModels` / `catalog.listPermissions`（`client.rs:241-245`），空表就报空表（`projection.rs:230-231`）——不编占位项。

下表是 `crates/farwork-agent/tests/dsh_live.rs` 的**真机只读探测记录**（只跑 `initialize` + `session.list` + 两份 catalog，**不建会话**）。**本轮只改文档、没有重跑**；要复核就按 §8 的命令跑那条 `#[ignore]` 测试。

| 项 | 实测值 | 我们的处理 |
| --- | --- | --- |
| `identity.runtimeVersion` | `0.2.1-rc.2` | 只写日志（`mod.rs:173-181`），不做版本分支 |
| `identity.bridgeVersion` | `2.1.0-desktop…` | 同上 |
| `identity.protocolVersion` | `1.0` | 只比 major（`handshake.rs:154-158`） |
| `features.approval` | `true` | 审批通知照转（`projection.rs:182`） |
| `features.userQuestions` | `true` | 问题通知照转（`projection.rs:188-195`） |
| `features.projectionVersion` | `3` | 保留字段，事件按 v3 解读 |
| `features.syncMode` | `events` | 唯一支持的模式（`mod.rs:183-192`） |
| `session.steer` | `supported=false` | `steer:false` → 浏览器不显示补充指令框 |
| `session.interrupt` | `true` | `interrupt:true` |
| 选择（model / permission） | 两份 catalog 都非空 | `selections:true` |
| `catalog.listModels` | **2 条** | 原样上报两条 |
| `catalog.listPermissions` | **4 条**：`read-only` / `workspace-write` / `danger-full-access` / `Auto review` | 原样上报；`--dsh-permission` 的短名就解析到这里（`selections.rs`；`cli.rs:163-175` 只允许在 `--runtime dsh` 下用） |
| `session.commands` | 桥报 `true`，**我们报 `false`** | 见下 |

**`session.commands` 是「不假装」的样板**：真桥确实上报了 `session.commands: true`，但本仓**没有实现 `session.executeCommand`**，也没有能填满 `GET /sessions/{id}/runtime/commands` 的真实数据源，所以 `capability_flags` 把 `commands` 固定成 `false`（`projection.rs:261-264`），`session.capabilities` 帧里的 `commands` 数组恒为空（`projection.rs:268-283`）。一个「填得满、点了不执行」的命令框比一个不出现的按钮更糟：能力位只能声明我们真的能兑现的东西。桥报的 `true` 也不许透传——那等于拿桥的能力冒充本 agent 的能力。

### 10.6 通知与回答：`actionId` 的闭环

DSH 的通知不是「`kind` + 三个决定」，而是带 `interactionType` 和一组**运行时自己命名的按钮**。agent 侧的处理是：

- **入**：批次里的 `notice.upsert` 操作 → `projection::notice`（`mod.rs:501-544`；`projection.rs:159-229`）。`options` 由桥的 actions 归一化成 `{id,label,style?}`（`projection.rs:114-157`）；`interactionType` 既进 `body` 也进 notice 顶层字段（`projection.rs:216,224`；ingress 侧 `ingress.rs:838-849`）。`responseRequired` 却**没有任何 action** 的通知直接丢弃并 warn（`projection.rs:202-207`）——发出去就是一张点不了任何按钮的卡。不是本 `sessionId` 的通知直接不转发（`projection.rs:160-162`）。
- **回答**：hub 的 `notice.respond` 带 `actionId` 下来（`ingress.rs:472-499`），agent 先按这条 notice **记录下来的 action 列表重新校验**（`interaction.rs:95-126`），不合法就回 error 帧并把通知放回队列（`mod.rs:683-706`）；桥回 `{ok:false}`（业务失败，不是 RPC 错误）同样算失败：恢复队列、回 error（`mod.rs:711-726`），**不写「已解决」**。只有成功才发 `notice.resolved`，并在能映射到闭集时附带 `decision`（`projection.rs:284-300`；`mod.rs:726-736`）。
- 于是 **hub 的 `422 unknown_action` 与 agent 的二次校验是两道闸**：前者防一个晚到的浏览器标签页塞进桥没给过的按钮，后者防 hub 与 agent 版本错位。

## 11. 自研 DSH 桥插件：`packages/dsh-plugin`（`@farwork/dsh-bridge`）

DSH 那一侧原本要装上游插件（`@agents-anywhere/dsh-bridge-next`）+ 上游的 Python connector。本仓把桥自己实现成一个 **DSH host plugin**（TypeScript / Cordis，**不是** cargo workspace 成员，也不在 pnpm workspace 里——根目录没有 `pnpm-workspace.yaml`，构建只用 Node 自带能力），于是这条链路里不再有 Python。

### 11.1 为什么自研

- **去掉对上游插件与其 Python connector 的依赖**：实测桥的工作 **100% 都在 TS 插件内**（loopback JSON-RPC 服务端 + 事件流 + 投影），上游那个 Python connector 只是**另一个客户端**——它不给桥加任何能力。既然本仓的客户端是 `farwork-agent`，跟着上游装 Python connector 就是纯负担（也与 §4.1 的零 Python 相冲突）。
- **契约以本仓为准**：wire 形状（`initialize` / `session.*` / `runtime.sync.subscribe|ack`）在 `crates/farwork-agent/src/dsh/` 已经是唯一实现且有测试，桥按它实现，而不是按上游某个版本的实现反推。

### 11.2 与上游插件共存：endpoint 必须换目录

| | 上游插件 | 本插件 |
| --- | --- | --- |
| rendezvous 文件 | `~/.agents-anywhere/dsh-bridge/endpoint.json` | **`~/.farwork/dsh-bridge/endpoint.json`** |
| manager lock | `…/.agents-anywhere/dsh-bridge/.manager-lock.json` | **`…/.farwork/dsh-bridge/.manager-lock.json`** |
| TCP | `listen(0)` 随机端口 | 同 |

**锁是按目录取的，而锁与 endpoint 文件同目录**（`packages/dsh-plugin/src/endpoint.ts:1-12,45-56`；`acquireManagerLock` 在 `:165`）。所以「两个桥共享一个目录」＝互相覆盖 endpoint、互相把对方踢下线；换一个目录就得到两把独立锁，两者可以同时跑。默认值就是 `~/.farwork/dsh-bridge`（`endpoint.ts:37-48`），`endpointDir` 可配但必须是绝对路径（`src/index.ts:33-50`）。endpoint 用 `link()` 发布而不是 rename/直写——读者永远不会看到半份文件，且 `link` 撞车时是失败而不是覆盖别人（`endpoint.ts:1-12,77-89`）。

> **agent 的发现顺序已经包含它**（`dsh/discovery.rs:173-191`）：`--dsh-endpoint` → `~/.farwork/dsh-bridge/endpoint.json`（自研，排第一）→ 上游的两条。装好本插件后裸 `--runtime dsh` 就能找到它，不必带 `--dsh-endpoint`；启动日志里的 `dsh_endpoint_source`（`explicit`/`farwork`/`upstream`/`upstream-dsh-home`，`discovery.rs:69-88`；`main.rs:89-105`）说明**到底是哪一个桥在服务**。⚠️ 插件自己的 README 里「crate 的默认发现路径仍指向上游目录」那句已过期（代码先行改了），以本节为准。

### 11.3 已实现的方法集（v1）

`initialize` / `ping` / `session.list` / `session.getState` / `session.getSnapshot` / `session.createAndStart` / `session.startTurn` / `session.interrupt` / `session.getCapabilities` / `runtime.getCapabilities` / `runtime.sync.subscribe` / `runtime.sync.ack`（`src/router.ts:103-133`）。

其余一律 `-32601 METHOD_NOT_FOUND`（`router.ts:129-131`）：`session.getNotices`、`session.respondInteraction`、`session.updateSelections`、`session.listCommands`、`session.executeCommand`、`catalog.*`、`workspace.list`、`runtime.sync.progress|unsubscribe|refresh`。**半实现的方法比明确拒绝更糟**：客户端会相信它的回答。

两个身份也是按 §10 的契约派生的：`platformId` = hub 的 session id（wire 上的 `sessionId`），`externalSessionId` = `aa_` + `sha256(namespace)[0:16]` + `_` + platformId，靠**派生**而不是枚举会话解析，所以一次会话级调用不依赖 `sessionQuery` 是否加载（`src/identity.ts`；插件 README「Identity contract」）。

### 11.4 v1 不实现审批 / 问答（以及怎么降级）

DSH 的审批与用户提问**没有公共入口**。上游的做法是反射进 `typertGateway.wireStream.open(...)`、按函数的 `length`（5 vs 3 个参数）分支、用 `connection.browserPeer ?? connection.operator` 当 peer、再手搓一个 `Request` 往 `$events/result` 投答案。这依赖内部签名，且**没有先例**、**没有 fail-soft 版本**：签名一变，插件要么在宿主里抛，要么（更糟）静默吞掉用户的答案。

所以 v1 明说没有它：

- `initialize` 上报 `features.approval = false` / `features.userQuestions = false`；
- `session.getNotices` / `session.respondInteraction` 回 `-32601`。

**降级路径**：浏览器不出现审批卡，`farwork-agent` 也不转发（`dsh/projection.rs` 的 notice 分支按这两个位门禁，§10.3）；卡在审批上的会话必须**在 DSH 自己的 Web UI 里回答**——这条通道正因为本插件从不碰审批路径而照常可用，答完之后回合继续、timeline 继续流动。

### 11.5 已知边界（别当成已支持）

- **投影是故意的子集**：只投 assistant 文本、工具调用、工具结果与中断；reasoning / 图片 / compaction 标记 / 审批记录全部丢弃（`src/projection.ts:1-12`）。声称完整 projection v3 会让将来的 checkpoint 方案撤不回承诺。
- **没有 checkpoint / upload-progress 协商**：`runtime.sync.subscribe` 既不回 `checkpointVersion` 也不回 `uploadProgressVersion`，v1 每次订阅都给全量快照。于是 agent 侧的 `checkpoints` 一直是空的——**§10.4 那条「没有 checkpoint 时一次性失败」正是用本插件时会走到的分支**（有续传的是上游桥）。
- **`session.list` 的标题恒为 `null`**：v1 没有 title 服务，编一个标题就是让浏览器渲染一个谎。
- **subscribe 之后新建的会话要等下一次 subscribe 才出现**：v1 不把半成品 baseline 缝进活流。
- **`features.syncMode` 在宿主没有事件总线时是 `"polling"`**：轮询同样没实现，它只是「不会有流推给你」的诚实说法——agent 会直接拒绝而不是慢慢变旧（§10.3）。
- **`apply()` 永不抛**：启动失败只 `logger.warn` 后返回；一个起不来的桥只该让用户失去桥，不该让宿主挂掉（`src/index.ts:1-8,52-83`）。
- **不 import 任何 DSH 包**：`link:` 装的第三方包解析不到 `@deepseek-ai/cordis`（它只在 `app.asar` 里），宿主能力一律 duck-typed 探测，缺服务是「那个方法拒绝」而不是加载期崩溃（`src/runtime.ts:1-8`）。

### 11.6 安装 / 卸载（都要重启 DSH）

```bash
node packages/dsh-plugin/scripts/build.mjs          # dist/ 不入库，先构建
dsh plugin --profile desktop add "link:$PWD/packages/dsh-plugin"
# 再把 "@farwork/dsh-bridge" 写进 ~/.dsh/profiles/desktop/package.json 的 dsh.profile.bundles
# （dsh plugin add 只装包；bundles 列表由 Desktop 插件管理器或手改写入）
# 完全退出 DSH 再启动（不要在有 live session 时做）
cat ~/.farwork/dsh-bridge/endpoint.json             # 验证 rendezvous 文件
```

agent 侧（**注意 `--dsh-endpoint`**，理由见 11.2）：

```bash
cargo run -p farwork-agent -- --runtime dsh --token '<connectorToken>' \
  --workdir /srv/work --dsh-endpoint ~/.farwork/dsh-bridge/endpoint.json
```

卸载：`dsh plugin --profile desktop remove @farwork/dsh-bridge` → 从 `dsh.profile.bundles` 删掉 `"@farwork/dsh-bridge"` → `rm -rf ~/.farwork/dsh-bridge` → 重启 DSH。插件正常关闭时也会自己收尾：只有当 `endpoint.json` 里的 token+pid 仍是自己才 unpublish（`endpoint.ts:90-127`），锁在 `close()` 里比对 token+pid+port 之后才 unlink（`endpoint.ts:231-242`）。

构建与自测（**本轮没有跑**）：`node scripts/build.mjs`（用 Node 自带的 `stripTypeScriptTypes`，**零构建依赖**——链接安装时 DSH 只做一次裸 `import()`）、`node --test dist/__tests__/*.test.js`、`node scripts/selftest-bridge.mjs`（假 ctx + 真 TCP + `mkdtemp` 的 endpoint 目录；不碰 `~/.dsh`、`~/.farwork`、62082 端口或正在跑的 DSH）。

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
