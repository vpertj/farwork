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
        |                          migrations/0001_init.sql + 0002_connector_ingress.sql，启动时自动应用
        ^
        |   WS  /api/v2/connector/ws?token=...    (Ingress Protocol v1，见 §5)
        |   连接由设备侧主动发起（出站），hub 从不反向拨号
        |
 farwork-agent（被接管的设备，常驻）  ---stdio JSON-RPC-->  codex app-server（`codex app-server --stdio`）
```

| 组件 | 位置 | 职责 | 对外 |
| --- | --- | --- | --- |
| `farwork-hub` | `crates/farwork-hub` | `/api/v2` 服务端：REST + 3 个 WS（dashboard / session / **connector**）+ `POST /ws-ticket`、认证、PostgreSQL 持久化、connector ingress 的 hub 半边、`501` 兜底 | 端口 7300 |
| `farwork-timeline` | `crates/farwork-timeline` | timeline **纯域逻辑**：seq 分配、`content_hash` 去重、`order_seq`、游标恢复、每会话写栅栏。无 IO、无时钟 | 库 |
| `farwork-protocol` | `crates/farwork-protocol` | **M1 遗留**：M1 线协议类型，仅 server/connector 使用 | 库 |
| `farwork-server` | `crates/farwork-server` | **M1 遗留**：`/api/health`、`/api/devices`、`/ws/connector`、`/ws/client` 终端骨架 | 端口 7300（与 hub 冲突） |
| `farwork-agent` | `crates/farwork-agent` | **M3 真正的 connector**：主动出站连 hub 的 `/connector/ws`，按 `session.start` 拉起本地 `codex app-server`（或 `--runtime echo` 的假后端），把 codex 的 JSON-RPC 翻成 Ingress Protocol v1 上行 | 无监听端口（只出站） |
| `farwork-connector` | `crates/farwork-connector` | **M1 遗留**：按 `terminal.open` 起 PTY 并双向转发，只连 `farwork-server` 的 `/ws/connector`；**没有 ingress 协议，也没接 hub** | 出站连接 |
| `apps/web` | `apps/web` | SvelteKit 静态 SPA：登录、connectors/项目/会话、timeline、发消息、WS 增量、审批卡、新建会话表单（`#/new-session`）、会话中断 | dev 端口 5173 |

> **`farwork-agent` 与 `farwork-connector` 是两件不同的东西，不是一次重命名。** `farwork-connector`（M1）说的是终端帧协议（`terminal.open` / `terminal.data` / `terminal.exit`），只跟 `farwork-server` 讲话，设备 id 每次启动重新生成；`farwork-agent`（M3）说的是 Ingress Protocol v1（`hello` / `session.*` / `item.upsert` / `notice.*`），只跟 `farwork-hub` 讲话，身份落盘在 `$FARWORK_HOME/agent.json`。两者不共享线协议也不共享代码路径；M3 没有删除 `farwork-connector`，它仍是 M1 终端探针。

**关键分工：timeline 的语义不在 hub 里。** `farwork-timeline` 拥有语义（序列分配、按内容哈希去重、`order_seq`、游标恢复、以及「宁可拒绝也不发部分 delta」的规则），hub 只拥有**持久化与传输**——从 PostgreSQL 水合引擎、回写已提交条目、wire 适配（上游多出的 `source` / `completedAt` / `protocolVersion` 由 hub 补）、以及进程内每会话引擎缓存。出处：`crates/farwork-hub/src/timeline.rs:1-20`。

由此产生两条可验证的性质：

- **进程重启不改变语义**：已提交条目与水头（`timeline_items` + `sessions.seq`）都在 PostgreSQL，重放顺序即 `updated_seq` 顺序。
- **无法重放就明说**：存了 `revision > 1` 的条目无法按重放复原（重放会退化成 revision 1），水合时直接给出 refetch 信号，而不是假装一致（`crates/farwork-hub/src/timeline.rs:18-20`）。

## 2. 端口与环境变量

| 端口 | 用途 | 说明 |
| --- | --- | --- |
| `7300` | `farwork-hub` | `/api/v2` REST + 3 个 WS；`GET /healthz` 也在此 |
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

