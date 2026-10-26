# Farwork 路线图

**当前**：M2 纵向切片已交付；**M3 全部计划项已交付** —— connector ingress + `farwork-agent` + 审批/中断、文件浏览、设备终端、分享、管理台，以及本轮的**运行中干预（Ingress Protocol v1.3：`session.capabilities` / `session.steer` / `session.selections`）与会话命名空间（`/sessions/{id}/files*`、`/terminal*`、`runtime/{capabilities,catalogs/{catalog},commands,steer,selections}`）** 都已落地并验收（各条的验收证据见 M3 的「已验证」）。**roadmap 上计划的功能块到此全部交付**；剩下的不是新功能，而是 `codex` 成功路径（阻塞于本机无凭据）与 **37 条** `501` 的广度补齐。技术栈既定：Rust（hub / connector）+ Svelte（web），数据库 PostgreSQL，移动 App 用 Capacitor 包装静态 Web 产物并后置到 M4。

里程碑状态一览：

| 里程碑 | 状态 |
| --- | --- |
| M1 可行走骨架 | **已被 M2 取代**，代码保留为终端探针（`farwork-server` / `farwork-connector` / `farwork-protocol`） |
| M2 `/api/v2` 纵向切片 | **已交付**（2026-10-26，commit `2079149`） |
| M3 ingress / 审批 / 文件 / 终端 / 分享 / 管理台 / 运行中干预 / 会话命名空间 | **已交付**：ingress、`farwork-agent`、审批与中断（后端 + 协议）已实测；Web 审批卡与新建会话表单已交付；文件浏览（只读）、设备终端、分享、管理台、运行中干预与会话命名空间全部交付并逐条验收（11/11、11/11、9/9、12/12、14/14） |
| M4 Capacitor Android App（其后 iOS） | 规划中 |

## M1 · 可行走骨架（已被 M2 取代，保留为终端探针）

commit `1504c42` 交付。链路是：`farwork-connector` 主动出站 → `farwork-server` 内存注册表 → 浏览器 `/ws/client` 打开真 PTY 终端。

**为什么保留**：PTY 转发（`portable-pty`）与 base64 字节帧这套实现是可复用的终端探针；M3 接终端时直接拿它改，而不是重写。

**为什么不再是主线**：设备与 stream 状态全在进程内存里（重启即丢、无法多副本），且和 hub 的 `/api/v2` + PostgreSQL 模型不兼容；`farwork-server` 与 `farwork-hub` **默认都监听 7300**，两者不能同时运行。

**收敛计划（M3 内完成）**：

- 终端能力并入 hub 的 `POST /sessions/{id}/terminal` 隧道（现在 `501`），M1 的 WS 帧形态降级为 hub 内部的 connector 通道帧。
- `farwork-protocol`（现仅被 server/connector 使用）要么迁进 hub 的 connector 帧模块，要么随 `farwork-server` 一起退场。
- 设备身份落盘：M1 connector 的 device id 每次进程启动重新生成（`crates/farwork-connector/src/main.rs:28-30`），接 ingress 前必须持久化。

M1 的契约细节保留在 `docs/architecture.md` 附录 A（标注为遗留）。

## M2 · `/api/v2` 纵向切片（已交付 2026-10-26，commit `2079149`）

**目标**：第一次用真实 PostgreSQL 跑通「一个会话的完整读写回路」，而不是继续堆骨架。

### 已交付范围

- **认证**：`/api/v2/auth/*`——PBKDF2 口令、首个账号 setup token 引导、注册开关。
- **connectors（设备）/ 项目 / 会话**：REST CRUD + 分页游标。
- **timeline**：`GET /sessions/{id}/snapshot` 建投影；`seq:N` 事件游标补拉；条目按 `order_seq` 排序渲染。语义由新 crate `farwork-timeline`（纯域逻辑）承担，hub 只做持久化与传输。
- **发消息**：`POST /sessions/{id}/runtime/messages` 持久化到 `timeline_items`，`clientMessageId` 由唯一索引保证幂等。
- **WS 增量**：`POST /ws-ticket` 换一次性票据 → `/sessions/{id}/ws` 事件流 / `/dashboard/ws` 总览；`snapshotRequired` 时客户端整块重拉。
- **诚实兜底**：52 条未实现路径统一 `501 not_implemented`，未注册路径 `404`；`GET /connectors` 的 `status` 当时恒为 `offline`（没有 ingress 就没有在线值）——这条已被 M3 取代：`status` 有真实 `online`/`offline`；`501` 表现在是 **37 条**（M3 移出 3 条 → 49，分享再移出 2 条 → 47，管理台再移出 `/admin` 与 `/admin/{*rest}` 两条 → 45，会话命名空间与运行时控制再移出 8 条 → 37）。
- **CI 硬化**：`rust` job 挂 `postgres:18-alpine` service 并注入 `TEST_DATABASE_URL`，让 hub 集成测试从「静默 SKIP」变成真跑。

### 已验收

- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（带 `TEST_DATABASE_URL`）全绿。
- `apps/web` 的 `pnpm run check`、`pnpm run build` 通过。
- 端到端旅程（`crates/farwork-hub/tests/e2e.rs`）：`/healthz` → 首个 admin 注册（setup token）→ 登录 → 建 connector / 项目 / 会话 → snapshot → 发消息 → `events?after=seq:N` 补拉 → `/sessions/{id}/ws` 收到增量。
- 重复 `clientMessageId` 不再产生新条目（`WriteOutcome::Unchanged`，无新事件）。

