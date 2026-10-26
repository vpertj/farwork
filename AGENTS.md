# AGENTS.md — Farwork 仓库级指令

> 给在这个仓库里干活的 AI agent（和接手的人）看的。动手前读完本文；协议与端点的唯一事实来源是 `docs/architecture.md`。

## 1. 项目与技术栈

Farwork = 跨设备 Agent 工作台：在浏览器里接管远端设备上的 Agent 会话，设备只需一个常驻 connector 主动出站，不需要公网 IP 或端口映射。

- Rust workspace（6 个 crate）：`farwork-hub`（`/api/v2` 服务端，axum + sqlx/PostgreSQL）、`farwork-timeline`（timeline 纯域逻辑）、`farwork-agent`（M3 的设备侧常驻进程，连 hub 并驱动 codex app-server）、以及 M1 遗留的 `farwork-server`、`farwork-connector`、`farwork-protocol`。
- Web：`apps/web`，SvelteKit + `@sveltejs/adapter-static`（`ssr = false`、`fallback: 'index.html'`）的静态 SPA，pnpm 管理，Svelte 5。
- 数据库：**PostgreSQL 是硬依赖**，hub 没有内存后端；`crates/farwork-hub/migrations/` 在启动时自动应用。
- Android App：计划用 **Capacitor** 包装**同一套**静态 Web 产物（`apps/web/build`），**尚未实现**，属 M4；iOS 复用同一产物。
- 上游关系：anywhere-labs/Agents-Anywhere 的独立重写，代码不共享；归属与致谢见 `NOTICE`。

## 2. 仓库地图（已用 `ls` 核实）

- `crates/farwork-hub` — `/api/v2` 服务端：REST + `/dashboard/ws`、`/sessions/{id}/ws`、`/connector/ws`（connector ingress，Ingress Protocol v1）、`/connectors/{id}/terminals/{terminalId}/stream`（设备终端流）、`POST /ws-ticket`；PostgreSQL 持久化（6 个迁移、12 张表、16 个索引；`0005_session_terminal_scope.sql` 只给 `terminals` 加可空 `session_id`，`0006_notice_interaction_type.sql` 只给 `session_notices` 加可空 `interaction_type`——两者都不建表、不加索引）；未实现路径统一回 `501`（`src/routes/unsupported.rs`，37 条）。ingress 的 hub 半边在 `src/ingress.rs` + `src/routes/connector_ws.rs`，审批在 `src/routes/notices.rs`，文件浏览中继在 `src/routes/fs.rs` + `src/fsrelay.rs`（按 `requestId` 配对，10s 超时 → `504`），设备终端中继在 `src/routes/terminals.rs` + `src/termrelay.rs`（按 `terminalId` 配对，表在 `migrations/0003_terminals.sql`，会话归属列在 `0005`）——**connector 命名空间与会话命名空间共用这两条中继**，会话级入口在 `src/routes/fs.rs` 的 `session_*` 与 `src/routes/terminals.rs` 的 `session_*`，分享在 `src/routes/shares.rs`（公开读 `GET /public/shares/{id}` 单独 mount、不取 `AuthedUser`，表在 `migrations/0004_shares.sql`），会话运行时控制在 `src/routes/runtime.rs`（capabilities / catalogs / commands / steer / selections；能力缓存是同级的 `src/runtime_caps.rs`，**进程内、按 session、不落库**），管理台在 `src/routes/admin.rs`（`GET`/`POST /admin/users`、`PATCH /admin/users/{id}`、`GET`/`PATCH /admin/settings`；权限门重读 `users` 行、响应七键白名单、三种 `409 admin_lockout`，见 `docs/architecture.md` §3.9）。集成测试在 `crates/farwork-hub/tests/`：`e2e.rs` / `connector_ingress.rs` / `fs_bridge.rs` / `terminal_bridge.rs` / `shares.rs` / `admin.rs`（第六个是管理台，`tests/admin.rs`）。
- `crates/farwork-timeline` — timeline 的**纯域逻辑**：条目排序、`seq:N` 事件游标与补拉、`content_hash` 去重、`SessionFence` 每会话写栅栏。IO 无关（连接口都是 trait），可用内存后端单测。
- `crates/farwork-protocol` — **M1 遗留/待收敛**：M1 线协议类型，现仅被 `farwork-server` 与 `farwork-connector` 使用，hub 不依赖它。
- `crates/farwork-server` — **M1 遗留/待收敛**：7300 上的 M1 REST（`/api/health`、`/api/devices`）+ `/ws/connector`、`/ws/client` 终端骨架。已被 hub 取代，仅作终端探针保留。
- `crates/farwork-agent` — **M3 真正的 connector**：常驻进程，主动出站连 `/api/v2/connector/ws?token=...`；按 `session.start` 拉起 `codex app-server --stdio`，把 JSON-RPC 翻成 ingress 帧；自带 `--runtime echo` 假后端。身份落盘 `$FARWORK_HOME/agent.json`（0600，目录 0700）。入口 `src/main.rs`、CLI `src/cli.rs`（`--workdir` / `--shell` / `--runtime` / `--once`）、协议 `src/protocol.rs`、连接循环 `src/hub.rs`、codex 映射 `src/codex/`、文件访问 `src/fsops.rs`（沙箱根 = `--workdir`）、PTY 宿主 `src/term.rs`（按 `terminal.open` 起 shell；沙箱同样只约束**起始目录**）、DSH 后端 `src/dsh/`（**九个**文件：`mod.rs` 主循环 / `client.rs` 方法封装 / `rpc.rs` 换行分帧与 `aa-<n>` id / `discovery.rs` 端点发现 / `handshake.rs` 握手与能力位 / `sync.rs` 批次·快照·checkpoint / `projection.rs` 帧投影 / `interaction.rs` 待答队列 / `selections.rs` catalog 短名解析；契约见 `docs/architecture.md` §10）。`tests/dsh_live.rs` 是**对本机真 DSH 的只读探测**，用 `#[ignore]` 关掉（不碰数据库、不建会话），跑法：`cargo test -p farwork-agent --test dsh_live -- --ignored --nocapture`；没有桥时它打印 `PROBE SKIP` 并正常返回，**CI 不跑**。
- `crates/farwork-connector` — **M1 遗留/待收敛**：远端常驻进程，按 `terminal.open` 起 PTY 并双向转发，只连 `farwork-server` 的 `/ws/connector`。**它不是 `farwork-agent` 的旧版本**：一个说终端帧协议、一个说 Ingress Protocol v1，不共享线协议也不共享代码路径，别把两者混为一次重命名。
- `apps/web` — SvelteKit 静态 SPA：登录、connectors/项目/会话、timeline 渲染、发消息、WS 增量、文件面板（`src/components/FilePanel.svelte`）、终端面板（`src/components/TerminalPanel.svelte` + `src/lib/terminal.ts`，xterm.js）、管理页（`src/components/AdminPage.svelte` + `src/lib/api/admin.ts`：用户目录/建号/改角色与停用/实例开关）；`src/lib/api/*`（REST 客户端）、`src/lib/session.svelte.ts`（会话与事件状态）、`src/lib/timeline.ts`（渲染辅助）是浏览器侧协议实现。
- `docs/architecture.md` — 当前架构、`/api/v2` 端点表、**Connector Ingress（§5，两个方向的帧表）**、timeline 语义、环境变量、M1 协议附录（唯一协议真相源）。
- `docs/roadmap.md` — M1–M4 里程碑与验收点（里程碑的唯一事实来源）。
- `.github/workflows/ci.yml` — CI 双 job：`rust`（fmt/clippy/test + `postgres:18-alpine` service）、`web`（install/check/build）。
- 根目录：`Cargo.toml`（workspace）、`rust-toolchain.toml`、`README.md`、`NOTICE`、`LICENSE`。
- 未落盘：`apps/mobile`（M4 的 Capacitor 工程，目前不存在；先别引用它）、本仓 `contracts/`（仓内尚未落库；上游 `/tmp/anywhere-survey/contracts/` 是存在的，见 4.2）。