`/api/v2` 下共 **38 个已实现的 method+path 组合**，由 31 次 `.route(...)` 注册（auth 5 + connectors 5 + projects 3 + sessions 11 + notices 3 + connector_ws 1 + ws 3；`routes/mod.rs:17-27` 合并 8 个子 router，其中 `unsupported` 子 router 用宏注册 49 条 `501` 路径，见 §4）。另有 `GET /healthz` 挂在根（`lib.rs:33`）。

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
| `GET` | `/sessions` | 会话分页（不透明 `cursor`，按 `pinned/sort_at/updated_seq/id` 排序） | `sessions.rs:28,643-659` |
| `POST` | `/sessions` | **导入**一个已存在于运行时的会话：必须带 `externalSessionId`（否则 422 提示改用 `create-and-start`）。导入会话只读（`takeover:false`） | `sessions.rs:28,458-514` |
| `GET` | `/sessions/list` | 不分页的 dashboard 清单（行数上限 100，取 `pinned/sort_at/updated_seq/id` 序） | `sessions.rs:29,661-687` |
| `POST` | `/sessions/create-and-start` | 建会话 + 落首条消息 + **下发 `session.start`**。首条用户消息随 `session.start` 的 `prompt` 走（此刻还没有流可以 `session.message`）；`runtime` 必须在允许集合内（`echo` 只在 `FARWORK_HUB_DEV_RUNTIMES` 里才放行，否则 422）。connector 不在线时仍回 201/200，但 `runtimeDispatched:false` + `connectorResult.code = connector_offline` | `sessions.rs:30,516-641` |
| `GET` | `/sessions/{session_id}/meta` | 会话元数据 | `sessions.rs:31-34` |
| `PATCH` | `/sessions/{session_id}/meta` | 改标题/置顶/归档/takeover | `sessions.rs:31-34` |
| `GET` | `/sessions/{session_id}/timeline` | 条目读接口，`mode=latest\|changes\|history` + `limit/afterSeq/beforeOrderSeq`。**客户端不得轮询**（见 AGENTS.md 4.3） | `sessions.rs:35,769-806` |
| `GET` | `/sessions/{session_id}/snapshot` | 一次拿齐：会话 + runtime state + timeline 首页 + **真实 notices**（来自 `session_notices`）+ `eventCursor`。`approvals` 仍是空数组、`effectiveCapabilities`/`runtimeCapabilities` 仍是空集 `revision:0`（理由见 §5.8） | `sessions.rs:36,808-846` |
| `GET` | `/sessions/{session_id}/events` | 事件补拉，`after=seq:N`；无法无缝续接时回 `snapshotRequired:true` | `sessions.rs:37,860-914` |
| `GET` | `/sessions/{session_id}/runtime/state` | 运行时状态（无则 404）。M3 起由 connector 的 `session.state` / `session.exit` 真实驱动 | `sessions.rs:38-41,916-929` |
| `GET` | `/sessions/{session_id}/runtime/capabilities` | 仍是空集 `revision:0`：Ingress Protocol v1 没有 capability 帧，没协商过就不编 | `sessions.rs:42-45,931-944` |
| `POST` | `/sessions/{session_id}/runtime/messages` | 发消息：先落 timeline（`clientMessageId` 幂等，重复不重放），**提交之后**才向活跃流下发 `session.message`。要求会话可写：非归档且 `takeover`，否则 409。响应里的 `runtimeDispatched` 才是「是否真的投给了 agent」 | `sessions.rs:46-49,946-1032` |
| `POST` | `/sessions/{session_id}/runtime/interrupt` | 下发 `session.interrupt`。没有活跃流 → 409 `session_not_running`；connector 不在线或 socket 已断 → 409 `connector_offline`。拿到 200 就意味着帧真的进了 socket | `sessions.rs:50-53,1044-1096` |

### 3.5 审批（notices） —— `routes/notices.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `GET` | `/sessions/{session_id}/runtime/notices` | 该会话的审批列表，字段 `id/kind/status/title/body/options/decision/createdAt/updatedAt`。`decision` 是 hub 侧的**附加**字段：契约的 notice 形状没有表达「怎么关掉的」的位置，而只看 `status` 会让浏览器分不清 approve 与 reject | `notices.rs:25,253-277` |
| `POST` | `/sessions/{session_id}/notices/{notice_id}/respond` | 浏览器契约里的拼法；body `{"decision":"approve"\|"approve_for_session"\|"reject"}` | `notices.rs:29-32,279-341` |
| `POST` | `/sessions/{session_id}/runtime/notices/{notice_id}/respond` | 同一 handler 的别名（原 `501` 表用的就是 runtime 命名空间，为兼容保留） | `notices.rs:33-36` |

