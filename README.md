# Farwork

**跨设备的开源 Agent 工作台。**

在任意设备的浏览器里打开远端设备上的真实 Agent 会话：会话、工具调用与产物以结构化 timeline 呈现，消息可在多端续接。远端设备只需跑一个常驻 Connector 主动出站，不需要公网 IP，也不需要端口映射。

技术栈：**纯 Rust 后端 + Svelte Web，全仓零 Python**——仓库内没有任何 `.py` 文件、Python 脚本或 Python 依赖，也不允许引入（详见 [`AGENTS.md`](./AGENTS.md) 第 4 节）。

## 与上游项目的关系

Farwork 是 [anywhere-labs/Agents-Anywhere](https://github.com/anywhere-labs/Agents-Anywhere) 的**非官方**重写项目：

- **独立重写**：上游服务端是 Python（FastAPI）单体；本仓库服务端是 Rust（axum + sqlx + PostgreSQL），Web 是 Svelte/SvelteKit；Connector 目标是 Rust。
- **代码不共享**：本仓库是独立实现，不包含、不复制上游源代码，只借鉴产品形态、线协议字段与交互概念。
- **致谢**：上游以 MIT 许可发布，归属声明见 [NOTICE](./NOTICE)。
- **非隶属关系**：本项目与上游维护者无隶属、赞助或背书关系，不代表上游立场。

## 当前状态：M3（部分交付）

已交付（真实可用，不是骨架）：

- **登录**：`/api/v2/auth/*`——邮箱 + PBKDF2 口令；首个账号需 setup token（启动日志打印）。
- **设备（connector）/ 项目 / 会话**：列表、详情、改名、归档；新建会话走 `create-and-start`（导入已存在的运行时会话走 `POST /sessions`）。
- **timeline 渲染**：`/api/v2/sessions/{id}/snapshot` 一次拿到会话 + 运行时状态 + 条目；`seq:N` 事件游标可在断线后补拉。
- **发消息**：`POST /api/v2/sessions/{id}/runtime/messages`，写入即持久化，`clientMessageId` 幂等。
- **WS 增量**：`/api/v2/sessions/{id}/ws`（会话事件流）与 `/api/v2/dashboard/ws`（设备/项目/会话总览），凭一次性 `ws-ticket` 握手。
- **connector ingress + 真 agent（M3）**：设备侧常驻进程 `farwork-agent` 主动出站连 `GET /api/v2/connector/ws?token=...`（Ingress Protocol v1），hub 按 `session.start` 把会话交给设备上的 `codex app-server`；因此 `connectors.status` 有了真实 `online`/`offline`。
- **审批与中断（后端，M3）**：`GET /api/v2/sessions/{id}/runtime/notices`、`POST /api/v2/sessions/{id}/notices/{id}/respond`、`POST /api/v2/sessions/{id}/runtime/interrupt`；审批落库，刷新不掉。
- **Web 审批卡**：会话页把待办审批渲染成卡片，三个决定 `approve` / `approve_for_session` / `reject`；点击后先显示「已下发，等待运行时确认」，**不本地改成已批准**——变灰只认服务端 `notice.resolved`；投递失败可重试。
- **Web 新建会话表单**：`#/new-session`（可带项目 id），一次填 connector + 项目 + runtime + 首条消息；成功即跳进会话页；设备不在线时如实提示「设备未连接：指令未下发」+ 返回码 + 已创建会话 id（不会假装下发成功）。runtime 下拉取自 `SUPPORTED_RUNTIMES`（`apps/web/src/lib/timeline.ts:37`：`codex/claude/opencode/acp/dsh`），与 hub 的 `RUNTIMES`（`crates/farwork-hub/src/routes/sessions.rs:57`）一致。
- **Web 会话中断与状态原因**：中断按钮只在指令下发后提示「等待运行时回推状态更新」，不本地改状态；会话的 `statusReason`（如 `connector disconnected`）单独一行展示。

**还不能做**（诚实清单）：

- **codex 的成功路径未验证**：assistant 文本增量、工具执行、真实审批往返都还没有跑通过，原因是本机没有 Codex 凭据。已验证的是「codex 起得来、401 如实落到 timeline」和「用 `--runtime echo` 把审批管道跑通」。
- **`file_change` / `permissions` 两种 notice kind 未在联调中出现**：echo 后端只发 `command_execution`，codex 的这两个审批分支因无凭据从未被触发；页面上的「文件变更」「权限申请」标签只是代码就位，没有联调证据。
- **审批卡在浏览器里的端到端流程没有留证据**：已验收的是 `pnpm run check`（0 error）/ `pnpm run build` 通过与「hub 不乐观 resolve」的代码层核对，不是「点批准 → 卡片转已解决」的实机记录。
- **仍是 `501 not_implemented`**（不会伪造成功）：文件浏览、终端、会话分享、管理台（admin / agents / usage / announcements / pairing）。

当前端点数：`/api/v2` 下 **38 个已实现的 method+path**（31 条 `route()` 注册）+ `GET /healthz`；另有 **49 条路径**在 `routes/unsupported.rs` 里统一回 `501`。逐条清单见 [docs/architecture.md](./docs/architecture.md)。

路线图：[docs/roadmap.md](./docs/roadmap.md)。

## 架构（M3）

```
 +-----------------------------+        HTTP /api/v2           +-----------------------------------+
 |  Browser (apps/web :5173)   |        WS   /dashboard/ws     |  farwork-hub            :7300     |
 |  SvelteKit 静态 SPA         |<----------------------------->|  axum: REST + 3 个 WS 端点         |
 |  Svelte 5 runes + timeline  |        WS /sessions/{id}/ws   |  PostgreSQL（唯一状态来源）        |
 |  渲染                       |                               |  farwork-timeline（纯域逻辑）      |
 +-----------------------------+                               +------------------+----------------+
                                                                                  |
                                                                                  |  connector ingress（Ingress Protocol v1，设备出站）
                                                                                  |
                                                                                  v
                                                        +-------------------------+---------+
                                                        |  farwork-agent（M3，设备侧常驻）  |
                                                        |  按 session.start 拉起 codex      |
                                                        +-------------------------+---------+
                                                                                  |
                                                                                  v
                                              [ codex app-server（stdio JSON-RPC） ]
```

- `farwork-hub`：`/api/v2` 服务端。REST + 3 个 WS 端点（dashboard / session / **connector ingress**）+ `POST /ws-ticket`，PostgreSQL 持久化，启动时自动跑迁移（2 个迁移、10 张表）。
- `farwork-agent`：M3 的设备侧常驻进程。主动出站连 hub，按 `session.start` 拉起 `codex app-server --stdio`，把 JSON-RPC 翻成 ingress 帧；身份落盘 `~/.farwork/agent.json`（0600）。
- `farwork-timeline`：timeline 的**纯域逻辑**（排序、`seq:N` 事件游标与补拉、`content_hash` 去重、每会话写栅栏），无 IO、无时钟、可用内存后端单测。hub 只负责持久化与传输。
- `farwork-server` / `farwork-connector` / `farwork-protocol`：**M1 遗留**，保留为终端探针，尚未与 hub 打通。
- `apps/web`：SvelteKit + `adapter-static` 的静态 SPA；dev 期经 Vite 代理 `/api`、`/ws` 到 `127.0.0.1:7300`。

协议与端点的逐条定义以 [docs/architecture.md](./docs/architecture.md) 为唯一事实来源。

## 快速开始

前置：Rust stable（见 `rust-toolchain.toml`）、**PostgreSQL（必装，hub 没有内存后端）**、Node.js 24 + pnpm 11.19.0。

### 1. 建库

```bash
# 用你自己的 PostgreSQL；库名可自定义，下一步通过 DATABASE_URL 指过去
createdb farwork
# 等价写法：psql -d postgres -c 'create database farwork'
```

### 2. 起 hub（默认 7300）

```bash
export DATABASE_URL='postgres://<user>@127.0.0.1:5432/farwork'   # 不设置则回退到 postgres://tianjun@127.0.0.1:5432/farwork
cargo run -p farwork-hub
# 启动时打印：first-run setup token (required to register the first admin)
```

`migrations/` 在每次启动时自动应用，不需要手工建表。首个账号用日志里的 setup token 注册（或用 `FARWORK_HUB_SETUP_TOKEN` 预先固定）。

### 3. 起 Web（默认 5173）

```bash
cd apps/web && pnpm install && pnpm dev
```

浏览器打开 <http://localhost:5173>，用刚注册的账号登录，即可看到 connectors（设备）/ 项目 / 会话，进入会话能看到 timeline 并发送消息。

### 4. 起 agent（M3，可选）

```bash
# connectorToken 来自 POST /api/v2/connectors 的响应，明文只出现这一次
cargo run -p farwork-agent -- --token '<connectorToken>'
# 或：FARWORK_CONNECTOR_TOKEN='<connectorToken>' cargo run -p farwork-agent
```

连上后该设备在 `GET /api/v2/connectors` 里变成 `online`，新建的会话会被真的派发到设备上。设备需要能执行 `codex`（agent 会以 `codex app-server --stdio` 拉起它）；没有 codex 或没有凭据时，会话会以一条 `failed` 的系统条目收场——这是如实结果，不是成功。

只想验证协议与审批管道、不装 codex：hub 用 `FARWORK_HUB_DEV_RUNTIMES=echo cargo run -p farwork-hub` 启动，agent 加 `--runtime echo`。`echo` 默认**不在**允许的 runtime 集合里，必须显式打开这个 dev-only 开关（启动会打 WARN），生产不要设。

> **端口冲突提醒**：`farwork-hub` 与 M1 的 `farwork-server` **默认都监听 7300**。两套后端不能同时起；要跑 M1 探针请显式 `--port`。

## 环境变量

| 变量 | 默认 | 用途 |
| --- | --- | --- |
| `DATABASE_URL` | `postgres://tianjun@127.0.0.1:5432/farwork` | hub 的 PostgreSQL 连接串。**库必须已存在**；表由内置迁移创建。连接失败则启动失败 |
| `FARWORK_HUB_PORT` / `PORT` | `7300` | 监听端口（`FARWORK_HUB_PORT` 优先）。命令行 `--port <n>` 覆盖两者 |
| `FARWORK_HUB_SECRET` | `agent-server-dev-secret` | 签名/令牌派生密钥。**未设置时打印 warn 并回退到开发密钥**；生产必须显式设置 |
| `FARWORK_HUB_SETUP_TOKEN` | 未设置 | 首个 admin 注册令牌。未设置时进程生成一个随机值并打到日志（30 分钟 TTL，过期轮换），从不经 HTTP 返回 |
| `FARWORK_HUB_DB_POOL` | `10` | sqlx 连接池最大连接数（非法值回退默认） |
| `TEST_DATABASE_URL` | 未设置 | **仅测试**：hub 集成测试的目标库。未设置时测试打印 `SKIP ...` 并直接返回（不是通过） |
| `PUBLIC_FARWORK_API` | 空 | **仅 web 构建期**：API base。留空 = 与页面同源（dev 走 Vite 代理） |
| `FARWORK_HUB_DEV_RUNTIMES` | 空 | **dev-only，默认关闭**：逗号分隔的额外 runtime 名（例如 `echo`），追加到 hub 的允许集合；非空时启动打 WARN。默认允许集合不含 `echo`，生产不要设 |
| `FARWORK_CONNECTOR_TOKEN` | 未设置 | **仅 agent**：`--token` 的等价环境变量（`--token` 优先）。都没有时 agent 以退出码 1 退出 |
| `FARWORK_CODEX_BIN` | `codex` | **仅 agent**：codex 可执行文件路径（`--codex-bin` 覆盖） |
| `FARWORK_HOME` | `~/.farwork` | **仅 agent**：身份文件目录（`agent.json`，0600，不含 token） |

## 常用命令

```bash
cargo run -p farwork-hub                              # 起 /api/v2 后端，需要 DATABASE_URL
cargo run -p farwork-agent -- --token '<connectorToken>'   # 起设备侧 agent，连本机 hub
cargo run -p farwork-agent -- --token '<t>' --runtime echo # 假后端，只跑协议与审批管道
cargo fmt --all -- --check                            # CI 要过
cargo clippy --workspace --all-targets -- -D warnings  # CI 要过
TEST_DATABASE_URL=postgres://<user>@127.0.0.1:5432/farwork_test cargo test --workspace
TEST_DATABASE_URL=... && psql "$TEST_DATABASE_URL" -c 'select 1'   # 先确认测试库存在
```

> `cargo test --workspace` **需要** `TEST_DATABASE_URL`：缺失时 hub 的端到端测试会打印 SKIP 并跳过，结果看起来是绿的但什么都没验。CI 里由 `postgres:18-alpine` service 提供该库，见 `.github/workflows/ci.yml`。测试会**清空**目标库的表，必须指向一次性库（例如 `farwork_test`），不要指向开发库。

Web（在 `apps/web` 下）：`pnpm install --frozen-lockfile` → `pnpm run check` → `pnpm run build`（静态产物在 `apps/web/build`）。

## 目录结构

```
farwork/
├── Cargo.toml                    # workspace 根（6 个 crate）
├── rust-toolchain.toml
├── crates/
│   ├── farwork-hub/              # /api/v2 服务端：axum + sqlx/PostgreSQL + 3 个 WS（含 connector ingress）
│   ├── farwork-timeline/         # timeline 纯域逻辑：排序、seq 游标、content_hash、写栅栏
│   ├── farwork-agent/            # M3 设备侧：连 hub + 驱动 codex app-server（真 connector）
│   ├── farwork-protocol/         # M1 遗留：M1 线协议类型（现仅 server/connector 使用）
│   ├── farwork-server/           # M1 遗留：7300 上的 M1 REST + /ws 终端骨架
│   └── farwork-connector/        # M1 遗留：PTY 转发，只连 farwork-server（不是 agent 的旧版本）
├── apps/
│   └── web/                      # SvelteKit + adapter-static SPA（pnpm 管理）
├── docs/
│   ├── architecture.md           # 当前架构、/api/v2 端点表、Connector Ingress（§5）、timeline 语义、M1 附录
│   └── roadmap.md                # M1–M4 里程碑与验收点
├── .github/workflows/ci.yml      # CI：rust(fmt/clippy/test + postgres) + web(check/build)
├── LICENSE                       # MIT
└── NOTICE                        # 上游归属声明
```

## 路线图

M1 可行走骨架（已被 M2 取代，保留为终端探针）→ **M2 纵向切片（已交付）** → **M3 connector ingress + agent（部分交付：后端与协议已实测，Web 审批卡与新建会话表单已交付；文件浏览、终端、分享、管理台未开始）** → M4 Capacitor Android App（其后 iOS）。
每个里程碑的交付物与验收点见 [docs/roadmap.md](./docs/roadmap.md)。

## 许可

本项目以 [MIT](./LICENSE) 许可发布。对上游项目 anywhere-labs/Agents-Anywhere 的归属与致谢见 [NOTICE](./NOTICE)。

## English

Farwork is an open-source, cross-device agent workbench: open a real agent session on a remote machine straight from your browser, with no inbound ports and no public IP. A small always-on Connector dials out; the hub persists sessions and their timeline in PostgreSQL and pushes deltas over WebSocket.

This repository is an **unofficial** rewrite of [anywhere-labs/Agents-Anywhere](https://github.com/anywhere-labs/Agents-Anywhere) in **Rust + Svelte**. It shares no source code with upstream and only borrows the product shape; upstream is MIT-licensed (see NOTICE). We are not affiliated with or endorsed by the upstream maintainers.

Status: the M2 vertical slice is delivered (login, connectors/projects/sessions, timeline, sending messages, WebSocket deltas, cursor catch-up), and M3 is partially delivered — connector ingress (`/api/v2/connector/ws`, Ingress Protocol v1) plus `farwork-agent`, which drives a local `codex app-server` and relays approvals; `connectors.status` now reflects real presence. Still missing: the web approval card, the new-session form, files, terminal, sharing and the admin console (`501`), and — importantly — the codex success path (assistant streaming, tool execution, a real approval round trip) is unverified because there are no Codex credentials on the test machine. Details in [docs/](./docs). Licensed under MIT.