## 3. 能跑的命令（单一事实来源，与 CI 一致）

工具链：Rust stable（见 `rust-toolchain.toml`）、Node 24、pnpm **11.19.0**（锁文件由该版本生成）、PostgreSQL。

Rust，在仓库根执行：

```bash
export DATABASE_URL='postgres://<user>@127.0.0.1:5432/farwork'   # 起 hub 必需，库必须已存在
cargo run -p farwork-hub             # 起 /api/v2 后端，默认 7300，启动时自动迁移
cargo run -p farwork-server          # M1 遗留探针，默认也是 7300 —— 不能与 hub 同时起
cargo run -p farwork-connector       # M1 遗留：以本机身份连 M1 server
cargo run -p farwork-agent -- --token '<connectorToken>' --hub ws://127.0.0.1:7300 --workdir /srv/work --shell /bin/bash
                                     # M3 设备侧：主动出站连 hub，按 session.start 拉起 codex app-server
                                     # --workdir 同时是「会话默认 cwd」、「文件浏览沙箱根」和「终端起始目录沙箱根」
                                     # --shell 是设备终端的默认 shell（缺省退到 $SHELL）
cargo run -p farwork-agent -- --token '<connectorToken>' --runtime echo
                                     # 假后端：只跑协议与审批管道，不执行任何东西
cargo run -p farwork-agent -- --token '<connectorToken>' --runtime dsh --workdir /srv/work
                                     # 连**本机已在运行**的 DSH bridge（需先在 DSH 里启用 agents-anywhere 桥插件）
                                     # 找不到 ~/.agents-anywhere/dsh-bridge/endpoint.json 时直接启动失败（退出码 1）
                                     # 可选：--dsh-endpoint <path> / --dsh-permission read-only / --dsh-model <名或 id>
cargo fmt --all -- --check           # CI 要过
cargo clippy --workspace --all-targets -- -D warnings   # CI 要过
TEST_DATABASE_URL=postgres://<user>@127.0.0.1:5432/farwork_test cargo test --workspace   # CI 要过
```