### 明确未含（当时都是 `501`）

connector ingress · 真 agent 执行 · 审批与中断 · 文件浏览 · 终端 · 分享 · 管理台（admin / agents / usage / announcements / pairing）。其中 connector ingress、审批/中断、文件浏览与设备终端已由 M3 交付（见下），分享也已交付，管理台交付的是**用户目录 + 实例开关**这一块；同条列里的 `/agents*`、`/usage*`、`/announcements*`、`/pairing*` 四组没有随这一批交付，仍回 `501`。

## M3 · connector ingress + agent（**计划项已全部交付**）

目标没变：让 hub 真的连上一台设备、真的把会话交给运行时。本轮先交付**协议与后端**，随后 Web 侧补上了审批卡、新建会话表单、会话中断与状态原因行。

### 已交付

- **connector ingress**：`GET /api/v2/connector/ws?token=...`（Ingress Protocol v1，契约见 `docs/architecture.md` §5）。hello → welcome → 心跳 → 断连收敛；`connectors.status` 从「恒为 `offline`」变成真实的 `online`/`offline`。
- **`farwork-agent`（新 crate）**：主动出站连 hub；身份落盘在 `$FARWORK_HOME/agent.json`（文件 0600、目录 0700，重启不换 id）；按 `session.start` 拉起 `codex app-server --stdio` 并把 JSON-RPC 翻成 ingress 帧；自带 `--runtime echo` 假后端用于自测。
- **会话真派发**：`POST /sessions/create-and-start` 会下发 `session.start`（首条消息走 `prompt`）；`POST /sessions/{id}/runtime/messages` 在会话有活跃流时把 `session.message` 推给 agent，响应里的 `runtimeDispatched` 才是真实投递结果。
- **审批与中断（后端）**：`GET /sessions/{id}/runtime/notices`、`POST /sessions/{id}/notices/{id}/respond`（+ `runtime` 别名）、`POST /sessions/{id}/runtime/interrupt` 落地；新增迁移 `0002_connector_ingress.sql`（`session_streams` 让 `streamId` 跨 hub 重启存活，`session_notices` 让审批跨刷新存活）。
- **文件浏览（只读）**：新增 ingress 帧 `fs.request` / `fs.response`（hub 侧 `crates/farwork-hub/src/ingress.rs` + 配对表 `crates/farwork-hub/src/fsrelay.rs`，agent 侧 `crates/farwork-agent/src/protocol.rs` + 沙箱 `crates/farwork-agent/src/fsops.rs`），REST 两条 `POST /connectors/{id}/fs/list`、`POST /connectors/{id}/fs/readText`（`crates/farwork-hub/src/routes/fs.rs`），Web 侧会话页文件面板（`apps/web/src/components/FilePanel.svelte`，API 是 `apps/web/src/lib/api/workspace.ts` 的 `listWorkspaceDir` / `readWorkspaceText`）。**沙箱根是 agent 的 `--workdir`**：`canonicalize` 后按路径组件判前缀，越界/符号链接逃逸一律 403，`workdir` 不可解析时直接拒绝（契约见 `docs/architecture.md` §5.5–§5.6）。`501` 表**当时没有变化**，仍是 49 条：这批入口本来就不在那张表里（表里的 `/sessions/{id}/files*` 是另一组，后来在「会话命名空间」那一批补上了）。
- **设备终端（M3）**：新增 ingress 帧 `terminal.open` / `terminal.opened` / `terminal.data` / `terminal.resize` / `terminal.exit` / `terminal.error` / `terminal.close`（hub 侧 `crates/farwork-hub/src/ingress.rs:385-432,510-540`、中继 `crates/farwork-hub/src/termrelay.rs`、agent 侧 `crates/farwork-agent/src/protocol.rs:289-321,384-415` + PTY 宿主 `crates/farwork-agent/src/term.rs`），REST 两条 `POST /api/v2/connectors/{id}/terminals`（`crates/farwork-hub/src/routes/terminals.rs:45,85-173`，201 恰好五个键）与 `GET /api/v2/connectors/{id}/terminals/{terminalId}/stream`（同文件 `:46-49,224-383`，浏览器侧 `terminal.input`/`terminal.resize`/`terminal.close` ↔ `terminal.opened`/`terminal.data`/`terminal.exit`/`terminal.error`），迁移 `0003_terminals.sql`（`terminals` 表：`closed_at` 区分「已结束」与「从未存在」，`error_code`/`error_message` 让 agent 的拒绝原因活过它被报告的那一刻），Web 侧会话页终端面板（`apps/web/src/components/TerminalPanel.svelte`、`apps/web/src/lib/terminal.ts`、`apps/web/src/lib/api/workspace.ts:191-238`；xterm.js）。**入口是 connector 命名空间**（这是当时的边界；`/sessions/{id}/terminal*` 后来在「会话命名空间」那一批补齐，见下文）。契约见 `docs/architecture.md` §5.11–§5.12。
- **分享**：三条 REST —— `POST /sessions/{id}/shares`、`GET /public/shares/{id}`（**免鉴权**）、`DELETE /sessions/{id}/shares/{share_id}`（`crates/farwork-hub/src/routes/shares.rs`），迁移 `0004_shares.sql`（`snapshot_json` 存一次写死、`deleted_at` 软删）。语义：① 快照在 per-session engine mutex 内拍，所以永远是已提交 head；② 公开读只 `SELECT snapshot_json` 一列，条目走七键白名单（`id`/`type`/`status`/`role`/`content`/`orderSeq`/`itemTime`），`connectorId`/`workspacePath`/`cwd`/`sessionId`/`source`/`hash` 从不进快照；③ 免鉴权路由**单独 mount**，不在 `AuthedUser` 里；④ 删除是软删，公开读对已删的返回 `404`。契约与源码行号见 `docs/architecture.md` §3.8。
- **管理台（实例管理）**：三条 REST、展开成 5 个 method+path —— `GET`/`POST /admin/users`（用户目录 + 建号）、`PATCH /admin/users/{id}`（改 `role`/`disabled`/`displayName`）、`GET`/`PATCH /admin/settings`（实例开关）；入口 `crates/farwork-hub/src/routes/admin.rs`，集成测试 `crates/farwork-hub/tests/admin.rs`，Web 侧 `apps/web/src/components/AdminPage.svelte` + `apps/web/src/lib/api/admin.ts`。语义三条：① 权限门**重读 `users` 行**判 `role = admin`（被停用的 admin 也 `403`）；② 响应是七键白名单投影（`id`/`email`/`displayName`/`role`/`disabled`/`createdAt`/`emailVerified`），哈希材料永不出现在响应里；③ **不锁死自己**——`self_disable` / `last_admin` / `self_demote` 三种 `409 admin_lockout`，活跃 admin 数在**同一事务内** `select … for update` 取，并发降级不可能都成功。两个开关的边界必须分清：`registrationOpen` 是**真开关**（关掉后 `register` 真的 `403`），`emailVerificationRequired` **只落库并如实上报**（本 build 无邮件通道、`/auth/email-code` 仍 `501`），**不拦注册**。`501` 表由 47 → 45。契约见 `docs/architecture.md` §3.9。
- **运行中干预（Ingress Protocol v1.3）**：新增三帧 `session.capabilities`（上行）/ `session.steer`、`session.selections`（下行）；hub 侧 `crates/farwork-hub/src/ingress.rs:376-404,520-532` + 进程内缓存 `crates/farwork-hub/src/runtime_caps.rs` + 分发 `crates/farwork-hub/src/routes/connector_ws.rs:415-439`（清缓存在 `:379-388,196-207`），agent 侧 `crates/farwork-agent/src/protocol.rs:242-257,331-338,441-466` + 分发 `hub.rs:357-391`，两个后端的真实上报 `echo.rs:25-68`（四项全 true + 占位 catalog）与 `codex/mod.rs:20-38`（只有 interrupt）。REST 五条：`GET .../runtime/capabilities`、`GET .../runtime/catalogs/{catalog}`（未知 422）、`GET .../runtime/commands`、`POST .../runtime/steer`（202）、`PATCH .../runtime/selections`（转发成功才 200），入口 `crates/farwork-hub/src/routes/runtime.rs` + `sessions.rs:937-951`。Web 侧：会话页的补充指令框（`state=running` 且有 `steer` 能力时才出现）、模型/权限选择（有 catalog 时才显示）、斜杠命令建议（`apps/web/src/lib/commands.ts`）。契约见 `docs/architecture.md` §3.10 与 §5.13。
- **会话命名空间**：`POST /sessions/{id}/files/list|readText`、`POST /sessions/{id}/terminal`、`GET /sessions/{id}/terminal/{terminalId}/stream` —— **与 connector 级共用同一条 `fsrelay`/`termrelay`**（没有第二套中继），只多一层「由 session 解析出 connector + workspace」；终端行绑定 `session_id`（`migrations/0005_session_terminal_scope.sql:10-11` 在既有 `terminals` 表上加可空列，没建第二张表）；别人的会话一律 `404`。Web 侧文件面板与终端面板已切到会话命名空间（`apps/web/src/lib/api/workspace.ts:154,170,204,232`）。语义见 `docs/architecture.md` §3.5/§3.6。
- **诚实兜底更窄了**：`501` 表从 52 条减到 49 条（移出 interrupt / notices / notices respond 三条），分享交付后再减 2 条（`/sessions/{id}/shares`、`/sessions/{id}/shares/{*rest}`）→ 管理台再移出 2 条（`/admin`、`/admin/{*rest}`）→ 45；本轮会话命名空间与运行时控制再移出 8 条 → **37 条**。REST 与 ingress 两侧仍然「宁可变窄，不伪造成功」。
- **Web 审批卡**（`apps/web/src/components/ApprovalCard.svelte`）：三个决定按钮 `approve` / `approve_for_session` / `reject`（`:75-93`）；点击后状态先落在「已下发，等待运行时确认」（`:24-35,101`），卡片变灰**只**取决于服务端 `notice.status === 'resolved'`（`:22,25`）；投递失败可点「重试」（`:96-97`）。
- **Web 新建会话表单**（`apps/web/src/components/NewSessionPage.svelte`，路由 `#/new-session[/:projectId]`，`apps/web/src/lib/router.ts:26-53`）：一次填 connector + 项目 + runtime + 首条消息；`connectorResult.ok` 为真则跳进会话页（`NewSessionPage.svelte:93`），为假则如实显示「设备未连接：指令未下发」（`:105,213`）+ 返回码（`:222`）+ 已创建会话 id（`:218`）。
- **Web 中断与状态原因**（`apps/web/src/components/SessionView.svelte`）：中断按钮不本地改状态，只显示「中断指令已下发（stream …），等待运行时回推状态更新」（`:116-119,135-137`）；会话 `statusReason` 单独一行「状态原因：…」（`:129-130`，例如 `connector disconnected`）。

