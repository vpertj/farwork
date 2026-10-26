# Farwork 路线图

**当前**：M2 纵向切片已交付；**M3 部分交付** —— connector ingress + `farwork-agent` + 审批/中断的后端与协议已落地并实测，Web 侧审批卡与新建会话表单也已交付并验收；文件浏览及之后的项未开始。技术栈既定：Rust（hub / connector）+ Svelte（web），数据库 PostgreSQL，移动 App 用 Capacitor 包装静态 Web 产物并后置到 M4。

里程碑状态一览：

| 里程碑 | 状态 |
| --- | --- |
| M1 可行走骨架 | **已被 M2 取代**，代码保留为终端探针（`farwork-server` / `farwork-connector` / `farwork-protocol`） |
| M2 `/api/v2` 纵向切片 | **已交付**（2026-10-26，commit `2079149`） |
| M3 ingress / 审批 / 文件 / 终端 / 分享 / 管理台 | **部分交付**：ingress、`farwork-agent`、审批与中断（后端 + 协议）已实测；Web 审批卡与新建会话表单已交付；文件浏览及之后的项未开始 |
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
- **诚实兜底**：52 条未实现路径统一 `501 not_implemented`，未注册路径 `404`；`GET /connectors` 的 `status` 当时恒为 `offline`（没有 ingress 就没有在线值）——这条已被 M3 取代：现在 49 条 `501`，`status` 有真实 `online`/`offline`。
- **CI 硬化**：`rust` job 挂 `postgres:18-alpine` service 并注入 `TEST_DATABASE_URL`，让 hub 集成测试从「静默 SKIP」变成真跑。

### 已验收

- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（带 `TEST_DATABASE_URL`）全绿。
- `apps/web` 的 `pnpm run check`、`pnpm run build` 通过。
- 端到端旅程（`crates/farwork-hub/tests/e2e.rs`）：`/healthz` → 首个 admin 注册（setup token）→ 登录 → 建 connector / 项目 / 会话 → snapshot → 发消息 → `events?after=seq:N` 补拉 → `/sessions/{id}/ws` 收到增量。
- 重复 `clientMessageId` 不再产生新条目（`WriteOutcome::Unchanged`，无新事件）。

### 明确未含（当时都是 `501`）

connector ingress · 真 agent 执行 · 审批与中断 · 文件浏览 · 终端 · 分享 · 管理台（admin / agents / usage / announcements / pairing）。其中 connector ingress、审批/中断与文件浏览已由 M3 交付（见下），终端 / 分享 / 管理台仍未实现。

## M3 · connector ingress + agent（**部分交付**）

目标没变：让 hub 真的连上一台设备、真的把会话交给运行时。本轮先交付**协议与后端**，随后 Web 侧补上了审批卡、新建会话表单、会话中断与状态原因行。

### 已交付

- **connector ingress**：`GET /api/v2/connector/ws?token=...`（Ingress Protocol v1，契约见 `docs/architecture.md` §5）。hello → welcome → 心跳 → 断连收敛；`connectors.status` 从「恒为 `offline`」变成真实的 `online`/`offline`。
- **`farwork-agent`（新 crate）**：主动出站连 hub；身份落盘在 `$FARWORK_HOME/agent.json`（文件 0600、目录 0700，重启不换 id）；按 `session.start` 拉起 `codex app-server --stdio` 并把 JSON-RPC 翻成 ingress 帧；自带 `--runtime echo` 假后端用于自测。
- **会话真派发**：`POST /sessions/create-and-start` 会下发 `session.start`（首条消息走 `prompt`）；`POST /sessions/{id}/runtime/messages` 在会话有活跃流时把 `session.message` 推给 agent，响应里的 `runtimeDispatched` 才是真实投递结果。
- **审批与中断（后端）**：`GET /sessions/{id}/runtime/notices`、`POST /sessions/{id}/notices/{id}/respond`（+ `runtime` 别名）、`POST /sessions/{id}/runtime/interrupt` 落地；新增迁移 `0002_connector_ingress.sql`（`session_streams` 让 `streamId` 跨 hub 重启存活，`session_notices` 让审批跨刷新存活）。
- **文件浏览（只读）**：新增 ingress 帧 `fs.request` / `fs.response`（hub 侧 `crates/farwork-hub/src/ingress.rs` + 配对表 `crates/farwork-hub/src/fsrelay.rs`，agent 侧 `crates/farwork-agent/src/protocol.rs` + 沙箱 `crates/farwork-agent/src/fsops.rs`），REST 两条 `POST /connectors/{id}/fs/list`、`POST /connectors/{id}/fs/readText`（`crates/farwork-hub/src/routes/fs.rs`），Web 侧会话页文件面板（`apps/web/src/components/FilePanel.svelte`，API 是 `apps/web/src/lib/api/workspace.ts` 的 `listWorkspaceDir` / `readWorkspaceText`）。**沙箱根是 agent 的 `--workdir`**：`canonicalize` 后按路径组件判前缀，越界/符号链接逃逸一律 403，`workdir` 不可解析时直接拒绝（契约见 `docs/architecture.md` §5.5–§5.6）。`501` 表**没有变化**，仍是 49 条：这批入口本来就不在那张表里（表里的 `/sessions/{id}/files*` 是另一组）。
- **诚实兜底更窄了**：`501` 表从 52 条减到 **49 条**（移出 interrupt / notices / notices respond 三条）；REST 与 ingress 两侧仍然「宁可变窄，不伪造成功」。
- **Web 审批卡**（`apps/web/src/components/ApprovalCard.svelte`）：三个决定按钮 `approve` / `approve_for_session` / `reject`（`:75-93`）；点击后状态先落在「已下发，等待运行时确认」（`:24-35,101`），卡片变灰**只**取决于服务端 `notice.status === 'resolved'`（`:22,25`）；投递失败可点「重试」（`:96-97`）。
- **Web 新建会话表单**（`apps/web/src/components/NewSessionPage.svelte`，路由 `#/new-session[/:projectId]`，`apps/web/src/lib/router.ts:26-53`）：一次填 connector + 项目 + runtime + 首条消息；`connectorResult.ok` 为真则跳进会话页（`NewSessionPage.svelte:93`），为假则如实显示「设备未连接：指令未下发」（`:105,213`）+ 返回码（`:222`）+ 已创建会话 id（`:218`）。
- **Web 中断与状态原因**（`apps/web/src/components/SessionView.svelte`）：中断按钮不本地改状态，只显示「中断指令已下发（stream …），等待运行时回推状态更新」（`:116-119,135-137`）；会话 `statusReason` 单独一行「状态原因：…」（`:129-130`，例如 `connector disconnected`）。