- `cargo test --workspace` **需要** `TEST_DATABASE_URL`。缺失（或为空）时 `crates/farwork-hub/tests/e2e.rs` 打印 `SKIP farwork-hub integration test: TEST_DATABASE_URL is not set.` 后直接返回——**结果是绿的，但 hub 的端到端链路一行都没验**。这就是 CI 必须挂 postgres service 的原因；本地也请照做。
- 测试会清空目标库的表（`tests/e2e.rs` 每次先 truncate）。只能指向一次性库（如 `farwork_test`），**不要指向开发库**。
- CI 的 `rust` job 用 `psql "$TEST_DATABASE_URL" -c 'select 1'` 先确认库存在，再跑测试；服务本身由 `postgres:18-alpine` service 提供。

启动 agent（M3）的关键事实：

- **token 从哪来**：先在 Web 或 REST 里 `POST /api/v2/connectors`（body 至少带 `name`），响应里的 **`connectorToken` 字段**就是它。明文**只在这一次响应里出现**，hub 只存 `sha256`；丢了只能 `POST /connectors/{id}/revoke` 重新签发。
- **等价环境变量**：`FARWORK_CONNECTOR_TOKEN`（`--token` 优先）、`FARWORK_CODEX_BIN`（默认 `codex`）、`FARWORK_HOME`（默认 `~/.farwork`）。其它参数：`--name`（默认主机名）、`--workdir`、`--runtime codex|echo`、`--once`（连一次就退出，不重连）。
- **`--workdir` 一物三用**：它既是 `session.start.cwd`（会话默认工作目录），也是**文件浏览的沙箱根**（`fsops.rs` 对根 `canonicalize()` 一次，之后每个 `fs.request` 都必须落在它下面），还是**设备终端的起始目录沙箱根**（`term.rs:275-287` 用同一套判定）。必须是绝对路径（否则启动失败，退出码 1）；**解析不出来时 agent 照常连 hub，但所有 `fs.request` 与 `terminal.open` 一律回 `permission_denied`**，绝不退化成无约束读。注意它约束的是终端**从哪开始**，不是 shell 起来之后能去哪。
- **`--shell` 只影响设备终端**：`terminal.open.shell` 优先，其次 `--shell`，最后 `$SHELL`（`cli.rs:45,92,124`）。与 codex 无关；`--runtime echo` 下终端照常可用。
- **身份文件**：`$FARWORK_HOME/agent.json`（默认 `~/.farwork/agent.json`），文件 0600、目录 0700，**不含 token**；删掉它会换一个新的 `agent_id`（重启不换 id 正是它与 M1 connector 的区别）。
- **退出码**：`0` = 正常退出（`--help`/`--version`/收到 `--once` 收尾）；`1` = 任何启动错误（缺 token、workdir 非绝对路径、身份文件损坏等）。`--once` 模式下连接失败也只记 warn 然后返回 `0` —— 想用退出码做健康检查就别用 `--once`。
- **让 `--runtime echo` 真的可用**：hub 侧必须带 `FARWORK_HUB_DEV_RUNTIMES=echo` 启动，否则 `create-and-start` 会以 `422 unknown runtime` 拒绝（默认允许集合不含 `echo`）。这是 dev-only 开关，生产不要设。

Web，在 `apps/web` 执行：

```bash
pnpm install --frozen-lockfile   # CI 要过
pnpm run check                   # svelte-kit sync + svelte-check
pnpm run build                   # vite build，静态产物到 apps/web/build
pnpm dev                         # vite dev --port 5173
pnpm run preview                 # 4173，预览静态产物（CI 不跑）
```

上面带 "CI 要过" 的命令与 `.github/workflows/ci.yml` 完全一致；提交前本地跑通。

### pnpm workspace 布局陷阱（已实测，别踩）

- `apps/web/pnpm-workspace.yaml` **存在**，内容只有两行：`allowBuilds:` / `  esbuild: true`；根目录**没有** `pnpm-workspace.yaml`。这是 pnpm 11 里 `allowBuilds` 唯一生效的位置。
- 副作用：**`apps/web` 自成 workspace root**。CI 的 `working-directory: apps/web` 正依赖这个布局。
- 别只加根的 `pnpm-workspace.yaml` 而不迁移：那会改变 `pnpm install` 在 `apps/web` 下的归属（包被提升到根、`apps/web` 退化为子包），现有布局与 CI 一起失效。
- 将来真要建根 workspace，必须两步同时做：把 `allowBuilds` 上移到根 `pnpm-workspace.yaml`，然后**删除** `apps/web/pnpm-workspace.yaml`。漏掉任一步，`pnpm install` 会以 `ERR_PNPM_IGNORED_BUILDS`（退出码 1）失败，CI 直接红。
- pnpm 版本必须与 CI 一致：**11.19.0**（见 `ci.yml` 的 `pnpm/action-setup`）。`apps/web/pnpm-lock.yaml` 由该版本生成，换版本可能改写锁文件。

## 4. 不可违反的约定

### 4.1 全仓禁止引入 Python

