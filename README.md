# Farwork

**跨设备的开源 Agent 工作台。**

在任意设备的浏览器里打开远端设备上的真实终端，像坐在那台机器前一样与 Agent 协作。远端设备只需跑一个常驻 Connector 主动出站，不需要公网 IP，也不需要端口映射。

## 与上游项目的关系

Farwork 是 [anywhere-labs/Agents-Anywhere](https://github.com/anywhere-labs/Agents-Anywhere) 的**非官方**重写项目：

- **技术栈重写**：上游是 TypeScript 单体；本仓库服务端与 Connector 用 **Rust**，Web 前端用 **Svelte**。
- **代码不共享**：本仓库是独立实现，不包含、不复制上游源代码，只借鉴产品形态与交互概念。
- **致谢**：上游以 MIT 许可发布，归属声明见 [NOTICE](./NOTICE)。
- **非隶属关系**：本项目与上游维护者无隶属、赞助或背书关系，不代表上游立场。

## 当前状态

里程碑 **M1「可行走骨架」**，目标链路：Connector 上线 → 设备出现在设备列表 → 浏览器里对远程设备打开真终端并可交互。

- 已有：`GET /api/health`、`GET /api/devices`、`/ws/connector`、`/ws/client`，以及端到端 `terminal.*` 帧链路。
- 设备注册表是**内存态**：进程重启即丢失设备记录，M2 落 SQLite（取舍说明见 [docs/architecture.md](./docs/architecture.md)）。
- **Web 优先**：本轮只有 Web 端；Android App 计划用 **Capacitor** 包装同一套静态 Web 产物（M4，尚未实现），iOS 复用同一产物。

路线图：[docs/roadmap.md](./docs/roadmap.md)。

## 架构

三方结构：`farwork-server` 是唯一中枢，Connector 与浏览器都主动连它。

```
 +-----------------------+      HTTP /api/*        +--------------------------------+
 |  Browser UI           |      WS   /ws/client    |  farwork-server      :7300     |
 |  apps/web :5173       |<----------------------->|  Axum: REST + WS hub           |
 |  xterm.js + device    |  (Vite dev proxy -> 7300)|  in-memory registry:          |
 |  list                 |                         |  devices + streams routing     |
 +-----------------------+                         +----------------+---------------+
                                                                    ^
                                                                    |  WS /ws/connector?token=...
                                                                    |  (device dials out; no inbound port)
                                                    +---------------+---------------+
                                                    |  farwork-connector            |
                                                    |  spawns a PTY per             |
                                                    |  terminal.open, relays        |
                                                    |  terminal.* frames            |
                                                    +---------------+---------------+
                                                                    |
                                                                    v
                                                          [ remote device shell / PTY ]
```

- `farwork-server`：Axum 服务，同时提供 REST 与两个 WS 端点，维护设备表与 stream 到连接的映射，是唯一的帧转发点。
- `farwork-connector`：部署在远端设备的常驻进程，主动连 `/ws/connector`，收到 `terminal.open` 就为本机起 PTY，双向转发 `terminal.data`。
- `apps/web`：浏览器侧 UI，开发期经 Vite 代理访问 `/api` 与 `/ws`，用 xterm.js 渲染终端。

协议逐字段定义（REST 表 + 两个 WS 帧表）见 [docs/architecture.md](./docs/architecture.md)。

## 快速开始

前置：Rust stable 工具链、Node.js 24、pnpm 11.19.0（`apps/web` 锁文件由该版本生成，见 `apps/web/node_modules/.modules.yaml`；CI 固定同版本）。

三个终端，依次起服务端、Connector、Web：

```bash
# 终端 1：服务端，监听 7300
cargo run -p farwork-server
```

```bash
# 终端 2：Connector，以本机身份上线
cargo run -p farwork-connector
```

```bash
# 终端 3：Web 开发服务器，监听 5173
cd apps/web && pnpm install && pnpm dev
```

然后浏览器打开 <http://localhost:5173>，设备列表里应出现本机，点进去即可打开终端。

端口与环境变量：

| 项 | 值 | 说明 |
| --- | --- | --- |
| server 端口 | `7300` | 服务端 REST + WS |
| web dev 端口 | `5173` | Vite dev server，代理 `/api` 与 `/ws`（`ws: true`）指向 `127.0.0.1:7300` |
| `FARWORK_TOKEN` | 可选 | 设置后 REST 需 `Authorization: Bearer <token>`，WS 需 `?token=<token>`；未设置则开发模式放行并在启动时打印 warn |

## 目录结构

```
farwork/
├── Cargo.toml                  # workspace 根
├── rust-toolchain.toml
├── crates/
│   ├── farwork-protocol/       # 两端共享的线协议类型（帧 + REST 模型）
│   ├── farwork-server/         # Axum 服务：REST + 两个 WS 端点 + 内存注册表
│   └── farwork-connector/      # 远端设备常驻进程：PTY + 帧转发
├── apps/
│   └── web/                    # SvelteKit + adapter-static 前端（pnpm 管理，xterm.js 终端）
├── docs/
│   ├── architecture.md         # 端口/环境变量、协议表、组件职责、M1 取舍
│   └── roadmap.md              # M1–M4 里程碑与验收点
├── .github/workflows/ci.yml    # CI：rust(fmt/clippy/test) + web(check/build)
├── LICENSE                     # MIT
└── NOTICE                      # 上游归属声明
```

## 路线图

M1 可行走骨架（本轮）→ M2 ACP 会话·审批·文件浏览 → M3 多设备中继与自托管鉴权 → M4 Capacitor Android App（其后 iOS）。
每个里程碑的交付物与验收点见 [docs/roadmap.md](./docs/roadmap.md)。

## 许可

本项目以 [MIT](./LICENSE) 许可发布。对上游项目 anywhere-labs/Agents-Anywhere 的归属与致谢见 [NOTICE](./NOTICE)。

## English

Farwork is an open-source, cross-device agent workbench: open a real terminal on a remote machine straight from your browser, with no inbound ports and no public IP. A small always-on Connector dials out to the server; the web UI subscribes to the device list and attaches terminal streams.

This repository is an **unofficial** rewrite of [anywhere-labs/Agents-Anywhere](https://github.com/anywhere-labs/Agents-Anywhere) in **Rust + Svelte**. It shares no source code with upstream and only borrows the product shape; upstream is MIT-licensed (see NOTICE). We are not affiliated with or endorsed by the upstream maintainers.

Status: milestone M1 (walkable skeleton) — Connector online, device list, interactive remote terminal. Web first: an Android app is planned for M4 by wrapping the same static web build with Capacitor (iOS reuses the same output); no mobile app exists yet. Licensed under MIT.
