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

- `crates/farwork-hub` — `/api/v2` 服务端：REST + `/dashboard/ws`、`/sessions/{id}/ws`、`/connector/ws`（connector ingress，Ingress Protocol v1）、`POST /ws-ticket`；PostgreSQL 持久化（2 个迁移、10 张表）；未实现路径统一回 `501`（`src/routes/unsupported.rs`，49 条）。ingress 的 hub 半边在 `src/ingress.rs` + `src/routes/connector_ws.rs`，审批在 `src/routes/notices.rs`。
- `crates/farwork-timeline` — timeline 的**纯域逻辑**：条目排序、`seq:N` 事件游标与补拉、`content_hash` 去重、`SessionFence` 每会话写栅栏。IO 无关（连接口都是 trait），可用内存后端单测。
- `crates/farwork-protocol` — **M1 遗留/待收敛**：M1 线协议类型，现仅被 `farwork-server` 与 `farwork-connector` 使用，hub 不依赖它。
- `crates/farwork-server` — **M1 遗留/待收敛**：7300 上的 M1 REST（`/api/health`、`/api/devices`）+ `/ws/connector`、`/ws/client` 终端骨架。已被 hub 取代，仅作终端探针保留。
- `crates/farwork-agent` — **M3 真正的 connector**：常驻进程，主动出站连 `/api/v2/connector/ws?token=...`；按 `session.start` 拉起 `codex app-server --stdio`，把 JSON-RPC 翻成 ingress 帧；自带 `--runtime echo` 假后端。身份落盘 `$FARWORK_HOME/agent.json`（0600，目录 0700）。入口 `src/main.rs`、CLI `src/cli.rs`、协议 `src/protocol.rs`、连接循环 `src/hub.rs`、codex 映射 `src/codex/`。
- `crates/farwork-connector` — **M1 遗留/待收敛**：远端常驻进程，按 `terminal.open` 起 PTY 并双向转发，只连 `farwork-server` 的 `/ws/connector`。**它不是 `farwork-agent` 的旧版本**：一个说终端帧协议、一个说 Ingress Protocol v1，不共享线协议也不共享代码路径，别把两者混为一次重命名。
- `apps/web` — SvelteKit 静态 SPA：登录、connectors/项目/会话、timeline 渲染、发消息、WS 增量；`src/lib/api/*`（REST 客户端）、`src/lib/session.svelte.ts`（会话与事件状态）、`src/lib/timeline.ts`（渲染辅助）是浏览器侧协议实现。
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
cargo run -p farwork-agent -- --token '<connectorToken>' --hub ws://127.0.0.1:7300
                                     # M3 设备侧：主动出站连 hub，按 session.start 拉起 codex app-server
cargo run -p farwork-agent -- --token '<connectorToken>' --runtime echo
                                     # 假后端：只跑协议与审批管道，不执行任何东西