三条行为契约（M3 时定死，别改成「看起来更快」的版本）：

- **只下发，不乐观 resolve**：respond 成功时响应里的 `notice.status` 仍是 `open`；`resolved` 只有 connector 的 `notice.resolved` 能写（`notices.rs:330-341`）。
- **投递失败就报错**：connector 不在线或 socket 已断 → `409 connector_offline`，审批保持 `open`。浏览器点了按钮不等于 agent 收到了决定。
- **状态机**：非 `open` 的 notice 再 respond → `409 notice_already_resolved`；未知 notice → `409 notice_not_found`；`decision` 不在三值内 → 422。

### 3.6 WebSocket —— `routes/ws.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/ws-ticket` | 换取一次性票据（60s TTL，`scope` 必须是 `{sessionId}` 或 `{dashboard:true}` 之一） | `ws.rs:33,112-150` |
| `GET` | `/dashboard/ws` | 总览流：connectors / projects / 会话状态 | `ws.rs:34,292-320`（socket 循环 `398-420`） |
| `GET` | `/sessions/{session_id}/ws` | 会话事件流：先发 `session.subscribed`，随后增量；15s keepalive | `ws.rs:35,159-291` |

### 3.7 `GET /healthz`（不在 `/api/v2` 下）

```json
{"status":"ok","version":"0.1.0"}
```

源码：`crates/farwork-hub/src/lib.rs:33,45-48`。

## 4. `501 not_implemented`：**49 条路径**

`crates/farwork-hub/src/routes/unsupported.rs:19-77` 用一个宏注册了 49 条路径，全部 `any(not_implemented)`，返回 `501` 且 body 里带 `"METHOD /path is not implemented in this build"`；未注册的其余路径落到 `unknown_path` → `404`（`unsupported.rs:78,81-105`）。

M3 从这张表里**移出了 3 条**并改为真实实现：`/sessions/{id}/runtime/interrupt`（§3.4）、`/sessions/{id}/runtime/notices` 与 `/sessions/{id}/runtime/notices/{notice_id}/respond`（§3.5）。移出这 3 条不等于那组都补齐了：steer / commands / selections / catalogs / sync / takeover 仍在表里。

| 分组 | 路径（方法不限） |
| --- | --- |
| 会话运行时控制（steer / commands / selections / catalogs / sync / takeover） | `/sessions/{id}/runtime/steer`、`/runtime/commands`、`/runtime/selections`、`/runtime/catalogs/{catalog}`、`/sessions/{id}/sync`、`/sessions/{id}/takeover` |
| 终端与文件浏览中继 | `/sessions/{id}/terminal`、`/sessions/{id}/terminal/{*rest}`、`/sessions/{id}/files`、`/sessions/{id}/files/{*rest}` |
| 分享 | `/sessions/{id}/shares`、`/sessions/{id}/shares/{*rest}`、`/shares`、`/shares/{*rest}` |
| 批量会话维护 | `/sessions/archive`、`/sessions/unarchive`、`/sessions/read` |
| 设备运行时管理 | `/connectors/{id}/runtimes`、`/connectors/{id}/runtimes/{*rest}`、`/connectors/{id}/protocol/capabilities`、`/connectors/{id}/pairing`、`/runtimes`、`/runtimes/{*rest}`、`/devices`、`/devices/{*rest}` |
| OAuth / 邮件码 / 移动端扫码登录 | `/auth/oauth/start|callback|finalize`、`/auth/mobile-login/{qr,exchange,refresh,request,status,confirm}`、`/auth/email-code`、`/auth/change-password`、`/auth/me/email|profile|avatar` |
| 实例管理 / 公告 / agents / usage / 配对 | `/admin`、`/admin/{*rest}`、`/announcements`、`/announcements/{*rest}`、`/agents`、`/agents/{*rest}`、`/pairing`、`/pairing/{*rest}`、`/usage`、`/usage/{*rest}` |

