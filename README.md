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

## 当前状态：M2 纵向切片

已交付（真实可用，不是骨架）：

- **登录**：`/api/v2/auth/*`——邮箱 + PBKDF2 口令；首个账号需 setup token（启动日志打印）。
- **设备（connector）/ 项目 / 会话**：列表、详情、改名、归档；新建会话走 `create-and-start`（导入已存在的运行时会话走 `POST /sessions`）。
- **timeline 渲染**：`/api/v2/sessions/{id}/snapshot` 一次拿到会话 + 运行时状态 + 条目；`seq:N` 事件游标可在断线后补拉。
- **发消息**：`POST /api/v2/sessions/{id}/runtime/messages`，写入即持久化，`clientMessageId` 幂等。
- **WS 增量**：`/api/v2/sessions/{id}/ws`（会话事件流）与 `/api/v2/dashboard/ws`（设备/项目/会话总览），凭一次性 `ws-ticket` 握手。

**还没有**（调用会得到 `501 not_implemented`，不会伪造成功）：

connector ingress（没有任何上游连接接进 hub，因此 agent 不会真的执行）· 真 agent 执行 / 运行时拉起 · 审批与中断 · 文件浏览与终端 · 会话分享 · 管理台（admin/agents/usage/announcements 等）。

当前端点数：`/api/v2` 下 **34 个已实现的 method+path**（26 条 `route()` 注册）+ `GET /healthz`；另有 **52 条路径**在 `routes/unsupported.rs` 里统一回 `501`。逐条清单见 [docs/architecture.md](./docs/architecture.md)。

路线图：[docs/roadmap.md](./docs/roadmap.md)。

## 架构（M2）

```
 +-----------------------------+        HTTP /api/v2           +-----------------------------------+
 |  Browser (apps/web :5173)   |        WS   /dashboard/ws     |  farwork-hub            :7300     |
 |  SvelteKit 静态 SPA         |<----------------------------->|  axum: REST + 3 个 WS 端点         |
 |  Svelte 5 runes + timeline  |        WS /sessions/{id}/ws   |  PostgreSQL（唯一状态来源）        |
 |  渲染                       |                               |  farwork-timeline（纯域逻辑）      |
 +-----------------------------+                               +------------------+----------------+
                                                                                  |
                                                            (M3 才实现) connector ingress
                                                                                  |
                                                                                  v
                                                        +-------------------------+---------+
                                                        |  farwork-connector（M1 遗留）     |
                                                        |  PTY 转发；尚未接入 hub           |
                                                        +-------------------------+---------+
                                                                                  |
                                                                                  v
                                                                     [ agent runtime / shell ]
```

- `farwork-hub`：`/api/v2` 服务端。REST + 3 个 WS 端点，PostgreSQL 持久化，启动时自动跑迁移。
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

## 常用命令

```bash
cargo run -p farwork-hub                              # 起 /api/v2 后端，需要 DATABASE_URL
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
├── Cargo.toml                    # workspace 根（5 个 crate）
├── rust-toolchain.toml
├── crates/
│   ├── farwork-hub/              # /api/v2 服务端：axum + sqlx/PostgreSQL + 3 个 WS
│   ├── farwork-timeline/         # timeline 纯域逻辑：排序、seq 游标、content_hash、写栅栏
│   ├── farwork-protocol/         # M1 遗留：M1 线协议类型（现仅 server/connector 使用）
│   ├── farwork-server/           # M1 遗留：7300 上的 M1 REST + /ws 终端骨架
│   └── farwork-connector/        # M1 遗留：PTY 转发；尚未接入 hub
├── apps/
│   └── web/                      # SvelteKit + adapter-static SPA（pnpm 管理）
├── docs/
│   ├── architecture.md           # 当前架构、/api/v2 端点表、timeline 语义、M1 协议附录
│   └── roadmap.md                # M1–M4 里程碑与验收点
├── .github/workflows/ci.yml      # CI：rust(fmt/clippy/test + postgres) + web(check/build)
├── LICENSE                       # MIT
└── NOTICE                        # 上游归属声明
```

## 路线图

M1 可行走骨架（已被 M2 取代，保留为终端探针）→ **M2 纵向切片（已交付）** → M3 connector ingress / 审批 / 文件 / 终端 / 分享 / 管理台 → M4 Capacitor Android App（其后 iOS）。
每个里程碑的交付物与验收点见 [docs/roadmap.md](./docs/roadmap.md)。

## 许可

本项目以 [MIT](./LICENSE) 许可发布。对上游项目 anywhere-labs/Agents-Anywhere 的归属与致谢见 [NOTICE](./NOTICE)。

## English

Farwork is an open-source, cross-device agent workbench: open a real agent session on a remote machine straight from your browser, with no inbound ports and no public IP. A small always-on Connector dials out; the hub persists sessions and their timeline in PostgreSQL and pushes deltas over WebSocket.

This repository is an **unofficial** rewrite of [anywhere-labs/Agents-Anywhere](https://github.com/anywhere-labs/Agents-Anywhere) in **Rust + Svelte**. It shares no source code with upstream and only borrows the product shape; upstream is MIT-licensed (see NOTICE). We are not affiliated with or endorsed by the upstream maintainers.

Status: the M2 vertical slice is delivered — login, connectors/projects/sessions, timeline rendering, sending messages, WebSocket deltas and cursor catch-up. Connector ingress, real agent execution, approvals, files, terminal, sharing and the admin console are not implemented yet and answer `501`. Everything else (roadmap, protocol, env vars) is in [docs/](./docs). Licensed under MIT.