服务端是 Rust，前端是 TypeScript/Svelte。**本条无例外**：仓库内不得出现任何 `.py` 文件、Python 脚本、Python 依赖，也不得有**驱动 Python 工具链**的脚本或配置（uv / pip / poetry / uvicorn / alembic / pytest / ruff 等一概不许，含 lint、codegen、测试脚本）。需要脚本就用 shell / Rust / Node。

上游 Python 源码若存在，只允许作为本机 `/tmp` 下的**只读**对照材料（例如 `/tmp/anywhere-survey`），用于核对 wire 字段；本仓不得包含任何驱动它的脚本、配置或 CI 步骤，也不得把其中任何 Python 工具链带进本仓。

### 4.2 协议与 wire 字段以真实代码 + 上游源码为准

- 现行 wire 契约的唯一事实来源是 `docs/architecture.md` + `crates/farwork-timeline` 的类型（`item` / `event` / `cursor` 的 `serde` 重命名即字段名）。上游源码仅可作本机只读对照（如 `/tmp/anywhere-survey`），不随仓分发，也不是本仓的构建输入。
- **`contracts/` 的归属要说准**：**本仓没有** `contracts/` 目录（`find . -type d -name contracts` 无结果，仓内也没有任何 `*.schema.json`）；**上游有**，机器上通常位于 `/tmp/anywhere-survey/contracts/`，其中 `/tmp/anywhere-survey/contracts/protocol/1.0/schemas/`（13 个 schema：`event-envelope`、`session-snapshot-response`、`ws-ticket-request/response`、`capability-set` 等）是 Web 侧协议实现的对照来源，另有 `contracts/dsh-bridge/1.0/`、`runtime-control/2.0/`、`local-machine/`。
- 因此字段该信谁：**本仓行为以本仓代码 + `docs/architecture.md` 为准；与上游是否一致，去 `/tmp/anywhere-survey/contracts/` 对照确认**。不要凭记忆或凭本文件推断字段。将来的做法是把需要固化的 schema 落库到本仓 `contracts/`（届时以仓内副本为准），在落库之前仓内没有可引用的 schema 文件。
- 改一处必须同改其余面：`crates/farwork-timeline`（或 `farwork-protocol`）→ `crates/farwork-hub` → `apps/web/src/lib/api/types.ts` → `docs/architecture.md`。ingress 帧同理：`crates/farwork-hub/src/ingress.rs` ↔ `crates/farwork-agent/src/protocol.rs` ↔ `docs/architecture.md` §5。少一处即为破坏契约。

### 4.3 客户端禁止轮询 `/timeline`

客户端只允许：一次 `GET /sessions/{id}/snapshot` 建投影 → 订阅 `/sessions/{id}/ws` 收增量 → 断线后用 `GET /sessions/{id}/events?after=seq:N` 补拉。
`GET /sessions/{id}/timeline` 是**诊断/分页/历史**用的读接口（`mode=latest|changes|history`），**不得放进定时器轮询**：轮询会掩盖游标 bug、放大数据库压力，并让「增量是否正确」变成不可测。

### 4.4 `snapshotRequired: true` 必须整块重拉

`/events` 返回 `snapshotRequired: true` 意味着服务端**无法**给出无缝后缀（游标超前、日志已重置、条目类型变更）。此时客户端必须丢弃本地投影并重新 `GET /snapshot`，**不允许**把返回的 `events: []` 当成「已是最新」，也不允许拼接部分 delta 继续用。触发原因由 `RefetchReason` 给出（`cursor_ahead` / `timeline_reset` / `incompatible_item_change` / `requested`）。

### 4.5 `clientMessageId` 幂等由唯一索引保证

幂等的**唯一**保证是数据库：`timeline_items` 上的 `unique (session_id, client_message_id)`（`migrations/0001_init.sql`，`uq_timeline_items_client_message`）。代码里的「先查再写」只是快路径；并发下必须让唯一索引兜底，不要把幂等托付给内存状态或单进程假设。

### 4.6 `ws-ticket` 一次性，且必须校验 user + session 作用域

- 票据 60 秒 TTL，**读即删**（`delete ... returning`），重放不可用；不要在客户端缓存复用。
- 握手时必须同时校验：票据的 `user_id` 属于当前登录用户、票据 `scope` 与 URL 里 `session_id` 完全一致。任一不符即拒绝——只校验 user 或只校验 session 都是越权漏洞。

### 4.7 `content_hash` 覆盖 `{type,status,role,content}`

去重哈希是 SHA-256 over canonical JSON of `{type, status, role, content}`（`farwork-timeline/src/hash.rs`），**不是** payload-only：一次只把 `running` 翻成 `done` 的写入 payload 完全不变，payload-only 哈希会把它判成「未变更」并静默丢弃，状态迁移就消失了。改哈希输入＝改协议，必须同步上游口径与本仓文档。

### 4.8 其余不可违反项