### 已验证（有证据）

1. **真二进制全链路（codex，失败路径）**：hub + `farwork-agent`（真 codex 二进制）跑通 `connectors.status → online` → `create-and-start` → agent 收到 `session.start` → codex 子进程启动 → codex 因**本机无凭据返回 401** → 这个错误**如实落进 timeline**（一条 `system`/`failed` 条目，不是只写日志）→ 杀掉 agent 后 `connectors.status` 回落 `offline`。
2. **审批管道往返（echo 假后端）**：hub + `farwork-agent --runtime echo`（需 `FARWORK_HUB_DEV_RUNTIMES=echo`，默认关闭，启动会打 WARN）跑通完整一轮：`create-and-start` 下发 → agent 产生 `command_execution` notice（`status: open`）→ 会话状态 `waiting_approval` → `POST .../notices/{id}/respond` 带 `approve` 返回 200 → connector 回报 `notice.resolved{decision:approve}` → 会话回 `idle`；杀掉 agent 后 `connectors.status` 回落 `offline`。
   - 注意这条验证的是**审批管道**，不是真实 codex 审批。echo 后端不执行任何东西，它只是把契约跑一遍（`crates/farwork-agent/src/echo.rs`）。
3. **Web 交付的静态验收（仓库所有者验收）**：`apps/web` 的 `pnpm run check` 0 error、`pnpm run build` 通过；审批卡不乐观 resolve 在代码层确认——`POST .../notices/{id}/respond` 的分支只返回 `ok` + 仍是 `open` 的 `notice.view()` + `serverTime`（`crates/farwork-hub/src/routes/notices.rs:333-340`），把 `status` 写成 `resolved` 的 SQL 只在 connector 回报的 `resolve_notice` 里（`notices.rs:159-169`）；前端同样只在 `notice.status === 'resolved'` 时变灰（`apps/web/src/components/ApprovalCard.svelte:22,25`）。
4. **文件浏览 11/11 断言通过（仓库所有者验收）**：`crates/farwork-hub/tests/fs_bridge.rs`（假 connector + 真 PostgreSQL）跑通 `fs/list` 成功、`fs/readText` 成功，并逐条断言越权与异常：根外路径（`/etc/passwd` 这类）→ `403`、`../` 上跳 → `403`、指向根外的符号链接 → `403`、文件缺失 → `404`、相对路径 → `422 invalid_path`、并发两个请求按 `requestId` 配对不串号（`fs_bridge.rs:545-560`）、agent 停掉后 → `503 connector_offline`（`fs_bridge.rs:385-395`）。三条越权用例的判定在 agent 沙箱单测里（`crates/farwork-agent/src/fsops.rs:320-425`，另有一条前缀陷阱 `fsops.rs:381-402`）；REST 侧 `permission_denied → 403` 的映射断言在 `fs_bridge.rs:497-535`（由假 connector 回 `permission_denied`），同一处还断言了 `404 not_found`、`413 too_large`、`502 io_error`，`504 fs_timeout` 在 `fs_bridge.rs:630-650`。
5. **设备终端 11/11 验收通过（仓库所有者验收）**：hub + `farwork-agent` 真二进制跑通整条终端链路，逐项过：
   1. **整行匹配输出**：键入 `echo …` / `uname` 后回读的整行与预期逐字节相同（不是「看起来像」）；base64 与字节语义在协议层另由集成测试断言两次往返且非 UTF-8 字节（`0x00`/`0xff`）原样存活（`crates/farwork-hub/tests/terminal_bridge.rs:509-550`）。
   2. **resize 生效**：`terminal.resize{cols,rows}` 到达设备并被 PTY 采纳，回执证明帧真的走完来回（`terminal_bridge.rs:566-588`；agent 侧 `crates/farwork-agent/src/term.rs:310-314`，PTY resize 单测 `term.rs:638` 起）。
   3. **exit 收尾**：`terminal.close` → `terminal.exit`，回读 `code`，socket 在 exit 帧后立即结束、行被标 `closed_at`（`terminal_bridge.rs:590-604`）。
   4. **cwd 越界 → `permission_denied` 且不发 `terminal.opened`**：`cwd` 落在 `--workdir` 外时设备回 `terminal.error{code:"permission_denied"}`（带设备原话），流上**没有** `terminal.opened`；原因写进表，晚取票的浏览器仍能看到（`terminal_bridge.rs:669-706`；判定 `crates/farwork-agent/src/term.rs:275-287`、`crates/farwork-agent/src/fsops.rs:73-92`）。
   5. **票据重放被拒**：同一张 `{terminalId}` 票用第二次 → `401`（读即删，`crates/farwork-hub/src/routes/ws.rs:87-114`；`terminal_bridge.rs:606-614`）。
   6. **票据跨终端被拒**：A 终端的票去连 B 终端 → `401`（scope 逐字比较，`ws.rs:104-108`；`terminal_bridge.rs:616-627`）。
   7. **未知 / 已删除的 terminalId**：socket 允许 upgrade，先发 `terminal.error{code:"terminal_not_found"}` 再关，不是裸握手失败（`crates/farwork-hub/src/routes/terminals.rs:253-265`；`terminal_bridge.rs:642-667`）。
   8. **杀掉 agent 后 `connectors.status` 回落 `offline`**（手工验收，真二进制），且未关终端被推 `terminal.error{code:"connector_offline"}` 并关行（同一收敛逻辑 `terminals.rs:592-614`；`crates/farwork-hub/src/routes/connector_ws.rs:176-193`；集成测试用断开假 connector 覆盖：`terminal_bridge.rs:732-762`）。
   9. **失败不冒充成功**：connector 离线时接流 → `503`、开终端 → `503 code=connector_offline`、未知 connector → `404 code=connector_not_found`、`cols=0` → `422 code=invalid_size`、相对 `cwd` → `422 code=invalid_path`（`crates/farwork-hub/src/routes/terminals.rs:91-115,205-222,245-251`；`terminal_bridge.rs:384-414,770-783`）。
   10. **没有残留 PTY**：浏览器断开时 hub 补发 `terminal.close`、agent 侧 `close_all` 收掉全部 shell，进程退出后无孤儿 shell（`terminals.rs:369-381`；`term.rs:250-260,756`）。
   11. **201 不冒充成功**：响应终端对象恰好五个键、无 `status`；`201` 仅表示已落库 + 已下发（`terminals.rs:73-83,156-165`；`terminal_bridge.rs:439-446`）。
   口径以 `docs/architecture.md` §5.11–§5.12 为准。上表来自仓库所有者的真二进制手工验收（hub + 真 `farwork-agent`）；协议层与拒绝路径另有集成测试 `crates/farwork-hub/tests/terminal_bridge.rs`（假 connector + 真 PostgreSQL）逐条断言，行号见各条。**未覆盖的**：浏览器页面里手动敲字的手工记录没有留存（面板本身的构建验收同第 3 条）。
