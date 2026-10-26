# Farwork 路线图

**当前**：M2 纵向切片已交付；M3 是下一批工作。技术栈既定：Rust（hub / connector）+ Svelte（web），数据库 PostgreSQL，移动 App 用 Capacitor 包装静态 Web 产物并后置到 M4。

里程碑状态一览：

| 里程碑 | 状态 |
| --- | --- |
| M1 可行走骨架 | **已被 M2 取代**，代码保留为终端探针（`farwork-server` / `farwork-connector` / `farwork-protocol`） |
| M2 `/api/v2` 纵向切片 | **已交付**（2026-10-26，commit `2079149`） |
| M3 ingress / 审批 / 文件 / 终端 / 分享 / 管理台 | 下一批，按 §M3 顺序推进 |
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
- **诚实兜底**：52 条未实现路径统一 `501 not_implemented`，未注册路径 `404`；`GET /connectors` 的 `status` 恒为 `offline`（没有 ingress 就没有在线值）。
- **CI 硬化**：`rust` job 挂 `postgres:18-alpine` service 并注入 `TEST_DATABASE_URL`，让 hub 集成测试从「静默 SKIP」变成真跑。

### 已验收

- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（带 `TEST_DATABASE_URL`）全绿。
- `apps/web` 的 `pnpm run check`、`pnpm run build` 通过。
- 端到端旅程（`crates/farwork-hub/tests/e2e.rs`）：`/healthz` → 首个 admin 注册（setup token）→ 登录 → 建 connector / 项目 / 会话 → snapshot → 发消息 → `events?after=seq:N` 补拉 → `/sessions/{id}/ws` 收到增量。
- 重复 `clientMessageId` 不再产生新条目（`WriteOutcome::Unchanged`，无新事件）。

### 明确未含（都是 `501`）

connector ingress · 真 agent 执行 · 审批与中断 · 文件浏览 · 终端 · 分享 · 管理台（admin / agents / usage / announcements / pairing）。

## M3 · 下一批（按此顺序）

顺序的理由：后面每一项都依赖 ingress 提供的真实连接，先做 UI 只会做出假状态。

### 1. connector ingress + codex app-server 集成

- hub 增加 connector 入站通道（现无任何路径，`/connectors/{id}/runtimes*` 全是 `501`），connector 侧改为连 hub 并带持久化身份。
- 接 codex app-server：会话真正被派发、`runtime/state` 与 `capabilities` 有真实值，条目由运行时上报而不是只有用户消息。
- 验收：`create-and-start` 的响应里 `runtimeDispatched` 变成 `true`；`connectors.status` 出现真实 `online`；重启 hub 后 connector 自动重连。

### 2. 审批 / 中断

- `/runtime/notices`、`/runtime/notices/{id}/respond`、`/runtime/interrupt`、`/runtime/steer` 落地；审批决策落库可审计。
- 验收：Agent 提议敏感工具调用 → 前端卡片 → 同意后执行、拒绝则不执行且 Agent 收到原因；中断后会话仍可继续。

### 3. 新建会话表单

- 前端按 connector + 项目 + runtime + 首条消息一次性创建，取代当前只能在已有会话里发消息的限制。
- 验收：表单创建后立刻进入会话页并且首次消息已经在 timeline 里，刷新不丢。

### 4. 文件浏览

- `/sessions/{id}/files*` 经 connector 中继；大文件与二进制流式处理，不整块读入内存；写操作一律过审批闸门。

### 5. 终端

- `/sessions/{id}/terminal*` 隧道；复用 M1 的 PTY + base64 字节帧实现，前端复用 xterm.js。
- 验收：真终端可交互、resize 生效、设备断连以 `exit code -1` 明确区分；随之让 `farwork-server` 退场。

### 6. 分享

- `/sessions/{id}/shares*` 与 `/shares*`：只读/可写分享链接、过期与撤销。

### 7. 管理台

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
