# Farwork 架构与线协议

本文是**当前实现（M2 纵向切片）**的架构与 wire 契约。`farwork-hub`、`apps/web` 两侧必须严格一致；M1 的终端协议保留为附录 A（遗留）。

改协议的唯一顺序：域类型（`crates/farwork-timeline` / `crates/farwork-protocol`）→ 本文 → hub 与 web 两侧实现。

## 1. 当前架构（M2）

```
 Browser (apps/web :5173, 静态 SPA)
        |   HTTP  /api/v2/*           (REST, Bearer token)
        |   WS    /api/v2/dashboard/ws
        |   WS    /api/v2/sessions/{id}/ws?ticket=...
        v
 farwork-hub (:7300, axum)  --->  PostgreSQL (唯一状态来源)
        |                          migrations/0001_init.sql，启动时自动应用
        |
        +--(未实现，M3) connector ingress-->  farwork-connector --> agent runtime
```

| 组件 | 位置 | 职责 | 对外 |
| --- | --- | --- | --- |
| `farwork-hub` | `crates/farwork-hub` | `/api/v2` 服务端：REST + 3 个 WS 端点、认证、PostgreSQL 持久化、`501` 兜底 | 端口 7300 |
| `farwork-timeline` | `crates/farwork-timeline` | timeline **纯域逻辑**：seq 分配、`content_hash` 去重、`order_seq`、游标恢复、每会话写栅栏。无 IO、无时钟 | 库 |
| `farwork-protocol` | `crates/farwork-protocol` | **M1 遗留**：M1 线协议类型，仅 server/connector 使用 | 库 |
| `farwork-server` | `crates/farwork-server` | **M1 遗留**：`/api/health`、`/api/devices`、`/ws/connector`、`/ws/client` 终端骨架 | 端口 7300（与 hub 冲突） |
| `farwork-connector` | `crates/farwork-connector` | **M1 遗留**：PTY 转发；**尚未接入 hub** | 出站连接 |
| `apps/web` | `apps/web` | SvelteKit 静态 SPA：登录、connectors/项目/会话、timeline、发消息、WS 增量 | dev 端口 5173 |

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

| 环境变量 | 默认 | 行为 | 源码 |
| --- | --- | --- | --- |
| `DATABASE_URL` | `postgres://tianjun@127.0.0.1:5432/farwork` | hub 的 PostgreSQL 连接串。**库必须已存在**，表由迁移创建；连接失败则启动失败 | `farwork-hub/src/config.rs:6,27` |
| `FARWORK_HUB_PORT` / `PORT` | `7300` | 监听端口，`FARWORK_HUB_PORT` 优先，其次 `PORT`；`--port <n>` 覆盖两者 | `config.rs:7,28-33`；`main.rs:32-34` |
| `FARWORK_HUB_SECRET` | `agent-server-dev-secret` | **未设置时打印 warn**（`FARWORK_HUB_SECRET is not set; falling back to the development secret`）并回退到开发密钥 | `config.rs:9,34-37` |
| `FARWORK_HUB_SETUP_TOKEN` | 未设置 | 首个 admin 注册令牌。未设置时按需生成随机值并 `warn!` 打到日志（30 分钟 TTL，过期轮换），从不出现在 HTTP 响应里 | `config.rs:10,38-40`；`state.rs:55-100` |
| `FARWORK_HUB_DB_POOL` | `10` | sqlx 连接池上限；非法值静默回退 | `config.rs:41-44` |
| `TEST_DATABASE_URL` | 未设置 | **仅测试**：`tests/e2e.rs` 的目标库。缺失时打印 SKIP 并直接返回（见 §7） | `tests/e2e.rs:125-138` |
| `PUBLIC_FARWORK_API` | 空 | 仅 web 构建期：API base。留空 = 同源（dev 走 Vite 代理） | `apps/web/.env.example`；`src/lib/api/client.ts:3` |