6. **`session.message.clientMessageId` 契约缺陷已修复**：hub 无值时不发该键（`crates/farwork-hub/src/ingress.rs:346-368`，回归测试 `ingress::tests::session_message_frame_omits_an_absent_client_message_id`，`ingress.rs:774-793`）、agent 收 `Option<String>` 兼容缺失与 `null`（`crates/farwork-agent/src/protocol.rs:358-364`，回归测试 `protocol::tests::session_message_parses_without_a_client_message_id`，`protocol.rs:582-597`）、agent 本地生成 `cmid_<uuid>` 供 `turn/start`（`crates/farwork-agent/src/hub.rs:337-353`）。契约口径见 `docs/architecture.md` §5.4。
7. **分享 9/9 通过（仓库所有者验收）**：`crates/farwork-hub/tests/shares.rs`（假 connector + 真 PostgreSQL）跑通整条旅程，逐项过：① 会话级分享 `201`、share id 形如 `shr_*`、`scope` 回 `session`（`shares.rs:296-311`）；② **公开读不带 Authorization 也 `200`**，`sessionTitle` 与条目数与创建时的 timeline 一致（`shares.rs:330-355`）；③ **快照不可变、逐字节相同**：分享之后往原会话再发一条消息，公开页读回的 items 与首次完全相同（`shares.rs:359-395`）；④ 消息级分享恰好一条（`shares.rs:430`）；未知 `messageId` → `404`、`scope:"message"` 缺 `messageId` → `422`、未知 `scope` → `422`（`shares.rs:316-325,443,453`）；⑤ **敏感字段零命中**：直接对**裸 body** 断言不含 `sessionId`/`source`/`runtime`/`contentHash`/`updatedSeq`/`connectorId`/`workspacePath` 等（`shares.rs:238-262,357,395`）；⑥ **陌生人对别人的会话建分享 → `404`**，同一账号读别人的 timeline 也是 `404`（`shares.rs:461-487`）；⑦ **免鉴权的公开读 `200`，但同一串垃圾 token 打私有端点仍 `401`**（`shares.rs:497-523`）；⑧ 未知 share id → `404`（`shares.rs:526-534`）；⑨ **删除后 `404`**：owner `DELETE` → `204` 且无 body，公开读 `404`，重复删除 `404`，同会话的另一个 share 仍可读（`shares.rs:536-602`）。

