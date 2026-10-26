# AGENTS.md — Farwork 仓库级指令

> 给在这个仓库里干活的 AI agent（和接手的人）看的。动代码前读完本节；协议的唯一事实来源是 `docs/architecture.md`。

## 1. 项目与技术栈

Farwork = 跨设备 Agent 工作台：在浏览器里打开远端设备上的**真终端**，设备只需一个常驻 connector 主动出站，不需要公网 IP 或端口映射。

- Rust workspace：`crates/farwork-server`（axum）+ `crates/farwork-connector`（PTY 转发），共享类型在 `crates/farwork-protocol`。
- Web：`apps/web`，SvelteKit + `@sveltejs/adapter-static`（`ssr = false`、`fallback: 'index.html'`）的静态 SPA，pnpm 管理，xterm.js 渲染终端。
- Android App：计划用 **Capacitor** 包装**同一套**静态 Web 产物（`apps/web/build`），**尚未实现**，属 M4；iOS 复用同一产物。
- 上游关系：anywhere-labs/Agents-Anywhere 的独立重写，代码不共享；归属与致谢见 `NOTICE`。

## 2. 仓库地图（已用 `ls` 核实）

- `crates/farwork-protocol` — 两端共享的线协议类型（WS 帧 + REST 模型）。改协议从这里开始。
- `crates/farwork-server` — axum 服务：`GET /api/health`、`GET /api/devices`、`/ws/connector`、`/ws/client`，内存注册表与 stream 路由。
- `crates/farwork-connector` — 远端设备常驻进程：按 `terminal.open` 起 PTY，双向转发 `terminal.*` 帧。
- `apps/web` — SvelteKit 静态 SPA：设备列表 + 终端页；`apps/web/src/lib/protocol.ts`、`apps/web/src/lib/ws.ts`、`apps/web/src/lib/api.ts`、`apps/web/src/lib/base64.ts` 是浏览器侧协议客户端。
- `docs/architecture.md` — 端口/环境变量、REST 表、两个 WS 帧表、内存注册表取舍。
- `docs/roadmap.md` — M1–M4 里程碑与验收点（里程碑的唯一事实来源）。
- `.github/workflows/ci.yml` — CI 双 job：`rust`（fmt/clippy/test）、`web`（install/check/build）。
- 根目录：`Cargo.toml`（workspace）、`rust-toolchain.toml`、`README.md`、`NOTICE`、`LICENSE`。
- 未落盘：`apps/mobile`（M4 的 Capacitor 工程，目前不存在；先别引用它）。

## 3. 能跑的命令（单一事实来源，与 CI 一致）

工具链：Rust stable（见 `rust-toolchain.toml`）、Node 24、pnpm **11.19.0**（锁文件由该版本生成）。

Rust，在仓库根执行：

```bash
cargo run -p farwork-server          # 起 server，监听 7300
cargo run -p farwork-connector       # 起 connector（以本机身份上线）
cargo fmt --all -- --check           # CI 要过
cargo fmt --all                      # 本地重排
cargo clippy --workspace --all-targets -- -D warnings   # CI 要过
cargo test --workspace               # CI 要过
```

Web，在 `apps/web` 执行：

```bash
pnpm install --frozen-lockfile   # CI 要过
pnpm run check                   # svelte-kit sync + svelte-check
pnpm run build                   # vite build，静态产物到 apps/web/build
pnpm dev                         # vite dev --port 5173
pnpm run preview                 # 4173，预览静态产物（CI 不跑）
```

上面 6 条带 "CI 要过" 的命令与 `.github/workflows/ci.yml` 完全一致；提交前本地跑通。

### pnpm workspace 布局陷阱（已实测，别踩）