cargo fmt --all -- --check           # CI 要过
cargo clippy --workspace --all-targets -- -D warnings   # CI 要过
TEST_DATABASE_URL=postgres://<user>@127.0.0.1:5432/farwork_test cargo test --workspace   # CI 要过
```

- `cargo test --workspace` **需要** `TEST_DATABASE_URL`。缺失（或为空）时 `crates/farwork-hub/tests/e2e.rs` 打印 `SKIP farwork-hub integration test: TEST_DATABASE_URL is not set.` 后直接返回——**结果是绿的，但 hub 的端到端链路一行都没验**。这就是 CI 必须挂 postgres service 的原因；本地也请照做。
- 测试会清空目标库的表（`tests/e2e.rs` 每次先 truncate）。只能指向一次性库（如 `farwork_test`），**不要指向开发库**。
- CI 的 `rust` job 用 `psql "$TEST_DATABASE_URL" -c 'select 1'` 先确认库存在，再跑测试；服务本身由 `postgres:18-alpine` service 提供。

启动 agent（M3）的三个事实：

- **token 从哪来**：先在 Web 或 REST 里 `POST /api/v2/connectors`（body 至少带 `name`），响应里的 **`connectorToken` 字段**就是它。明文**只在这一次响应里出现**，hub 只存 `sha256`；丢了只能 `POST /connectors/{id}/revoke` 重新签发。
- **等价环境变量**：`FARWORK_CONNECTOR_TOKEN`（`--token` 优先）、`FARWORK_CODEX_BIN`（默认 `codex`）、`FARWORK_HOME`（默认 `~/.farwork`）。其它参数：`--name`（默认主机名）、`--workdir`（会话默认工作目录，必须是绝对路径）、`--runtime codex|echo`、`--once`（连一次就退出，不重连）。
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

### 4.9 connector ingress（M3）四条硬约束

- **契约两侧同改**：ingress 帧的任何字段改动必须一次改完三处 —— `crates/farwork-hub/src/ingress.rs`（构造 + 解析）、`crates/farwork-agent/src/protocol.rs`（`ClientFrame` / `ServerFrame`）、`docs/architecture.md` §5 的两张帧表。只改一侧＝线上静默丢帧；现状就有一例（`session.message.clientMessageId`：hub 发 `null`、agent 要 `String`，省略该字段的 REST 调用者会让整轮被丢 —— Web UI 恒发该字段，所以现网页面暂时没暴露，见 `docs/architecture.md` §5.4）。
- **hub 不得乐观 resolve notice**：`POST .../notices/{id}/respond` 只负责下发 `notice.respond`；把 `status` 写成 `resolved` 的**唯一**入口是 connector 回报的 `notice.resolved`。hub 抢先置为已解决，一旦投递失败就会留下「已批准但没人执行」的假状态。
- **不得伪造 codex 的 `GrantedPermissionProfile`**：`item/permissions/requestApproval` 必须回 JSON-RPC error（`-32601`）并如实上报，绝不凭空构造一个用户没看过的授权对象。同理，任何地方都不许用假成功掩盖缺能力。
- **connector 断连必须让在途流收敛为 error**：socket 结束 → 关掉该 connector 全部未关流、把对应会话标 `error`（`status_reason = "connector disconnected"`）并广播，而不是把在途流静默丢弃或让它永远挂在 `running`。同理，没有活跃流时 `POST runtime/messages` 只落库并回 `runtimeDispatched:false`。

## 5. 当前状态与下一步

- **M2 纵向切片已交付**：登录 → connectors/项目/会话 → timeline 渲染 → 发消息 → WS 增量 → `seq:N` 游标补拉，全部走真实 PostgreSQL。
- **M3 部分交付**：connector ingress（`GET /api/v2/connector/ws`）+ 新 crate `farwork-agent` + 审批/中断（后端与协议），已用真二进制实测；`connectors.status` 现在有真实 `online`/`offline`。已验证/未验证的清单以 `docs/roadmap.md` 的 M3 为准，别在这里重新措辞。
- **还没有**：Web 审批卡、新建会话表单、文件浏览、终端、分享、管理台；以及 codex 的**成功路径**（assistant 文本增量、工具执行、真实审批往返）—— 本机无 Codex 凭据，未验证。对应的 REST 仍在 `crates/farwork-hub/src/routes/unsupported.rs` 里统一回 `501 not_implemented`（**49 条**；M3 移出了 interrupt / notices / notices respond 三条），其余未注册路径回 `404`。**不要把 501 说成「已支持」，也不要把「协议跑通」说成「agent 能干活」。**
- `snapshot.approvals` 与 `runtime/capabilities` 仍是空值：Ingress Protocol v1 没有对应帧，空着是诚实值，不是待填的占位符。
- 下一步顺序见 `docs/roadmap.md` 的 M3「下一批」：Web 审批卡 → 新建会话表单 → 文件浏览 → 终端 → 分享 → 管理台。
- App 属 M4，本轮不做；不要提前引入 Capacitor、移动平台目录或原生桥接代码。
- **已实测的边界事实**：M1 `farwork-connector` 的设备 id 每次进程启动重新生成（`crates/farwork-connector/src/main.rs:28-30`，`Uuid::new_v4()`），**未持久化**；M3 的 `farwork-agent` 已把身份落盘（`$FARWORK_HOME/agent.json`，文件 0600 / 目录 0700）。两者身份策略不同，别照 M1 的写法去改 agent。

## 6. 给 agent 的三条纪律

1. 先读 `docs/architecture.md` 再动协议。不要边写实现边发明字段——契约优先于手感。
2. 新增帧/字段的顺序是：域类型（`farwork-timeline` / `farwork-protocol` / ingress 帧）→ 文档（`docs/architecture.md`）→ 各方实现（hub、agent、web）。顺序反了就会产出两份互不兼容的真相。
3. 不引入新依赖（数据库驱动、前端 UI 框架、状态库、原生壳）而不在 PR 里说明理由与替代方案。