另有一处**带内** `501`：`POST /sessions/{id}/runtime/messages` 携带非空 `attachments` 时返回 `501 attachments are not implemented in this build`（`sessions.rs:865-873`）——拒绝好过静默丢弃附件。

## 5. Connector Ingress（M3）：Ingress Protocol v1

一个端点、两个方向、一份契约。hub 半边是 `crates/farwork-hub/src/ingress.rs`（连接表 + 帧构造 + 入站解析）与 `crates/farwork-hub/src/routes/connector_ws.rs`（socket 循环）；agent 半边是 `crates/farwork-agent/src/protocol.rs`（`ClientFrame` / `ServerFrame`）与 `crates/farwork-agent/src/hub.rs`（连接循环）。**两侧字段必须同改**（hub `ingress.rs` + agent `protocol.rs` + 本文），见 AGENTS.md。

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

hub 侧类型是 `crates/farwork-hub/src/ingress.rs:367-437` 的 `ConnectorFrame`；agent 侧类型是 `crates/farwork-agent/src/protocol.rs:113-163` 的 `ClientFrame`。两侧的 `type` 判别值逐字相同。

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
| `error` | `message`, `streamId?` | 只记 warn（带 `streamId`），不改变会话状态、不断连 |

### 5.3 下行（hub → connector）

hub 用 `ingress.rs:286-354` 的六个构造函数产出帧；agent 用 `protocol.rs:166-215` 的 `ServerFrame` 解析。

| `type` | wire 字段 | agent 侧行为 | hub 何时发 |
| --- | --- | --- | --- |
| `welcome` | `serverVersion`, `heartbeatSecs` | 握手完成；心跳间隔取 `max(heartbeatSecs,1)` | 握手成功后 |
| `session.start` | `streamId`, `sessionId`, `runtime`, `cwd`, `prompt` | 校验 `runtime` 与本进程 `--runtime` 一致（不一致 → `error` + `session.exit{code:2}`，绝不冒充别的运行时）；一致则 spawn `codex app-server --stdio` 并回 `session.started` | `POST /sessions/create-and-start`（`sessions.rs:615-641`） |
| `session.message` | `streamId`, `text`, `clientMessageId`（**可选**：hub 无值时不发该键；agent 必须能解析缺失与 `null`） | 追加到该流的 codex 线程（`turn/start`），`clientMessageId` 缺失时本地生成 `cmid_<uuid>` 当幂等键；无该流 → `error` | `POST /sessions/{id}/runtime/messages`，且仅当该会话有活跃流 |
| `session.interrupt` | `streamId` | 映射成 codex `turn/interrupt`；`turn/completed{status:"interrupted"}` 把在跑条目收成 `interrupted` | `POST /sessions/{id}/runtime/interrupt` |
| `notice.respond` | `noticeId`, `decision`（**无** `streamId`，与上行同形） | 在所有 live worker 里按 noticeId 找提示并回 JSON-RPC result；找不到只 warn | `POST .../notices/{id}/respond` |
| `error` | `message`, `streamId?` | 只记日志，**不断连** | 无法路由的入站帧、心跳超时、被新连接顶替 |

**未知 `type` 必须丢帧不断连**：agent 的 `parse_server_frame` 把「未知类型」与「已知类型但 payload 不合法」分开（`protocol.rs:242-266`），前者 debug 后丢弃、后者 warn 后丢弃，socket 都不关。

### 5.4 `clientMessageId` 是可选字段（曾静默丢整轮，故两侧都必须宽容）

**当前契约**（三处已同改，见 `ingress.rs:324-346`、`protocol.rs:186-200`、`hub.rs:303-319`）：