- **base64**：M1 终端帧（遗留）里 `terminal.data` / `terminal.input` 的 `data` 恒为 **standard 字母表、带 `=` padding** 的 base64，按**字节**处理，禁止假定 UTF-8。
- **stream_id**：M1 终端链路中由发起方（浏览器）生成并全程复用；未知 `stream_id` 的帧丢弃即可，不要因此断开连接。
- **密钥**：`FARWORK_HUB_SECRET`、`FARWORK_HUB_SETUP_TOKEN`、`FARWORK_TOKEN` 只从环境变量读；`FARWORK_HUB_SECRET` 未设置会 warn 并回退到开发密钥（仅限开发）。禁止硬编码或提交任何密钥，`.env` / `.env.*` 不入库（`.env.example` 可入库）。
- **Rust 质量线**：`cargo fmt` 必过；clippy `-D warnings` 零告警。新写的错误路径用 thiserror/anyhow 收敛（workspace 已引入两者），不要新增 `unwrap()`/`expect()`（启动路径与测试除外）。
- **TS 质量线**：`strict: true`；禁止 `any`，外部输入一律按 `unknown` 收窄（参照 `apps/web/src/lib/api/types.ts` 的 `isRecord`/`asString` 风格）。
- **注释**：只解释非显然的「为什么」（取舍、坑、协议原因），不写逐行「是什么」。
- **提交门槛**：第 3 节命令本地跑通（含 `TEST_DATABASE_URL` 指向真实库）+ CI 两个 job 绿，才算完成。

### 4.9 connector ingress（M3）八条硬约束

- **契约两侧同改，且两侧都必须宽容**：ingress 帧的任何字段改动必须一次改完三处 —— `crates/farwork-hub/src/ingress.rs`（构造 + 解析）、`crates/farwork-agent/src/protocol.rs`（`ClientFrame` / `ServerFrame`）、`docs/architecture.md` §5 的帧表（§5.2/§5.3 是概览，§5.11 是设备终端的逐字段表）。只改一侧＝线上静默丢帧。
  - `session.message.clientMessageId` 是**可选字段**：hub 无值时不发该键（`ingress.rs:346-368`，`session_message_frame` 只在 `Some` 时插入 `clientMessageId`）；agent 用 `Option<String>` + `#[serde(default)]` 同时接受缺失与 `null`（`protocol.rs:352-364`）；hub 没给时 agent 本地生成 `cmid_<uuid>` 交给 codex `turn/start`，该 id **不回落 hub**，hub 侧幂等仍只靠 `unique (session_id, client_message_id)`（`hub.rs:337-353`）。
  - 两侧都**不许**把它收紧成必填或非 `null`：旧的「hub 发 `null`、agent 要 `String`」就是这样让整轮被当畸形帧丢掉（`hub.rs:268-277` 只 warn），而 hub 已落库并回了 `runtimeDispatched:true` —— 界面显示「已发送」、运行时毫无动静。这条经验是契约本身的理由：下游可能不传的字段，上游必须能省、下游必须能缺。细节见 `docs/architecture.md` §5.4；回归测试 `hub ingress::tests::session_message_frame_omits_an_absent_client_message_id`、`agent protocol::tests::session_message_parses_without_a_client_message_id`。