## 3. `/api/v2` 端点（已实现）

`/api/v2` 下共 **34 个已实现的 method+path 组合**，由 26 次 `.route(...)` 注册（auth 5 + connectors 5 + projects 3 + sessions 10 + ws 3；`routes/mod.rs:15-23` 合并 6 个子 router，其中 `unsupported` 子 router 用宏注册 52 条 `501` 路径，见 §4）。另有 `GET /healthz` 挂在根（`lib.rs:32`）。

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

> `connectors.status` 在本 build 中恒为 `offline`：**没有 connector ingress**，就没有真实在线值。`routes/sessions.rs:126-127` 的注释明确了这是「诚实值，不是占位符」。

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
| `GET` | `/sessions` | 会话分页（不透明 `cursor`，按 `pinned/sort_at/updated_seq/id` 排序） | `sessions.rs:28,550-566` |
| `POST` | `/sessions` | **导入**一个已存在于运行时的会话：必须带 `externalSessionId`（否则 422 提示改用 `create-and-start`）。导入会话只读（`takeover:false`），本 build 无法授予 takeover | `sessions.rs:28,408-464` |
| `GET` | `/sessions/list` | 不分页的 dashboard 清单（行数上限 100，取 `pinned/sort_at/updated_seq/id` 序） | `sessions.rs:29,568-594` |
| `POST` | `/sessions/create-and-start` | 建会话 + 落首条消息。**不派发 runtime**：响应带 `runtimeDispatched:false` 与 `connectorResult.code = connector_offline` | `sessions.rs:30,466-546` |
| `GET` | `/sessions/{session_id}/meta` | 会话元数据 | `sessions.rs:31-34` |
| `PATCH` | `/sessions/{session_id}/meta` | 改标题/置顶/归档/takeover | `sessions.rs:31-34` |
| `GET` | `/sessions/{session_id}/timeline` | 条目读接口，`mode=latest\|changes\|history` + `limit/afterSeq/beforeOrderSeq`。**客户端不得轮询**（见 AGENTS.md 4.3） | `sessions.rs:35,676-713` |
| `GET` | `/sessions/{session_id}/snapshot` | 一次拿齐：会话 + runtime state + timeline 首页 + `eventCursor`；`approvals/notices/catalogs` 为空数组（无 ingress 时不伪造） | `sessions.rs:36,715-748` |
| `GET` | `/sessions/{session_id}/events` | 事件补拉，`after=seq:N`；无法无缝续接时回 `snapshotRequired:true` | `sessions.rs:37,759-813` |
| `GET` | `/sessions/{session_id}/runtime/state` | 运行时状态（无则 404） | `sessions.rs:38-41,815-828` |
| `GET` | `/sessions/{session_id}/runtime/capabilities` | 运行时能力集（当前空集 `revision:0`） | `sessions.rs:42-45,830-844` |
| `POST` | `/sessions/{session_id}/runtime/messages` | 发消息（写 timeline）。要求会话可写：非归档且 `takeover`，否则 409 | `sessions.rs:46-49,846-905` |

### 3.5 WebSocket —— `routes/ws.rs`

| 方法 | 路径 | 说明 | 源码 |
| --- | --- | --- | --- |
| `POST` | `/ws-ticket` | 换取一次性票据（60s TTL，`scope` 必须是 `{sessionId}` 或 `{dashboard:true}` 之一） | `ws.rs:33,112-150` |
| `GET` | `/dashboard/ws` | 总览流：connectors / projects / 会话状态 | `ws.rs:34,292-320`（socket 循环 `398-420`） |
| `GET` | `/sessions/{session_id}/ws` | 会话事件流：先发 `session.subscribed`，随后增量；15s keepalive | `ws.rs:35,159-291` |

### 3.6 `GET /healthz`（不在 `/api/v2` 下）

```json
{"status":"ok","version":"0.1.0"}
```