- **hub 侧：可选字段**。`session_message_frame(stream_id, text, client_message_id: Option<&str>)` 在无值时**不输出 `clientMessageId` 键**，而不是序列化成 `null`（`ingress.rs:337-346`）。测试 `ingress::tests::session_message_frame_omits_an_absent_client_message_id`（`ingress.rs:586-604`）断言该键不存在、编码结果里不含这串字面量、键数恰为 `type`/`streamId`/`text` 三个。
- **agent 侧：`Option<String>`**。`ServerFrame::SessionMessage.client_message_id` 带 `#[serde(default, rename = "clientMessageId", skip_serializing_if = "Option::is_none")]`，因此**缺失、`null`、字符串三种都能解析**（`protocol.rs:192-200`）；测试 `protocol::tests::session_message_parses_without_a_client_message_id`（`protocol.rs:372-385`）以及 `session_message_parses_a_present_client_message_id`（含 `null` 分支，`protocol.rs:388-406`）。
- **agent 自造本地 id**：hub 未提供 id 时，agent 生成 `cmid_<uuid>` 交给 codex 的 `turn/start` 当它自己的幂等键；该 id **不回落 hub**，timeline 幂等仍只由 `unique (session_id, client_message_id)` 保证（§4.5；`hub.rs:303-319`）。
- **契约结论**：`clientMessageId` 在 ingress 上是**可选**字段；hub 无值时不发该键；agent **必须**同时能解析「缺失」与 `null`。

**为什么两侧都要宽容（踩过的坑，别改回去）**：这个缺陷真的发生过 —— hub 曾序列化 `"clientMessageId": null`，agent 曾要求必填 `String`，serde 报错后这一帧被 `parse_server_frame` 归入 `Malformed` 并丢弃（`hub.rs:237-239` 只 warn），而 hub 已经把这一轮落库并回了 `runtimeDispatched:true` —— 界面显示「已发送」，运行时毫无动静。省略该字段的 REST 调用者必踩；Web UI 恒发 `cmid-<uuid>`（`apps/web/src/lib/session.svelte.ts:194`），所以页面当时没暴露。经验：**下游可能不传的字段，上游必须能省、下游必须能缺；任一侧收紧成必填都会让整轮静默消失**。

其余差异都是「hub 比 agent 宽松」，不构成破坏，但说明「hub 接受」不等于「契约要求」：

| 字段 | agent（发送方） | hub（接收方） |
| --- | --- | --- |
| `hello.agent.platform` / `.version` | 必填 `String` | `Option<String>`，缺失也接受 |
| `heartbeat.ts` | 必填 `String` | `Option<String>` |
| `session.started.externalSessionId` | 必填 `String` | `Option<String>` |
| `session.exit.code` | 必填 `i32` | `Option<i64>`，缺失按 0 处理 |
| `item.itemTime` | 必发（agent 自己的钟） | `Option<String>` 且**只收不用**：排序一律用 hub 的 `orderSeq`，绝不用它排序或盖时间戳 |
| `notice.resolved.streamId` | 不发 | 接受并忽略（`ingress.rs:405-417`），因为该帧契约上无 `streamId` |
| `notice.upsert.status/title/body/options` | 必发（`status` 恒为 `open`） | 全部可选，缺省 `open`/空串/`null`/`[]` |

### 5.5 为什么 ingress 有两条必须落库的表

- **`session_streams` 让 `streamId` 跨 hub 重启存活**：`streamId` 由 hub 生成，connector 之后每条会话级帧**只**回这个 id，没有第二个会话标识。映射若只在内存，hub 重启后仍在跑的 turn 的所有 `item.upsert` 就无处路由、只能丢（`migrations/0002_connector_ingress.sql:1-12`）。
- **`session_notices` 让审批跨刷新存活**：审批是人的节奏，比浏览器会话长。待办审批必须在刷新、换设备、断线重连之后仍然是待办，所以它在表里而不在内存里。

### 5.6 hub 不做乐观 resolve；断连必须收敛

- `POST .../notices/{id}/respond` 只下发 `notice.respond`，响应里 `status` 仍是 `open`；`resolved` 只由 connector 的 `notice.resolved` 写入（§3.5）。hub 抢先置 `resolved` 的话，一旦投递失败就会留下「已批准但没人执行」的假状态。
- socket 一结束就 `abandon_connector_streams`：关掉该 connector 全部未关流，把对应会话的 `session_runtime_states.status` 与 `sessions.status` 一起标 `error`（`status_reason = "connector disconnected"`）再广播（`ingress.rs:207-247`；`connector_ws.rs:167-193`）。**在途流收敛成显式 error，而不是静默丢弃。**
- 同理 `POST runtime/messages`：没有活跃流就只落库并回 `runtimeDispatched:false`，不会假装派发。

### 5.7 codex 集成的真实边界