8. **管理台 12/12 通过（仓库所有者验收）**：`crates/farwork-hub/tests/admin.rs`（真 PostgreSQL）把下面的清单逐条断言成测试，行号即证据：① **匿名 `GET /admin/users` → `401`**（`tests/admin.rs:246-300`）；② **`member` 的 token → `403 forbidden`**（同处断言 `detail_code() == "forbidden"`）；③ **列表每项恰好七键**、且对裸 body `grep` 不到 `pbkdf2`/`password`/`salt`/`hash`（`tests/admin.rs:439-534`，词表 `:461-463`）；④ **建号 `201`**，回 `user` 的键正好那七个、`emailVerified: false`（`tests/admin.rs:342-400`）；⑤ **重复邮箱 `409`**（大小写归一后命中唯一索引，`tests/admin.rs:402-414`）；⑥ **自停用 → `409 code=admin_lockout reason=self_disable`**（`tests/admin.rs:548-570`）；⑦ **自降级 → `409`**：只剩一个 admin 时报 `last_admin`、两个 admin 时报 `self_demote`（`tests/admin.rs:571-610`）；⑧ **被拒之后 admin 仍可用**：`GET /auth/me` 仍 `role=admin, disabled=false`，列表照常 `200`（`tests/admin.rs:588-645`）；⑨ **`PATCH /admin/settings` 立刻被 `/auth/config` 反映**（`registrationOpen:false`、`emailVerificationRequired:true`，`tests/admin.rs:700-731`）；⑩ **关掉注册后 `POST /auth/register` → `403`**（`tests/admin.rs:733-757`）。所有者报告的 12 条里另两条落在同一文件、不在上面这份清单中：停用 member 后它的请求立刻 `403`（`:660-684`）、停用 admin 后那个 admin 的**下一次请求**即 `403`（`:311-341`）。纯函数侧另有 `admin.rs:433-475` 两条单测（`lockout_reason` 的六种组合、非法 role）与分页/坏游标断言（`tests/admin.rs:496-546`）。**本轮没有重跑 `cargo test`**，上面的绿来自仓库所有者的验收记录。