- **hub 不得乐观 resolve notice**：`POST .../notices/{id}/respond` 只负责下发 `notice.respond`；把 `status` 写成 `resolved` 的**唯一**入口是 connector 回报的 `notice.resolved`。hub 抢先置为已解决，一旦投递失败就会留下「已批准但没人执行」的假状态。
- **不得伪造 codex 的 `GrantedPermissionProfile`**：`item/permissions/requestApproval` 必须回 JSON-RPC error（`-32601`）并如实上报，绝不凭空构造一个用户没看过的授权对象。同理，任何地方都不许用假成功掩盖缺能力。
- **connector 断连必须让在途流收敛为 error**：socket 结束 → 关掉该 connector 全部未关流、把对应会话标 `error`（`status_reason = "connector disconnected"`）并广播，而不是把在途流静默丢弃或让它永远挂在 `running`。同理，没有活跃流时 `POST runtime/messages` 只落库并回 `runtimeDispatched:false`。
- **agent 的文件访问必须在 `--workdir` 沙箱内，且 workdir 不可解析时要拒绝而非放开**：每个 `fs.request` 的路径都要 `canonicalize`（跟符号链接）后按**路径组件**判 `starts_with(root)`（`crates/farwork-agent/src/fsops.rs:73-92`），符号链接逃逸、`../` 上跳、前缀陷阱（`/srv/work-evil` vs `/srv/work`）三类一律 `permission_denied`；`--workdir` 解析失败时 agent **不建沙箱**，此后所有 `fs.request` 回 `permission_denied`（`hub.rs:70-80,465-471`；`fsops.rs:187-191`），**绝不允许**退化成「读这台机器的一切」。`readText` 单文件硬上限 1 MiB，超限 `too_large` 而不是截断。契约见 `docs/architecture.md` §5.5–§5.6。
- **设备终端三件不许走样**：① `terminal.open.cwd` 必须走**同一套** `fsops` 沙箱判定（`term.rs:275-287` 调 `Sandbox::canonical_dir`，`fsops.rs:73-92`），越界就是 `permission_denied` 且**不发 `terminal.opened`**；沙箱只约束起始目录，不许对外承诺「终端被关在 workdir 里」。② `terminal.data` 一律 standard base64 带 padding，**按字节**处理（`term.rs:10-11`；hub 只校验不重写，`routes/terminals.rs:487-492`），任何一侧顺手解码再编码就是第二份独立实现的 bug 源头；`terminal.open` 的 `cwd`/`shell` 缺省时**不发键**（`ingress.rs:388-411`），`terminal.exit.code` 则**恒带键、可为 `null`**（`protocol.rs:302-314`）。③ `ws-ticket` 的 `scope {terminalId}` **必须绑定那一个 terminalId 且一次性**：铸票前校验归属（`ws.rs:154-162`）、握手时逐字比较 scope（`ws.rs:104-108`）、消费是 `delete … returning` 读即删（`ws.rs:87-114`）；A 终端的票不能换 B 终端的流。契约见 `docs/architecture.md` §5.11–§5.12。
- **能力必须来自 agent 上报，未上报即全 false；不得用默认 `true`，不得编造 catalog**：`GET /sessions/{id}/runtime/capabilities`、`.../runtime/catalogs/{catalog}`、`.../runtime/commands` 的唯一数据源是 agent 的 `session.capabilities` 帧（`docs/architecture.md` §5.13），hub 只缓存不推断（`crates/farwork-hub/src/runtime_caps.rs`）。三条落地要求：① 四个布尔与两份 catalog 全部 `#[serde(default)]`，**缺键读成 `false`／空表**，绝不缺省成 `true`；② 缓存为空、或会话已退出/connector 已断连（`connector_ws.rs:379-388,196-207`）时，REST 回**全 `false` + `revision:0` + `reportedAt:null`**——「没有能力信息」与「能力被收回」是同一个诚实答案（`routes/runtime.rs:45-62`）；③ **不许为了填满界面编一个模型/权限列表**（那是给用户一个点了没用的下拉框），也不许把 echo 的占位 catalog（`echo-fast`/`echo-slow`）当成产品能力。写接口只在**上报过** `false` 时才回 `501`，缓存为空时乐观下发（`runtime.rs:171-182,259-267`）——别把慢到的第一帧判成缺功能。
- **DSH 的能力与目录只能来自 bridge 上报；缺端点必须失败，不许假装**：`--runtime dsh` 是**本机已在运行**的 bridge 的客户端，生命周期与 codex 相反（codex 是 agent 起子进程，DSH 是 agent 去连一个 loopback 端点）。端点只读 `~/.agents-anywhere/dsh-bridge/endpoint.json`（回退 `$DSH_HOME/agents-anywhere/bridge/endpoint.json`），且必须 `version==1` / `host=="127.0.0.1"` / `port` 合法 / `token` 非空，任何一项不过都是拒绝拨号（`dsh/discovery.rs:137-183`）。**发现不了端点就是启动失败（退出码 1），不是「先连上 hub 再想办法」**（`main.rs:73-80`）——否则 hub 会照常接受 `session.start`，而会话什么也驱动不了。三条落地要求：① 能力位只认 `features`（字段**全部** `#[serde(default)]`，缺键即 `false`）与 `session.getCapabilities` 的条目；`commands` 恒为 `false`，因为本仓**没有实现 `session.executeCommand`**，桥报 `session.commands: true` 也不许透传（`dsh/projection.rs:248-283`）。② catalog 只来自 `catalog.listModels` / `catalog.listPermissions`，空表就报空表，不许编占位模型或权限。③ **sync 必须 ack、`batchSeq` 严格递增、乱序或跳号要重新订阅**（不是忽略，也不是从下一个号接着跑：`dsh/sync.rs:6-8,26`；`dsh/mod.rs:308,322,348`）；快照 `begin→items→commit` 暂存后才提交，条数与 id 唯一性都要校验（`dsh/sync.rs:152-277`）；checkpoint 按 native session 保存（`dsh/sync.rs:281-300`）。契约见 `docs/architecture.md` §10。

### 4.10 管理操作不得让实例失去最后一个 admin（并发下也要成立）