### 已验证（有证据）

1. **真二进制全链路（codex，失败路径）**：hub + `farwork-agent`（真 codex 二进制）跑通 `connectors.status → online` → `create-and-start` → agent 收到 `session.start` → codex 子进程启动 → codex 因**本机无凭据返回 401** → 这个错误**如实落进 timeline**（一条 `system`/`failed` 条目，不是只写日志）→ 杀掉 agent 后 `connectors.status` 回落 `offline`。
2. **审批管道往返（echo 假后端）**：hub + `farwork-agent --runtime echo`（需 `FARWORK_HUB_DEV_RUNTIMES=echo`，默认关闭，启动会打 WARN）跑通完整一轮：`create-and-start` 下发 → agent 产生 `command_execution` notice（`status: open`）→ 会话状态 `waiting_approval` → `POST .../notices/{id}/respond` 带 `approve` 返回 200 → connector 回报 `notice.resolved{decision:approve}` → 会话回 `idle`；杀掉 agent 后 `connectors.status` 回落 `offline`。
   - 注意这条验证的是**审批管道**，不是真实 codex 审批。echo 后端不执行任何东西，它只是把契约跑一遍（`crates/farwork-agent/src/echo.rs`）。
3. **Web 交付的静态验收（仓库所有者验收）**：`apps/web` 的 `pnpm run check` 0 error、`pnpm run build` 通过；审批卡不乐观 resolve 在代码层确认——`POST .../notices/{id}/respond` 的分支只返回 `ok` + 仍是 `open` 的 `notice.view()` + `serverTime`（`crates/farwork-hub/src/routes/notices.rs:333-340`），把 `status` 写成 `resolved` 的 SQL 只在 connector 回报的 `resolve_notice` 里（`notices.rs:159-169`）；前端同样只在 `notice.status === 'resolved'` 时变灰（`apps/web/src/components/ApprovalCard.svelte:22,25`）。
4. **文件浏览 11/11 断言通过（仓库所有者验收）**：`crates/farwork-hub/tests/fs_bridge.rs`（假 connector + 真 PostgreSQL）跑通 `fs/list` 成功、`fs/readText` 成功，并逐条断言越权与异常：根外路径（`/etc/passwd` 这类）→ `403`、`../` 上跳 → `403`、指向根外的符号链接 → `403`、文件缺失 → `404`、相对路径 → `422 invalid_path`、并发两个请求按 `requestId` 配对不串号（`fs_bridge.rs:545-560`）、agent 停掉后 → `503 connector_offline`（`fs_bridge.rs:385-395`）。三条越权用例的判定在 agent 沙箱单测里（`crates/farwork-agent/src/fsops.rs:320-425`，另有一条前缀陷阱 `fsops.rs:381-402`）；REST 侧 `permission_denied → 403` 的映射断言在 `fs_bridge.rs:497-535`（由假 connector 回 `permission_denied`），同一处还断言了 `404 not_found`、`413 too_large`、`502 io_error`，`504 fs_timeout` 在 `fs_bridge.rs:630-650`。
5. **`session.message.clientMessageId` 契约缺陷已修复**：hub 无值时不发该键（`crates/farwork-hub/src/ingress.rs:324-346`，回归测试 `ingress::tests::session_message_frame_omits_an_absent_client_message_id`）、agent 收 `Option<String>` 兼容缺失与 `null`（`crates/farwork-agent/src/protocol.rs:192-200`，回归测试 `protocol::tests::session_message_parses_without_a_client_message_id`）、agent 本地生成 `cmid_<uuid>` 供 `turn/start`（`crates/farwork-agent/src/hub.rs:303-319`）。契约口径见 `docs/architecture.md` §5.4。