9. **运行中干预 + 会话命名空间 14/14 通过（仓库所有者验收）**：hub + `farwork-agent`（`--runtime echo`，需 `FARWORK_HUB_DEV_RUNTIMES=echo`）跑通，逐项过：
   1. **能力随 agent 上报**：`session.capabilities` 一到，`GET /sessions/{id}/runtime/capabilities` 的 `capabilitySet` 四项布尔与 agent 报的一致（echo → 四项全 `true`，`codex/mod.rs:20-38` 的 codex 则只有 `interrupt:true`）。
   2. **`revision` 随上报自增**：每次接受一帧 +1（`runtime_caps.rs:163-187`），首帧为 `1`。
   3. **未上报 = 全 false**：缓存为空的会话回 `{steer:false, interrupt:false, selections:false, commands:false}` + `revision:0` + `reportedAt:null`——**不是全 `true`**（`runtime.rs:45-62`）。
   4. **catalog 有内容**：`GET .../runtime/catalogs/model` 回 echo 的两条占位模型（`echo-fast` / `echo-slow`），`GET .../runtime/catalogs/permission` 回权限项。
   5. **未知 catalog → `422 code=unknown_catalog`**（例如 `catalogs/models`），不是空列表（`runtime.rs:72-79`）。
   6. **commands 有内容**：`GET .../runtime/commands` 回该运行时的命令清单（`{name, description?}`）。
   7. **steer 202**：`POST .../runtime/steer {content}` 回 `202 {delivered:true, streamId}`（`runtime.rs:195-202`）。
   8. **timeline 出现该条**：steer 之后 timeline 里出现这条指令（echo 把它写成 user 条目 + assistant 回执，`echo.rs:179-186`）——`202` 说的是「已投递」，可见性由 timeline 证明。
   9. **selections 200**：`PATCH .../runtime/selections {model}` 回 `200 {selections}`，并把接受的键并进 `session_runtime_states.selections_json`（`runtime.rs:284-295`）。
   10. **会话级 `files/list` 可用**：`POST /sessions/{id}/files/list {path}` 列出会话工作目录（相对路径锚在会话 `cwd`）。
   11. **会话级 `files/readText` 可用**：读回文本内容，`truncated:false`（超 1 MiB 仍是 `413 too_large`，不截断冒充完整）。
   12. **会话级 `terminal` 可用**：`POST /sessions/{id}/terminal` → 取票 → `GET /sessions/{id}/terminal/{terminalId}/stream`，PTY 起来并能回显；`terminals.session_id` 记下归属。
   13. **陌生人 3 条全 `404`**：对别人的会话调 `files/list`、`terminal`、`runtime/steer` 一律 `404 session not found`（不是 `403`：不泄露存在性；`fs.rs:139-140`、`terminals.rs:114`、`sessions.rs:168-179`）。
   14. **断连后 steer 409**：杀掉 agent 后 `POST .../runtime/steer` 回 `409`（`session_not_running` / `connector_offline`），并且缓存被清——此时 `GET .../runtime/capabilities` 回到全 `false`（`connector_ws.rs:196-207`）。

### 未验证（别当成已支持）

- **codex 成功路径**：assistant 文本增量、工具执行、真实审批往返（codex 自己发起 `item/commandExecution/requestApproval`、用户批准、工具真的执行）。
- **原因**：本机没有 Codex 凭据，成功路径进不去；目前唯一被真实观测到的 codex 行为是「401 如实落 timeline」。
- 因此「点批准 → 工具真的执行」这条链**没有任何证据**。
- **`file_change` / `permissions` 两种 notice kind 从未在联调中出现**：`NoticeKind` 有三个变体（`crates/farwork-agent/src/protocol.rs:68-72`），但 echo 后端只会发 `command_execution`（`crates/farwork-agent/src/echo.rs:54-68`），codex 的 `item/fileChange/requestApproval`（`crates/farwork-agent/src/codex/mapper.rs:593`）与 `item/permissions/requestApproval`（`mapper.rs:652`）分支因本机无凭据从未被触发。Web 侧对应标签「文件变更」「权限申请」（`apps/web/src/lib/notice.ts:5-11`）因此**只是代码就位，没有联调证据**。
- **浏览器端到端仍未记录证据**：审批卡的原验收（用 `--runtime echo` 起会话 → 页面出现卡片 → 点批准 → 卡片转已解决、会话回 `idle`）没有留存操作记录；已验收的部分仅为上一节的静态检查 + 代码层核对。