- `apps/web/pnpm-workspace.yaml` **存在**，内容只有两行：`allowBuilds:` / `  esbuild: true`；根目录**没有** `pnpm-workspace.yaml`。这是 pnpm 11 里 `allowBuilds` 唯一生效的位置。
- 副作用：**`apps/web` 自成 workspace root**。CI 的 `working-directory: apps/web` 正依赖这个布局。
- 别只加根的 `pnpm-workspace.yaml` 而不迁移：那会改变 `pnpm install` 在 `apps/web` 下的归属（包被提升到根、`apps/web` 退化为子包），现有布局与 CI 一起失效。
- 将来真要建根 workspace，必须两步同时做：把 `allowBuilds` 上移到根 `pnpm-workspace.yaml`，然后**删除** `apps/web/pnpm-workspace.yaml`。漏掉任一步，`pnpm install` 会以 `ERR_PNPM_IGNORED_BUILDS`（退出码 1）失败，CI 直接红。
- pnpm 版本必须与 CI 一致：**11.19.0**（见 `ci.yml` 的 `pnpm/action-setup`）。`apps/web/pnpm-lock.yaml` 由该版本生成，换版本可能改写锁文件。

## 4. 不可违反的约定

- **协议三处同改**：任何字段名/类型/取值变更，必须在同一个 PR 内同步 `crates/farwork-protocol`、`apps/web/src/lib/protocol.ts`、`docs/architecture.md`。少一处即为破坏契约。
- **base64**：`terminal.data` / `terminal.input` 的 `data` 恒为 **standard 字母表、带 `=` padding** 的 base64；两端都按**字节**处理，禁止假定 UTF-8（PTY 输出是任意字节）。
- **stream_id**：由发起方（浏览器）生成并全程复用；未知 `stream_id` 的帧丢弃即可，不要因此断开连接。
- **端口固定**：server `7300`；web dev `5173`，Vite 代理 `/api` 与 `/ws` → `127.0.0.1:7300`（`ws: true`）。
- **密钥**：`FARWORK_TOKEN` 只从环境变量读；未设置 = 开发模式放行并 warn。禁止硬编码或提交任何密钥，`.env` / `.env.*` 不入库（`.env.example` 可入库，`apps/web` 用 `PUBLIC_FARWORK_API`，留空表示同源）。
- **Rust 质量线**：`cargo fmt` 必过；clippy `-D warnings` 零告警。新写的错误路径用 thiserror/anyhow 收敛，不要新增 `unwrap()`/`expect()`（启动路径与测试除外）。现状提醒：workspace 目前**尚未**引入 thiserror/anyhow，`crates/` 里现存约 50 处 unwrap/expect，是要逐步收敛的欠债，不是可照抄的范例。
- **TS 质量线**：`strict: true`；禁止 `any`，外部输入一律按 `unknown` 收窄（参照 `apps/web/src/lib/protocol.ts` 的 `isRecord`/`asString`/`asNumber` 风格）。
- **注释**：只解释非显然的「为什么」（取舍、坑、协议原因），不写逐行「是什么」。
- **提交门槛**：第 3 节命令本地跑通 + CI 两个 job 绿，才算完成。

## 5. 当前状态与下一步

- 里程碑：**M1 可行走骨架**已实现并通过端到端验收（真 server + connector + 浏览器 WS 客户端：设备上线 `connected: true`、`echo` 回显、设备断连后 `connected: false` 且在途流以 `exit code -1` 结束）。设备注册表是**内存态**，进程重启即丢设备记录。
- 里程碑事实来源是 `docs/roadmap.md`：M1 骨架 / M2 ACP 会话·审批·文件浏览 / M3 多设备中继与自托管鉴权 / M4 Capacitor Android App（其后 iOS）。
- App 属 M4，本轮不做；不要提前引入 Capacitor、移动平台目录或原生桥接代码。
- **已实测的边界事实（M2 的输入，不是缺陷）**：设备 id 在每次 connector 进程启动时重新生成（`crates/farwork-connector/src/main.rs:28-30`，`Uuid::new_v4()`，进程内复用于重连，原注释即 "Identity is per process run"），**未持久化**。因此重启 connector 后设备列表里会多出一条新记录（旧记录留在列表里但 `connected: false`）。M2 若需要稳定身份，必须把 device id 落盘。

## 6. 给 agent 的三条纪律

1. 先读 `docs/architecture.md` 再动协议。不要边写实现边发明字段——契约优先于手感。
2. 新增帧/字段的顺序是：协议类型（`farwork-protocol`）→ 文档（`docs/architecture.md`）→ 两侧实现。顺序反了就会产出两份互不兼容的真相。
3. 不引入新依赖（数据库、前端 UI 框架、状态库、原生壳）而不在 PR 里说明理由与替代方案；M1 明确用内存注册表，不是遗漏。