agent 不实现 agent 逻辑，只做协议翻译：一个 `session.start` = 一个 `codex app-server --stdio` 子进程，线上是 newline-delimited JSON-RPC（`crates/farwork-agent/src/codex/transport.rs:1-40`）。method 与字段名取自本机 `codex-cli 0.162.1` 生成的 schema，不是凭记忆（`codex/mapper.rs:8-18`）。

- **审批词表映射（本仓 3 值 ↔ codex 两套 family）**：`approve` / `approve_for_session` / `reject` 在下行映射为
  - v2 family（`item/commandExecution/requestApproval`、`item/fileChange/requestApproval`）：`accept` / `acceptForSession` / `decline`
  - legacy family（`execCommandApproval`、`applyPatchApproval`）：`approved` / `approved_for_session` / `denied`

  出处 `codex/mapper.rs:1058-1070`；上行时 notice 的 `options` 恒为这三个本仓词（`mapper.rs:568-700`）。
- **`item/permissions/requestApproval` 被拒绝**：v2 的响应要求一个 `GrantedPermissionProfile`，凭空构造等于替用户授予他没看过的权限，所以回 JSON-RPC `-32601` 并上抛一条 error（`mapper.rs:652-668`）。宁可失败，也不伪造授权。
- **codex 的 `userMessage` 不复述**：用户消息已由 hub 按 REST payload 落库（带 `clientMessageId` 幂等），再上行 codex 的副本会让同一句提示在 timeline 出现两次（`mapper.rs:712-718`）。
- **失败要如实落 timeline**：codex 的 `error` 通知（例如凭据失效的 401）生成一条 `system` + `failed` 条目，而不是只写日志（`mapper.rs:536-560`）；标了 `willRetry` 的瞬时错误不落条目。
- **不给会话挂死**：审批默认 600s 超时，超时后 agent 取消该审批并说明原因（`mapper.rs:42`）；流式文本攒够 256 字符才 upsert（`mapper.rs:36`）；命令输出超过 20000 字符保留尾部并标记截断（`mapper.rs:39`）。

### 5.8 尚未接通的部分（别当成已支持）

- `snapshot.approvals` 仍是 `[]`：可消费的审批形状走 `notices`，`approvals` 这个字段没有实现（§3.4）。
- `runtime/capabilities` 仍是空集：Ingress Protocol v1 没有 capability 帧，没协商过就不编。
- 文件 / 终端 / steer / takeover 都没有 ingress 帧，对应 REST 仍在 `501` 表里（§4）。
- codex 的**成功路径**（assistant 文本增量、工具执行、真实审批往返）在本机未验证：没有 Codex 凭据。见 `docs/roadmap.md` 的 M3。

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

- 迁移文件 **2 个**：`crates/farwork-hub/migrations/0001_init.sql` + `0002_connector_ingress.sql`（M3）。启动时由 `db::migrate` 通过 `sqlx::migrate!("./migrations")` 应用，sqlx 自带 ledger，重复启动是 no-op（`db.rs:17-22`；`main.rs` 的启动序列）。
- **10 张表**（`create table` 计数）：0001 的 `users`、`app_settings`、`connectors`、`projects`、`sessions`、`session_runtime_states`、`timeline_items`、`ws_tickets`，加 0002 的 `session_streams`、`session_notices`。后两张的存在理由见 §5.5：一张让 `streamId` 跨 hub 重启存活，一张让审批跨刷新存活。
- **12 个索引**（0001 十个 + 0002 两个），其中三条是语义承载点：
  - `uq_timeline_items_client_message`：`unique (session_id, client_message_id)`（`0001_init.sql:132`）——`clientMessageId` 幂等的**唯一**保证。
  - `idx_timeline_items_session_updated_seq` / `_session_order_seq`：游标补拉与展示排序的查询路径（`0001_init.sql:129-130`）。
  - `idx_session_notices_id`：`notice.resolved` 只带 notice id，解析时先按 id 找行、再校验它属于发帧的 connector（`0002_connector_ingress.sql:37-39`）。
- `ws_tickets` 走 `delete ... returning` 消费，行即删，靠 `idx_ws_tickets_expires` 清理（`0001_init.sql:144`；`ws.rs:83-108`）。

## 8. 测试的前置条件（`tests/e2e.rs`）

`crates/farwork-hub/tests/e2e.rs` 是**唯一**的端到端测试，跑真实 PostgreSQL：

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