### 已修复的契约缺陷（记下来，别改回去）

`session.message.clientMessageId` 曾是「hub 发 `null`、agent 要求 `String`」：省略该字段的 REST 调用者会让这一轮被 agent 当畸形帧静默丢弃，而 hub 已回 `runtimeDispatched:true`（细节见 `docs/architecture.md` §5.4）。现在 `clientMessageId` 是**可选**字段：hub 无值时不发该键（`ingress.rs:346-368`）、agent 收 `Option<String>` 同时接受缺失与 `null`（`protocol.rs:358-364`）、hub 没给时 agent 本地生成 `cmid_<uuid>` 交给 `turn/start` 且不回落 hub（`hub.rs:337-353`）。两侧都不许收紧成必填——收紧就会重演整轮静默消失。

## M3 · 下一批（按此顺序）

顺序的理由：后面每一项都依赖 ingress 提供的真实连接，先做 UI 只会做出假状态。

原「1. Web 审批卡」「2. 新建会话表单」已交付（见上文「已交付」与「已验证」第 3 条）；其中原定验收项——用 `--runtime echo` 点批准直到卡片转已解决——**仍未留证据**，属于下一批要补的验证，不是要重做功能。文件浏览、设备终端、分享与管理台都已交付并划掉（见下），**roadmap 上计划的功能块已全部交付**——剩下的只有 codex 成功路径（阻塞于本机无凭据）与 37 条 `501` 的广度补齐，见本节末尾。**未验证项不因划掉一项而消失**：codex 成功路径、`file_change` / `permissions` 两种 notice kind、审批卡在浏览器里的端到端证据，仍留在上面的「未验证」清单里。

### ~~1. 文件浏览~~ —— **已交付**（只读，见上文「已交付」与「已验证」第 4 条）

第一批交付的入口是 connector 命名空间下的 `POST /connectors/{id}/fs/list` 与 `/fs/readText`；**会话命名空间的两条**（`POST /sessions/{id}/files/list|readText`）在本轮补上，两条走同一条 `fsrelay`（§3.5），所以浏览器现在按会话读文件、不必自己拿 connector id。仍缺的部分：写操作（一律要过审批闸门）、大文件与二进制流式（现在整块读，且有 1 MiB 硬上限）、目录树懒加载。

### ~~1. 终端~~ —— **已交付**（见上文「已交付」与「已验证」第 5 条）

第一批入口是 connector 命名空间下的 `POST /connectors/{id}/terminals` 与 `GET /connectors/{id}/terminals/{terminalId}/stream`；**会话命名空间的单数两条**（`POST /sessions/{id}/terminal` + `GET /sessions/{id}/terminal/{terminalId}/stream`）在本轮补上，共用同一条 `termrelay`，终端行多一个可空 `session_id`（`0005_session_terminal_scope.sql`，§3.6），所以浏览器现在按会话开终端。帧见 `docs/architecture.md` §5.11–§5.12。与计划相比的两处修正：

- **设备断连不再用 `exit code -1`**：exit 是设备上报的**真实退出码**（`code` 键恒在，信号终止时为 `null`），设备掉线走 `terminal.error{code:"connector_offline"}`。M1 那套「server 替在途流补发 `code=-1`」（附录 A.4）会把「设备没了」伪装成「进程退出了」，两类事实必须分开表达。
- **cwd 沙箱只约束起始目录**：起点必须在 agent 的 `--workdir` 内（与文件浏览同一套判定），但 shell 起来之后是它自己的能力。**不要**对外承诺「终端被关在 workdir 里」。
- 仍缺的部分：`farwork-server` 尚未退场（它仍是 M1 探针）；终端不持久化 scrollback、没有多路复用/接管别人已开的终端。

### ~~1. 分享~~ —— **已交付**（见上文「已交付」与「已验证」第 7 条）

交付的是会话级 `POST /sessions/{id}/shares` + `DELETE /sessions/{id}/shares/{share_id}`、消息级（`scope:"message"`，只分享那一条）以及免鉴权的 `GET /public/shares/{id}`；**撤销 = 软删**（写 `deleted_at`，公开读随后 `404`）。与计划的差距，别当成已支持：

- 账号级分享清单 `/shares`、`/shares/{*rest}` 仍回 `501`（这是原计划的「我的分享」列表）。
- **过期（`expiresAt`）没有实现**：现在只有「发布」与「撤销」两个动作，没有 TTL。
- 快照里不带附件：`allowedFileIds` 只是随 share 存了一份白名单，没有任何取附件的路由，所以今天一律为空。

### ~~2. 管理台~~ —— **已交付**（见上文「已交付」与「已验证」第 8 条）

交付的是**实例管理**这一块：`GET`/`POST /admin/users`、`PATCH /admin/users/{id}`、`GET`/`PATCH /admin/settings`（`crates/farwork-hub/src/routes/admin.rs`）+ Web 管理页（`apps/web/src/components/AdminPage.svelte`）。与计划的差距，别当成已支持：