- **唯一判定入口是 `lockout_reason`**（`crates/farwork-hub/src/routes/admin.rs:144-167`）：`self_disable` / `last_admin` / `self_demote` 三种原因，全部映射成 `409`，body 恒为 `{"detail":{"code":"admin_lockout","reason":…,"message":…}}`（`admin.rs:169-181`）。别在 handler 里另写一套「还剩几个 admin」的判断——两套规则迟早分叉。
- **计数必须同一事务、同一把行锁**：`update_user` 先 `select id from users where role = 'admin' and disabled = false for update`（`admin.rs:337-344`），用这次读到的行数判定（`admin.rs:345-355`），然后才 `update … returning` 并提交（`admin.rs:357-373`）。**任何「先查后写」都是缺陷**：把 count 放到事务外、或先 count 再 update，两个并发降级会各自读到「2 个 admin」然后都成功，实例就此失去全部 admin。别拿单进程假设兜并发。
- **「活跃 admin」的定义只有一条**：`role = 'admin' and disabled = false`。被停用的 admin 不算——它既救不了场，也进不了 `/admin*`（权限门 `403`），所以 `last_admin` 与 `self_disable` 都按这个集合判。
- **权限门每次重读 `users` 行**（`admin.rs:74-85`）：不得用 token 里的角色或任何缓存做授权判断；被停用/降级的 admin 必须在**下一次请求**就失效。测试钉在 `crates/farwork-hub/tests/admin.rs:311-341,548-645`。
- **管理相关响应永远走 `user_view` 的七键白名单**（`admin.rs:102-112`）：`USER_COLUMNS` 里含 `password_hash`（连带 PBKDF2 salt），给 `UserRow` derive `Serialize` 就等于把哈希材料发布出去。加字段时改白名单，别动这条约束。
- **两个开关不等价**：`registrationOpen` 是**真开关**（关掉后 `register` `403`，`auth.rs:239-240`）；`emailVerificationRequired` 只落库并如实上报（`admin.rs:421-423` ↔ `auth.rs:122-124,192`），本 build 无邮件通道、`/auth/email-code` 仍 `501`，**它不拦注册**。别把后者写成已生效。

## 5. 当前状态与下一步

- **M2 纵向切片已交付**：登录 → connectors/项目/会话 → timeline 渲染 → 发消息 → WS 增量 → `seq:N` 游标补拉，全部走真实 PostgreSQL。
- **M3 计划项已全部交付**：connector ingress（`GET /api/v2/connector/ws`）+ 新 crate `farwork-agent` + 审批/中断（后端与协议），已用真二进制实测；`connectors.status` 现在有真实 `online`/`offline`。已验证/未验证的清单以 `docs/roadmap.md` 的 M3 为准，别在这里重新措辞。
- **文件浏览已交付（只读）**：ingress 帧 `fs.request` / `fs.response`（`docs/architecture.md` §5.5）+ REST `POST /connectors/{id}/fs/list`、`/fs/readText`（`routes/fs.rs`）+ agent 沙箱（`farwork-agent/src/fsops.rs`）+ Web 文件面板（`apps/web/src/components/FilePanel.svelte`）；沙箱边界与「workdir 不可解析即拒绝」见 §4.9 第五条。**会话命名空间的文件入口**（`POST /sessions/{id}/files/list|readText`）已补上，与 connector 级**共用同一条 `fsrelay`**（只多一层「由会话解析设备 + 相对路径锚在会话 `cwd`」，`routes/fs.rs:133-166`），别人的会话 `404`；写操作与大文件/二进制流式仍未实现。
- **设备终端已交付**：ingress 帧 `terminal.open` / `terminal.opened` / `terminal.data` / `terminal.resize` / `terminal.exit` / `terminal.error`（`terminal.error` 是本仓新增的一帧，理由见 `docs/architecture.md` §5.11）+ REST `POST /connectors/{id}/terminals` 与 WS `/connectors/{id}/terminals/{terminalId}/stream`（`routes/terminals.rs`，中继 `termrelay.rs`）+ agent PTY 宿主（`farwork-agent/src/term.rs`）+ 表 `migrations/0003_terminals.sql` + Web 终端面板（`apps/web/src/components/TerminalPanel.svelte`）。边界：cwd 沙箱只约束**起始目录**，`201` 只表示已落库并已下发（PTY 是否起来看 `terminal.opened`），拒绝原因落 `error_code`，见 §4.9 第六条与 `docs/architecture.md` §5.11–§5.12。
- **Web 侧已交付审批卡与新建会话表单**（`apps/web/src/components/ApprovalCard.svelte`、`NewSessionPage.svelte`，`#/new-session`）：卡片三按钮 + 「已下发，等待运行时确认」、失败可重试、`resolved` 变灰；中断按钮与状态原因行在 `SessionView.svelte`。口径以 `docs/roadmap.md` 的 M3「已验证」为准。
- **分享已交付**：三条 REST —— `POST /sessions/{id}/shares`、`GET /public/shares/{id}`（**免鉴权**）、`DELETE /sessions/{id}/shares/{share_id}`（`routes/shares.rs`）+ 表 `migrations/0004_shares.sql`。语义四条见 `docs/architecture.md` §3.8：快照在 per-session engine mutex 内拍（所以永远是已提交 head）、公开读只 `SELECT snapshot_json` 一列 + 七键白名单（`id/type/status/role/content/orderSeq/itemTime`）、免鉴权路由**单独 mount**（不在 `AuthedUser` 里）、删除是软删且公开读对已删的 `404`；空 timeline 分享回 `422 there is no content to share`。**不要把分享当成实时视图**：快照写一次就不再变。
- **管理台已交付**：三条 REST、5 个 method+path —— `GET`/`POST /admin/users`、`PATCH /admin/users/{id}`、`GET`/`PATCH /admin/settings`（`routes/admin.rs`）；语义与源码行号见 `docs/architecture.md` §3.9 与上面的 §4.10。**管理台只覆盖「用户目录 + 实例开关」**：`/agents*`、`/usage*`、`/announcements*`、`/pairing*` 仍回 `501`。
- **还没有**：只剩两件事。① codex 的**成功路径**（assistant 文本增量、工具执行、真实审批往返）—— 本机无 Codex 凭据，未验证；`file_change` / `permissions` 两种 notice kind 也从未在联调中出现。② **37 条**路径仍在 `crates/farwork-hub/src/routes/unsupported.rs` 里统一回 `501 not_implemented`（M3 移出 interrupt / notices / notices respond 三条 → 49，分享移出 `/sessions/{id}/shares` 与 `/sessions/{id}/shares/{*rest}` 两条 → 47，管理台移出 `/admin` 与 `/admin/{*rest}` 两条 → 45，会话命名空间与运行时控制再移出 8 条 —— `/sessions/{id}/terminal`、`/terminal/{*rest}`、`/files`、`/files/{*rest}` 与 `runtime/{steer,commands,selections,catalogs/{catalog}}` —— → 37），其余未注册路径回 `404`。**roadmap 上计划的功能块已全部交付**，剩下的就是这两件（codex 成功路径 + 37 条 `501` 的广度补齐）。**不要把 501 说成「已支持」，也不要把「协议跑通」说成「agent 能干活」。**
- `snapshot.approvals` 仍是 `[]`：可消费的审批形状走 `notices`，这个字段没有实现。`runtime/capabilities` **不再是空值**：它来自 agent 的 `session.capabilities`（Ingress Protocol v1.3，`docs/architecture.md` §5.13），未上报时回四布尔全 `false`（那是诚实值，不是待填的占位符）；但对着真 codex 只有 `interrupt` 为真，别把 echo 的四项全 `true` 当成产品能力。
- 下一步见 `docs/roadmap.md` 的 M3「下一批」：**计划的功能块已全部交付并划掉**（文件浏览、Web 审批卡与新建会话表单、设备终端、分享、管理台、运行中干预、会话命名空间）；剩下的不是新功能，而是 codex 成功路径（阻塞于本机无凭据）、37 条 `501` 的分组补齐（同步/接管 2、账号级分享 2、批量会话维护 3、设备运行时管理 8、OAuth/邮件码/扫码 14、公告/agents/usage/配对 8）与 M4。终端那段的原计划有两处修正：设备断连不再用 M1 的 `exit code -1`（改由 `terminal.error{code:"connector_offline"}` 表达），入口**先是** connector 命名空间、本轮再补上会话命名空间的单数两条（两者共用同一条 `termrelay`，见 `docs/architecture.md` §3.6）。
- App 属 M4，本轮不做；不要提前引入 Capacitor、移动平台目录或原生桥接代码。
- **已实测的边界事实**：M1 `farwork-connector` 的设备 id 每次进程启动重新生成（`crates/farwork-connector/src/main.rs:28-30`，`Uuid::new_v4()`），**未持久化**；M3 的 `farwork-agent` 已把身份落盘（`$FARWORK_HOME/agent.json`，文件 0600 / 目录 0700）。两者身份策略不同，别照 M1 的写法去改 agent。