### 未验证（别当成已支持）

- **codex 成功路径**：assistant 文本增量、工具执行、真实审批往返（codex 自己发起 `item/commandExecution/requestApproval`、用户批准、工具真的执行）。
- **原因**：本机没有 Codex 凭据，成功路径进不去；目前唯一被真实观测到的 codex 行为是「401 如实落 timeline」。
- 因此「点批准 → 工具真的执行」这条链**没有任何证据**。
- **`file_change` / `permissions` 两种 notice kind 从未在联调中出现**：`NoticeKind` 有三个变体（`crates/farwork-agent/src/protocol.rs:68-72`），但 echo 后端只会发 `command_execution`（`crates/farwork-agent/src/echo.rs:54-68`），codex 的 `item/fileChange/requestApproval`（`crates/farwork-agent/src/codex/mapper.rs:593`）与 `item/permissions/requestApproval`（`mapper.rs:652`）分支因本机无凭据从未被触发。Web 侧对应标签「文件变更」「权限申请」（`apps/web/src/lib/notice.ts:5-11`）因此**只是代码就位，没有联调证据**。
- **浏览器端到端仍未记录证据**：审批卡的原验收（用 `--runtime echo` 起会话 → 页面出现卡片 → 点批准 → 卡片转已解决、会话回 `idle`）没有留存操作记录；已验收的部分仅为上一节的静态检查 + 代码层核对。

### 已修复的契约缺陷（记下来，别改回去）

`session.message.clientMessageId` 曾是「hub 发 `null`、agent 要求 `String`」：省略该字段的 REST 调用者会让这一轮被 agent 当畸形帧静默丢弃，而 hub 已回 `runtimeDispatched:true`（细节见 `docs/architecture.md` §5.4）。现在 `clientMessageId` 是**可选**字段：hub 无值时不发该键（`ingress.rs:324-346`）、agent 收 `Option<String>` 同时接受缺失与 `null`（`protocol.rs:192-200`）、hub 没给时 agent 本地生成 `cmid_<uuid>` 交给 `turn/start` 且不回落 hub（`hub.rs:303-319`）。两侧都不许收紧成必填——收紧就会重演整轮静默消失。

## M3 · 下一批（按此顺序）

顺序的理由：后面每一项都依赖 ingress 提供的真实连接，先做 UI 只会做出假状态。

原「1. Web 审批卡」「2. 新建会话表单」已交付（见上文「已交付」与「已验证」第 3 条）；其中原定验收项——用 `--runtime echo` 点批准直到卡片转已解决——**仍未留证据**，属于下一批要补的验证，不是要重做功能。文件浏览已交付并划掉（见下）。**未验证项不因划掉一项而消失**：codex 成功路径、`file_change` / `permissions` 两种 notice kind、审批卡在浏览器里的端到端证据，仍留在上面的「未验证」清单里。

### ~~1. 文件浏览~~ —— **已交付**（只读，见上文「已交付」与「已验证」第 4 条）

交付的入口是 connector 命名空间下的 `POST /connectors/{id}/fs/list` 与 `/fs/readText`，**不是**原计划里的 `/sessions/{id}/files*`（那组仍回 `501`，归写操作）。仍缺的部分：写操作（一律要过审批闸门）、大文件与二进制流式（现在整块读，且有 1 MiB 硬上限）、目录树懒加载。

### 1. 终端

- `/sessions/{id}/terminal*` 隧道；复用 M1 的 PTY + base64 字节帧实现，前端复用 xterm.js。
- 验收：真终端可交互、resize 生效、设备断连以 `exit code -1` 明确区分；随之让 `farwork-server` 退场。

### 2. 分享

- `/sessions/{id}/shares*` 与 `/shares*`：只读/可写分享链接、过期与撤销。

### 3. 管理台

- `/admin*`、`/agents*`、`/usage*`、`/announcements*`、`/pairing*`：用户管理、运行时清单、用量、公告、设备配对。

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