源码：`crates/farwork-hub/src/lib.rs:32,44-46`。

## 4. `501 not_implemented`：**52 条路径**

`crates/farwork-hub/src/routes/unsupported.rs:19-79` 用一个宏注册了 52 条路径，全部 `any(not_implemented)`，返回 `501` 且 body 里带 `"METHOD /path is not implemented in this build"`；未注册的其余路径落到 `unknown_path` → `404`（`unsupported.rs:80,93-101`）。

| 分组 | 路径（方法不限） |
| --- | --- |
| 会话运行时控制（审批/中断/接管/同步） | `/sessions/{id}/runtime/interrupt`、`/runtime/steer`、`/runtime/commands`、`/runtime/notices`、`/runtime/notices/{notice_id}/respond`、`/runtime/selections`、`/runtime/catalogs/{catalog}`、`/sessions/{id}/sync`、`/sessions/{id}/takeover` |
| 终端与文件浏览中继 | `/sessions/{id}/terminal`、`/sessions/{id}/terminal/{*rest}`、`/sessions/{id}/files`、`/sessions/{id}/files/{*rest}` |
| 分享 | `/sessions/{id}/shares`、`/sessions/{id}/shares/{*rest}`、`/shares`、`/shares/{*rest}` |
| 批量会话维护 | `/sessions/archive`、`/sessions/unarchive`、`/sessions/read` |
| 设备运行时管理 | `/connectors/{id}/runtimes`、`/connectors/{id}/runtimes/{*rest}`、`/connectors/{id}/protocol/capabilities`、`/connectors/{id}/pairing`、`/runtimes`、`/runtimes/{*rest}`、`/devices`、`/devices/{*rest}` |
| OAuth / 邮件码 / 移动端扫码登录 | `/auth/oauth/start|callback|finalize`、`/auth/mobile-login/{qr,exchange,refresh,request,status,confirm}`、`/auth/email-code`、`/auth/change-password`、`/auth/me/email|profile|avatar` |
| 实例管理 / 公告 / agents / usage / 配对 | `/admin`、`/admin/{*rest}`、`/announcements`、`/announcements/{*rest}`、`/agents`、`/agents/{*rest}`、`/pairing`、`/pairing/{*rest}`、`/usage`、`/usage/{*rest}` |

另有一处**带内** `501`：`POST /sessions/{id}/runtime/messages` 携带非空 `attachments` 时返回 `501 attachments are not implemented in this build`（`sessions.rs:865-873`）——拒绝好过静默丢弃附件。

## 5. timeline 语义（由 `farwork-timeline` 定义）

契约原文见 `crates/farwork-timeline/src/lib.rs:13-27`，要点如下。

### 5.1 事件日志是密集的 `1..=cursor`

每提交一次变更恰好消耗一个事件序号，所以序列 `1..=cursor` 必然存在且不重复（`lib.rs:15-17`）。`order_seq` 是**另一条**单调分配的展示顺序，不要与事件序号混用。

### 5.2 游标是 `seq:N`

`Cursor` 的 wire 形式是 `seq:N`，`seq:0` 表示「还没看过任何事件」；上界是 `2^53-1`（`PROTOCOL_MAX_REVISION`），因为浏览器用 IEEE-754 双精度保存（`lib.rs:69-74`；`cursor.rs:10-42`）。上游 Python 服务端签发的 `seq:N` 同样可读——这是刻意保持的兼容点。

### 5.3 `content_hash` 覆盖 `{type,status,role,content}`

`state_hash` = SHA-256 over canonical JSON of `{type, status, role, content}`（`hash.rs:28-49`）。canonical JSON 规则：对象键排序、分隔符最小化、数字保留 JSON 拼写、非 ASCII 不转义、**数组顺序保留**——与 Python 的 `json.dumps(..., ensure_ascii=False, sort_keys=True, separators=(",",":"))` 对齐，因此同一 payload 在两侧得到同一哈希。