## 6. 给 agent 的三条纪律

1. 先读 `docs/architecture.md` 再动协议。不要边写实现边发明字段——契约优先于手感。
2. 新增帧/字段的顺序是：域类型（`farwork-timeline` / `farwork-protocol` / ingress 帧）→ 文档（`docs/architecture.md`）→ 各方实现（hub、agent、web）。顺序反了就会产出两份互不兼容的真相。
3. 不引入新依赖（数据库驱动、前端 UI 框架、状态库、原生壳）而不在 PR 里说明理由与替代方案。

## 7. 并行工作的环境纪律（实测踩出来的，必须遵守）

本仓经常有多个 agent/多个人同时改不同 crate。以下三条不是建议，是事故复现条件：

1. **每个并行测试进程用自己的数据库**。`cargo test` 的 hub 集成测试会在 bootstrap 阶段 truncate 目标库；两路同时指向 `farwork_test` 时，对方的 setup token / 用户行被抹掉，表现为成批 `invalid or expired setup token` 假失败（真实发生过，一次 5 条红）。并行时用 `farwork_test_<话题>` 之类的一次性库，跑完 drop。
2. **子代理必须隔离 `FARWORK_HOME`**。默认 `~/.farwork/agent.json` 是**用户真实凭证**；任何测试（尤其是 "在 `~` 下真跑一遍" 这类验收）都必须 `FARWORK_HOME=/tmp/...`，否则会把用户正在用的设备凭证覆盖成指向测试桩的假地址——真实发生过两次，表现为「怎么改命令都连不上」。
3. **不要动别人正在用的端口，也不要 kill 不是自己起的进程**。`7300`（hub）与 `5173`（web dev）是演示/联调常驻口。测试一律用 `7400+`（临时后端）与 `5199+`（临时前端），收尾按 PID 收拾自己起的那几个。清理时用 `pkill -f '<自己的完整命令行>'`，不要 `pkill -f farwork-hub` 这种模糊模式。