- 原计划同一条还列了 `/agents*`、`/usage*`、`/announcements*`、`/pairing*` 四组——它们**没有**随这一批交付，仍回 `501`。管理台今天只有「用户目录 + 实例开关」。
- **没有审计日志**：谁改了谁、谁被停用，都不落盘（只有被改行自己的 `updated_at` 变一下）。
- **没有删除用户**（`routes/admin.rs` 只有 get/post/patch 三个方法）；角色是 `admin`/`member` 两档闭集，没有更细的权限模型；`users.avatar` 连读都不读（不在七键投影里），`emailVerified` 只读且今天恒为 `false`（没有邮件通道，`/auth/email-code` 仍 `501`）。

### 下一批（不是新功能：codex 成功路径 + 剩余 37 条 `501` + M4）

**1. codex 成功路径**：assistant 文本增量、工具执行、真实审批往返——阻塞于本机没有 Codex 凭据（见「未验证」）。这一条比 501 更值钱：它决定「agent 能不能真干活」。

**2. 剩余 37 条 `501` 的分组清单**（逐条见 `docs/architecture.md` §4；每收一组都要同步改 §3/§4 与 README 的计数，现在 `/api/v2` 是 **59** 个已实现的 method+path / **37** 条 `501`）：

| 分组 | 条数 | 路径 | 备注 |
| --- | --- | --- | --- |
| 会话同步与接管 | 2 | `/sessions/{id}/sync`、`/sessions/{id}/takeover` | `takeover` 没有 ingress 帧，做它必须先定一帧；`sync` 上游是「拿运行时最新快照」，等价于我们已有的 `session.state` 收敛，需先想清语义 |
| 账号级分享清单 | 2 | `/shares`、`/shares/{*rest}` | 会话级 create/delete 与公开读已实现（§3.8），这里只剩「我的分享」列表；`expiresAt` 仍无实现 |
| 批量会话维护 | 3 | `/sessions/archive`、`/sessions/unarchive`、`/sessions/read` | 与 `PATCH /sessions/{id}/meta` 的 `archived` 同语义，只是批量 |
| 设备运行时管理 | 8 | `/connectors/{id}/runtimes`、`/connectors/{id}/runtimes/{*rest}`、`/connectors/{id}/protocol/capabilities`、`/connectors/{id}/pairing`、`/runtimes`、`/runtimes/{*rest}`、`/devices`、`/devices/{*rest}` | 「编排 + 多设备配对」这一层；`/devices*` 是 M1 名词，**不要**照 M1 的写法回填（§附录 A） |
| OAuth / 邮件码 / 移动端扫码登录 | 14 | `/auth/oauth/{start,callback,finalize}`、`/auth/mobile-login/{qr,exchange,refresh,request,status,confirm}`、`/auth/email-code`、`/auth/change-password`、`/auth/me/{email,profile,avatar}` | 都需要外部通道（IdP、邮件、扫码），本 build 一条都没有；`/auth/email-code` 是把 `emailVerificationRequired` 变成「真开关」的前置 |
| 公告 / agents / usage / 配对 | 8 | `/announcements`、`/announcements/{*rest}`、`/agents`、`/agents/{*rest}`、`/pairing`、`/pairing/{*rest}`、`/usage`、`/usage/{*rest}` | 实例管理（`/admin*`）已实现，这四组是**另外四块**：公告、agent 目录、用量统计、配对码 |

**3. M4**：Capacitor Android App（其后 iOS），见下一节。它与上面两条不冲突，但优先级最低：**没有 codex 成功路径，App 装出来也只是把失败路径搬进手机**。

## M4 · Capacitor Android App（其后 iOS）

**目标**：用 [Capacitor](https://capacitorjs.com/docs/) 把同一套 Web 体验装进原生容器，Android 先行，移动端可随手接管远端会话。

技术栈：

- 复用 `apps/web` 的**静态 SPA 构建产物**：`@sveltejs/adapter-static`（`fallback: 'index.html'`，`ssr = false`）产出到 `apps/web/build`，Capacitor 只做原生容器与桥接，不重写 UI。
- Android 先行；iOS 复用**同一套 Web 产物**与同一套 JS 桥接代码，只多一层平台工程。
- 与 Web 共用同一套 API/事件客户端（`apps/web/src/lib/api/*`），不复制一份。
- **API base 运行时可配置**：App 与 hub 非同源，不能依赖 Vite dev proxy，必须走完整 URL；沿用现有 `PUBLIC_FARWORK_API` 约定（`apps/web/.env.example` 中「留空 = 同源」），App 内置设置页可覆盖并在本地持久化。

交付物：

- `apps/mobile`：Capacitor 工程（`capacitor.config.ts` + `android/` 平台目录），`webDir` 指向静态构建产物。
- 运行时配置层：API base 与登录态的设置页 + 本地持久化（Capacitor Preferences / 安全存储）。
- 移动端交互适配：小屏 timeline、手势滚动、可读字号、后台恢复后按 `seq:N` 游标补拉。
- 原生能力接入：审批请求与会话结束的系统通知、前后台切换后的自动重连。
- 分发：签名 APK/AAB 构建流程与安装说明。

验收点：

- Android 模拟器/真机安装后，在设置页填入自建 hub 的完整 URL，登录并保存后重启 App 配置不丢。
- 连接成功后 connectors / 项目 / 会话与 Web 端看到的是同一批数据（同一 `/api/v2/connectors` 与 `/api/v2/sessions` 结果）。
- 进入会话能渲染 timeline 并发送消息；切后台再回前台，凭游标补拉后 timeline 与实际一致（不缺条、不重复）。
- 无有效登录态时给出明确错误提示而不是白屏。