为什么不是 payload-only：一次只把 `running` 翻成 `done` 的写入 payload 完全不变，payload-only 哈希会把它判成「未变更」并静默丢弃——状态迁移就这么消失了（`hash.rs:28-35` 的注释即为此）。

### 5.4 `RefetchRequired`：宁可拒绝，不发部分 delta

`Timeline::events_after(cursor)` 要么返回**无缝后缀**，要么显式拒绝（`timeline.rs:396-435`）。`RefetchRequired` 的四种触发条件（`timeline.rs:36-69`）：

| 原因 | `reason` | 触发条件 |
| --- | --- | --- |
| `CursorAhead` | `cursor_ahead` | 客户端游标 > 已提交水头（换了实例/脏游标） |
| `TimelineReset` | `timeline_reset` | 游标 < `refetch_floor`（日志已重置或裁剪） |
| `IncompatibleItemChange` | `incompatible_item_change` | 条目 `type` 变了，delta 无法表达；upsert 时检测（`timeline.rs:743-757`） |
| `Requested` | `requested` | 运行时/运维显式要求重建 |

hub 侧的对应行为：`routes/sessions.rs:766-812` —— `after` 不可解析时同样回 `snapshotRequired:true`（不返回 422 让客户端特判）；`Recovery::RefetchRequired` 时返回 `events: []` + `snapshotRequired: true`，而**不是**空的「你已是最新」。

参考客户端就是这么做的：`apps/web/src/lib/session.svelte.ts:189-200` 用 `GET /events?after=<cursor>` 补拉，看到 `snapshotRequired` 立刻改走 `refreshSnapshot()`；WS 断开只做指数退避重连（`178-187`），**没有**任何 `/timeline` 轮询。

### 5.5 `SessionFence`：每会话独占写锁

`farwork-timeline/src/fence.rs:9-33`：异步、**按 session 分桶**的独占锁，包住「分配序号 → 落库 → 发布 → 推进水头」这一段，保证同一会话不会交叉写入。语义要点：

- **不可重入**：同一 task 里对同一会话取两次 fence 会死锁；组合写入要用 store 自己的提交锁（是另一把锁），而不是嵌套 fence。
- **按会话隔离**：慢会话不会阻塞别的会话。
- **只保证互斥，不保证公平**：waiters 的唤醒顺序不构成契约。

## 6. 持久化

- 迁移文件：`crates/farwork-hub/migrations/0001_init.sql`（唯一一个）。启动时由 `db::migrate` 通过 `sqlx::migrate!("./migrations")` 应用，sqlx 自带 ledger，重复启动是 no-op（`db.rs:17-22`；`main.rs:36-37`）。
- **8 张表**（`create table` 计数）：`users`、`app_settings`、`connectors`、`projects`、`sessions`、`session_runtime_states`、`timeline_items`、`ws_tickets`。
- **10 个索引**，其中两条是语义承载点：
  - `uq_timeline_items_client_message`：`unique (session_id, client_message_id)`（`0001_init.sql:132`）——`clientMessageId` 幂等的**唯一**保证。
  - `idx_timeline_items_session_updated_seq` / `_session_order_seq`：游标补拉与展示排序的查询路径（`0001_init.sql:129-130`）。
- `ws_tickets` 走 `delete ... returning` 消费，行即删，靠 `idx_ws_tickets_expires` 清理（`0001_init.sql:144`；`ws.rs:83-108`）。

## 7. 测试的前置条件（`tests/e2e.rs`）

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

M1 的可验证目标是「链路是否走得通」，设备表与 stream 映射放在进程内存（`HashMap` + 一把异步锁）。代价：重启丢设备记录与 `last_seen_ms`、无法水平扩展、没有历史数据。M2 起这些状态改由 PostgreSQL 承载（见 §6），`connected` 类易失字段不进表、由活跃连接推导的原则保持不变。
