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

## 当前状态：M3（计划项已全部交付）

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
- **文件浏览（只读，M3）**：`POST /api/v2/connectors/{id}/fs/list` 与 `/fs/readText` 经 connector 中继读设备上的目录与文本文件；会话页右侧有文件面板（浏览、面包屑、文本预览）。**沙箱**：所有路径必须落在 agent 的 `--workdir` 内（`canonicalize` 后按路径组件判前缀，`--workdir` 不可解析时一律拒绝而不是放开），符号链接逃逸、`../` 上跳、前缀陷阱都回 `403`；单文件上限 1 MiB，超限 `413` 而不是截断。
- **分享（M3）**：`POST /api/v2/sessions/{id}/shares` 发布一条分享（`scope:"session"` 整条会话、`"message"` 单条），`GET /api/v2/public/shares/{id}` 是**公开页**——**无需登录**，`Authorization` 带什么都一样；`DELETE /api/v2/sessions/{id}/shares/{shareId}` 撤销（软删，撤销后公开页 `404`）。**公开页看到的是发布那一刻的副本**，不是实时视图：分享之后原会话继续发消息，链接里的内容一字不变。副本按白名单组装（`id`/`type`/`status`/`role`/`content`/`orderSeq`/`itemTime`），`connectorId`/`workspacePath`/`sessionId`/`source`/`hash` 在结构上不会出现在里面；空会话分享回 `422 there is no content to share`。语义与源码见 [docs/architecture.md](./docs/architecture.md) §3.8。
- **设备终端（M3）**：`POST /api/v2/connectors/{id}/terminals` 在设备上开一个 shell，`GET .../terminals/{terminalId}/stream?ticket=...` 用一次性票据接一条 WebSocket PTY 流（`terminal.input` / `terminal.resize` / `terminal.close` ↔ `terminal.opened` / `terminal.data` / `terminal.exit` / `terminal.error`），会话页右侧有 xterm.js 终端面板。**cwd 的沙箱只约束「起始目录」**：起点必须在 `--workdir` 内（越界直接 `permission_denied`，连 `terminal.opened` 都不会有），但 shell 起来之后能去哪由 shell 自己决定——不要把它理解成「终端被关在 workdir 里」。另外 `201` 只表示已落库并已下发，PTY 是否起来看 `terminal.opened`；agent 的拒绝会以 `terminal.error` 回来并落库，晚连上来的浏览器仍能看到原因。
- **管理台（M3）**：`GET`/`POST /api/v2/admin/users`、`PATCH /api/v2/admin/users/{id}`、`GET`/`PATCH /api/v2/admin/settings`——一句话：**仅 `role=admin` 可见**（权限门每次重读 `users` 行，被停用的 admin 也 `403`），**列表不含哈希**（响应是七键白名单投影，哈希材料永不出现在响应里），**不能把自己锁死**（`self_disable`/`last_admin`/`self_demote` 三种 `409 admin_lockout`，活跃 admin 数在同一事务内 `select … for update` 取，并发降级不可能都成功）。两个开关别混：`registrationOpen` 是**真开关**（关掉后 `register` 403），`emailVerificationRequired` 只落库并如实上报（本 build 无邮件通道，`/auth/email-code` 仍 501），**不拦注册**。语义与行号见 [docs/architecture.md](./docs/architecture.md) §3.9。
- **配对码（设备不再手抄 token）**：Web 的「连接设备」页（`#/connect`，`apps/web/src/components/ConnectDevicePage.svelte`）生成 8 位一次性码 `XXXX-XXXX`（`POST /api/v2/pairing/start`，**需登录**，`201`），设备侧 `farwork-agent pair --hub <url> --code <code>` 拿它换 token（`POST /api/v2/pairing/claim`，**免鉴权**——码就是凭证），页面轮询 `GET /api/v2/pairing/{id}` 显示 `pending`/`claimed`。规则：库里**只存带密钥的摘要**（`sha256("farwork-pairing-v1:{FARWORK_HUB_SECRET}:{规范化码}")`，8 位码只有约 39 bit，裸摘要能离线反推）、**一次性**（同码二次 `409 code_already_claimed`）、TTL 默认 600s（60..=3600）、按来源地址每分钟 10 次（`429`）、错码 `404`、过期 `410`；`GET {id}` 对未知或别人的 id 回 `status:"unknown"` 而不是 `404`（不泄露存在性）。`pair` 成功后 `~/.farwork/agent.json`（0600、目录 0700）多出 `hub`/`connectorId`/`connectorToken` 三键，**之后裸 `farwork-agent` 直接复用**；claim 的 404/409/410/429/连不上/旧 hub 裸 404 各给一句可操作文案。语义见 [docs/architecture.md](./docs/architecture.md) §3.12。
- **`GET /api/v2/meta`（免鉴权，hub 自报地址）**：回 `{hubUrl, version, serverTime, scheme, port}`。页面用它拼配对命令，而不是用页面自己的 origin——dev（5173 代理到 7300）与反代下这两者并不相同，用错的后果是「设备永远离线」。端口取 **hub 自己的监听端口**（`Host` 里的端口不用，那是代理的），scheme 只取自 `X-Forwarded-Proto` 首值；`FARWORK_HUB_PUBLIC_URL` 优先级最高，**写错则 hub 启动失败**（这个串会被逐字复制进每条配对命令）。语义见 [docs/architecture.md](./docs/architecture.md) §3.13。
- **运行中干预（M3，Ingress Protocol v1.3）**：会话跑到一半还能**补充指令**——`POST /api/v2/sessions/{id}/runtime/steer` 回 `202`（「已投递给设备」，执行结果由设备写进 timeline，不靠 HTTP 回答）；**模型 / 权限选择**——`GET .../runtime/catalogs/{model|permission}` 取目录、`PATCH .../runtime/selections` 提交，**只有帧真的送到设备才回 `200`**；**斜杠命令**——`GET .../runtime/commands` 取命令清单，输入框里以 `/` 开头时给出建议。能力本身来自设备上报（`session.capabilities`）：`GET .../runtime/capabilities` 的四个布尔**没上报就是全 `false`**，页面据此决定这几个控件出不出现（**有 catalog 才显示选择器**、`state=running` 且有 `steer` 才显示补充指令框，只有 `selections` 能力为真才显示模型/权限下拉）。对着真 `codex` 时如实只报 `interrupt`——协议通了不等于后端支持。
- **文件与终端改用会话命名空间**：文件面板与终端面板现在走 `POST /api/v2/sessions/{id}/files/list|readText`、`POST /api/v2/sessions/{id}/terminal` 与 `GET /api/v2/sessions/{id}/terminal/{terminalId}/stream`——**同一个会话页不再需要先知道 device id**。这两组与 connector 级的 `/connectors/{id}/fs/*`、`/connectors/{id}/terminals*` **共用同一条中继**（没有第二套实现），差别只是「设备与工作目录由会话解析出来」；终端行绑定 `session_id`（`migrations/0005` 在既有表上加可空列，没建第二张表）。**别人的会话一律 `404`**（不是 `403`：不泄露存在性），终端取票与握手两关都校验归属。语义见 [docs/architecture.md](./docs/architecture.md) §3.5/§3.6 与 §5.13；**本仓 REST 形状不是上游的镜像**，逐条差异见 §9。

- **DSH 作为第二个运行时（M3）**：`farwork-agent --runtime dsh` 连的是**你这台机器上已经在跑的 DSH**——需要先在 DSH 里**启用 agents-anywhere 桥插件**（它会写出 `~/.agents-anywhere/dsh-bridge/endpoint.json`，可用 `--dsh-endpoint` 指定别的路径）。DSH 不是 agent 拉起的子进程：桥没开就**连不上，agent 直接启动失败**（退出码 1），不会假装可用。接上以后会话照常跑：能力（steer/interrupt/selections）、审批与问题通知都来自桥的**上报值**，模型/权限下拉来自桥的两份 catalog（真机只读探测：models 2 条、permissions 4 条 `read-only`/`workspace-write`/`danger-full-access`/`Auto review`）。审批按钮用运行时自己的 id（`actionId`）回答，旧的三值 `decision` 仍兼容。**也可以改用本仓自研的桥插件** `packages/dsh-plugin`（`@farwork/dsh-bridge`）：它把 rendezvous 换到 `~/.farwork/dsh-bridge/endpoint.json`（锁按目录取，与上游并存而不互相踢），agent 的发现顺序是 `--dsh-endpoint` → `~/.farwork/dsh-bridge/endpoint.json` → 上游的两条，所以装好插件后裸 `--runtime dsh` 就能找到它；启动日志里的 `dsh_endpoint_source` 会说明用的是自研还是上游那个桥。自研桥 v1 **不含审批/问答**（DSH 没有公共入口，只能反射内部 `typertGateway`），降级方式是审批与提问都在 DSH 自己的 Web UI 里回答。DSH checkpoint 现在**落盘**在 `$FARWORK_HOME/dsh/sync/checkpoints/`（目录 0700、文件 0600、临时文件 + rename 原子替换），重连可以续传而不是每次全量重放。安装/卸载与已知边界见 [docs/architecture.md](./docs/architecture.md) §11。

**还不能做**（诚实清单）：

- **codex 的成功路径未验证**：assistant 文本增量、工具执行、真实审批往返都还没有跑通过，原因是本机没有 Codex 凭据。已验证的是「codex 起得来、401 如实落到 timeline」和「用 `--runtime echo` 把审批管道跑通」。
- **`file_change` / `permissions` 两种 notice kind 未在联调中出现**：echo 后端只发 `command_execution`，codex 的这两个审批分支因无凭据从未被触发；页面上的「文件变更」「权限申请」标签只是代码就位，没有联调证据。
- **审批卡在浏览器里的端到端流程没有留证据**：已验收的是 `pnpm run check`（0 error）/ `pnpm run build` 通过与「hub 不乐观 resolve」的代码层核对，不是「点批准 → 卡片转已解决」的实机记录。
- **DSH 的 `session.commands` 没有兑现**：真桥会报 `session.commands: true`，但本仓**没有实现 `session.executeCommand`**，所以能力位固定报 `false`、`GET .../runtime/commands` 对 DSH 会话是空表——一个填得满、点了不执行的命令框比不出现的按钮更糟。同理：**还没有在真 DSH 上建过会话**（`createAndStart` → `batch` → `ack` → `item` → `notice` → `respondInteraction` 这条链只在脚本化的假 bridge 上验过），`userQuestions` 也没有对着真 DSH 问答验证过。
- **仍有 35 条路径是 `501 not_implemented`**（不会伪造成功）：账号级分享清单 `/shares`、设备运行时管理 `/runtimes*` `/devices*` `/connectors/{id}/runtimes*`、OAuth / 邮件码 / 移动端扫码登录，以及 `/announcements*`、`/agents*`、`/usage*`——**管理台只交付了「用户目录 + 实例开关」**，这三组仍在表里。**配对码已实现**（`/pairing/start`、`/pairing/claim`、`/pairing/{id}`），因此 `/pairing*` 已从这份清单里移出。**会话命名空间已补齐**：`/sessions/{id}/files/list`、`/files/readText`、`/terminal`、`/terminal/{terminalId}/stream` 与 `runtime/capabilities|catalogs/{catalog}|commands|steer|selections` 都实到了；它们复用**同一套** `fsrelay`/`termrelay`，只多一层「由 session 解析出 connector + workspace」。文件浏览**只有只读**的 list/readText：写操作、大文件/二进制流式仍未实现；终端**只有交互式 PTY**：没有 scrollback 持久化、没有接管别人已开的终端，`farwork-server`（M1 探针）也还没退场。

当前端点数：`/api/v2` 下 **63 个已实现的 method+path**（**53 条** `route()` 注册，含文件浏览、设备终端、设备/会话两种终端、会话运行时控制 4 条、分享三条、配对码三条、`GET /meta` 一条与管理台 3 次注册，也含 §5 单独记录的 `GET /connector/ws`）+ `GET /healthz`；另有 **35 条路径**在 `routes/unsupported.rs` 里统一回 `501`（分享把 `/sessions/{id}/shares` 与 `/sessions/{id}/shares/{*rest}` 两条从表里移走了 49 → 47，管理台再把 `/admin` 与 `/admin/{*rest}` 两条移走 47 → 45，会话命名空间的 8 条 —— `/sessions/{id}/terminal`、`/terminal/{*rest}`、`/files`、`/files/{*rest}` 与 `runtime/{steer,commands,selections,catalogs/{catalog}}` —— 再减到 37，配对码的 `/pairing` 与 `/pairing/{*rest}` 两条再减到 35）。`/connectors/{id}/pairing` 仍在 `501` 表里：那是每台设备的运行时开关，与配对码不是同一件事。逐条清单见 [docs/architecture.md](./docs/architecture.md) §3。

路线图：[docs/roadmap.md](./docs/roadmap.md)。

## 架构（M3）

```
 +-----------------------------+        HTTP /api/v2           +-----------------------------------+
 |  Browser (apps/web :5173)   |        WS   /dashboard/ws     |  farwork-hub            :7300     |
 |  SvelteKit 静态 SPA         |<----------------------------->|  axum: REST + 4 个 WS 端点         |
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

- `farwork-hub`：`/api/v2` 服务端。REST + 4 个 WS 端点（dashboard / session / **connector ingress** / **设备终端流**）+ `POST /ws-ticket`，PostgreSQL 持久化，启动时自动跑迁移（**7 个迁移、13 张表、18 个索引**；`0005` 只给 `terminals` 加一列、`0006` 只给 `session_notices` 加一列，都不建表、不加索引；`0007` 建 `pairings` 表与两个索引，见配对码）。
- `farwork-agent`：M3 的设备侧常驻进程。主动出站连 hub，按 `session.start` 拉起 `codex app-server --stdio`，把 JSON-RPC 翻成 ingress 帧；身份落盘 `~/.farwork/agent.json`（0600）。
- `farwork-timeline`：timeline 的**纯域逻辑**（排序、`seq:N` 事件游标与补拉、`content_hash` 去重、每会话写栅栏），无 IO、无时钟、可用内存后端单测。hub 只负责持久化与传输。
- `farwork-server` / `farwork-connector` / `farwork-protocol`：**M1 遗留**，保留为终端探针，尚未与 hub 打通。
- `apps/web`：SvelteKit + `adapter-static` 的静态 SPA；dev 期经 Vite 代理 `/api`、`/ws` 到 `127.0.0.1:7300`。

协议与端点的逐条定义以 [docs/architecture.md](./docs/architecture.md) 为唯一事实来源。

## 安装（设备端：一个二进制，不需要 Rust，也不需要源码）

`farwork-agent` 是单个可执行文件，由 [`.github/workflows/release.yml`](./.github/workflows/release.yml) 在推 `v*` tag 时构建，覆盖 macOS（Intel / Apple Silicon）、Linux（x86_64 / aarch64）与 Windows（x86_64，`.zip`）。

### macOS / Linux

```sh
curl -fsSL https://github.com/vpertj/farwork/releases/latest/download/install.sh | sh
```

`install.sh` 只做四件事：按 `uname -s` / `uname -m` 选资产 → 下载 `farwork-agent-<target>.tar.gz` 与 `SHA256SUMS` → **校验 sha256，不一致就拒绝安装** → 解包到 `~/.local/bin`（不存在则创建）。它不碰系统包管理器、不要 root、不依赖 Python/Node，可以重复执行：同一个 tag 重装就是覆盖安装，校验失败时旧的二进制**原样保留**。

装完在 Web 的「连接设备」页（`#/connect`）生成一个 8 位配对码，然后：

```sh
# 换成凭据后立刻开始连 hub（同一个进程接着跑 connector）
farwork-agent pair --hub https://<你的服务器> --code <配对码>

# 以后重启不用再带参数：hub 与 token 已在 ~/.farwork/agent.json
farwork-agent
```

也可以跳过配对码，直接用 `POST /api/v2/connectors` 响应里的 `connectorToken`（明文只出现这一次）：

```sh
farwork-agent --hub wss://<你的服务器> --token '<connectorToken>'
```

`install.sh` 认这几个环境变量：

| 变量 | 默认 | 用途 |
| --- | --- | --- |
| `FARWORK_VERSION` | `latest` | 装指定 tag，例如 `v0.1.0` |
| `FARWORK_INSTALL_DIR` | `$HOME/.local/bin` | 换安装目录（目录不在 PATH 上时会提示加 PATH） |
| `FARWORK_RELEASE_BASE_URL` | GitHub releases | 走镜像/内网地址；该地址下需要平铺 `install.sh`、`farwork-agent-*.tar.gz` 与 `SHA256SUMS` |

### Windows

没有 POSIX sh：从 release 页面下 `farwork-agent-x86_64-pc-windows-msvc.zip`，解压后把 `farwork-agent.exe` 放进 PATH（例如 `%LOCALAPPDATA%\Programs\farwork`），再用同样的 `pair` / `--token` 参数启动。

### 升级与卸载

升级＝重跑同一条 `curl ... | sh`。卸载＝删掉 `~/.local/bin/farwork-agent`；设备身份在 `~/.farwork/agent.json`（0600），删掉它会换一个新的 `agent_id`。

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

### 5. 单容器部署（hub 自己托管前端，不需要 nginx）

`Dockerfile` 把前端产物与 hub 打进**一个**容器：hub 带着 `FARWORK_HUB_STATIC_DIR=/srv/web` 启动，自己托管 SPA（`/` 与未知深链接回 `index.html`），同时照旧提供 `/api/v2/*`、各 WS、`/healthz`、`/install.sh`。

```bash
docker build -t farwork:dev .
docker run -d --name farwork -p 13080:7300 \
  -e DATABASE_URL='postgres://<user>@<db-host>:5432/farwork' \
  -e FARWORK_HUB_SECRET='<随机长串>' \
  -e FARWORK_HUB_PUBLIC_URL='https://farwork.example.com' \
  farwork:dev
curl -fsS http://127.0.0.1:13080/healthz && curl -fsS http://127.0.0.1:13080/ | head -3
```

- 运行阶段是 `debian:bookworm-slim`（**必须有 shell**，不是 distroless），非 root 运行，`HEALTHCHECK` 打 `/healthz`。
- 打 tag 后 `.github/workflows/docker.yml` 自动构建并推送 `ghcr.io/vpertj/farwork`（`latest` + git tag + `sha-<short>`，amd64/arm64 双架构）。
- 容器不终止 TLS：域名与证书交给反向代理（例如 1Panel 的网站/反向代理）。

> **端口冲突提醒**：`farwork-hub` 与 M1 的 `farwork-server` **默认都监听 7300**。两套后端不能同时起；要跑 M1 探针请显式 `--port`。

## 环境变量

| 变量 | 默认 | 用途 |
| --- | --- | --- |
| `DATABASE_URL` | `postgres://tianjun@127.0.0.1:5432/farwork` | hub 的 PostgreSQL 连接串。**库必须已存在**；表由内置迁移创建。连接失败则启动失败 |
| `FARWORK_HUB_PORT` / `PORT` | `7300` | 监听端口（`FARWORK_HUB_PORT` 优先）。命令行 `--port <n>` 覆盖两者 |
| `FARWORK_HUB_SECRET` | `agent-server-dev-secret` | 签名/令牌派生密钥。**未设置时打印 warn 并回退到开发密钥**；生产必须显式设置 |
| `FARWORK_HUB_SETUP_TOKEN` | 未设置 | 首个 admin 注册令牌。未设置时进程生成一个随机值并打到日志（30 分钟 TTL，过期轮换），从不经 HTTP 返回 |
| `FARWORK_HUB_DB_POOL` | `10` | sqlx 连接池最大连接数（非法值回退默认） |
| `FARWORK_HUB_PUBLIC_URL` | 未设置 | hub 对外自报的基地址（`GET /api/v2/meta` 的 `hubUrl`），优先级最高；**写错则 hub 启动失败**（这个串会被逐字复制进每条配对命令） |
| `FARWORK_HUB_STATIC_DIR` | 未设置 | **前端静态产物根**（`apps/web/build`）。设了 hub 就自己托管 SPA：`/` 与未知深链接回 `index.html`（`index.html` 发 `no-cache`，`_app/immutable/*` 长缓存），`/api/*` 的未知路径仍是 404 JSON；**未设置则完全不挂载**。目录不存在或缺 `index.html` 时**启动失败** |
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
│   ├── farwork-hub/              # /api/v2 服务端：axum + sqlx/PostgreSQL + 4 个 WS（含 connector ingress、设备终端流）
│   ├── farwork-timeline/         # timeline 纯域逻辑：排序、seq 游标、content_hash、写栅栏
│   ├── farwork-agent/            # M3 设备侧：连 hub + 驱动 codex app-server（真 connector）
│   ├── farwork-protocol/         # M1 遗留：M1 线协议类型（现仅 server/connector 使用）
│   ├── farwork-server/           # M1 遗留：7300 上的 M1 REST + /ws 终端骨架
│   └── farwork-connector/        # M1 遗留：PTY 转发，只连 farwork-server（不是 agent 的旧版本）
├── apps/
│   └── web/                      # SvelteKit + adapter-static SPA（pnpm 管理）
├── packages/
│   └── dsh-plugin/               # 自研 DSH 桥插件 @farwork/dsh-bridge（TypeScript/Cordis，非 cargo workspace 成员）
├── docs/
│   ├── architecture.md           # 当前架构、/api/v2 端点表、Connector Ingress（§5）、timeline 语义、M1 附录
│   └── roadmap.md                # M1–M4 里程碑与验收点
├── .github/workflows/ci.yml      # CI：rust(fmt/clippy/test + postgres) + web(check/build)
├── LICENSE                       # MIT
└── NOTICE                        # 上游归属声明
```

## 路线图

M1 可行走骨架（已被 M2 取代，保留为终端探针）→ **M2 纵向切片（已交付）** → **M3 connector ingress + agent（计划项已全部交付：后端与协议已实测，Web 审批卡、新建会话表单、文件浏览、设备终端、分享、管理台都已交付并验收）** → M4 Capacitor Android App（其后 iOS）。
每个里程碑的交付物与验收点见 [docs/roadmap.md](./docs/roadmap.md)。

## 许可

本项目以 [MIT](./LICENSE) 许可发布。对上游项目 anywhere-labs/Agents-Anywhere 的归属与致谢见 [NOTICE](./NOTICE)。

## English

Farwork is an open-source, cross-device agent workbench: open a real agent session on a remote machine straight from your browser, with no inbound ports and no public IP. A small always-on Connector dials out; the hub persists sessions and their timeline in PostgreSQL and pushes deltas over WebSocket.

This repository is an **unofficial** rewrite of [anywhere-labs/Agents-Anywhere](https://github.com/anywhere-labs/Agents-Anywhere) in **Rust + Svelte**. It shares no source code with upstream and only borrows the product shape; upstream is MIT-licensed (see NOTICE). We are not affiliated with or endorsed by the upstream maintainers.

Status: the M2 vertical slice is delivered (login, connectors/projects/sessions, timeline, sending messages, WebSocket deltas, cursor catch-up), and M3 is delivered — connector ingress (`/api/v2/connector/ws`, Ingress Protocol v1) plus `farwork-agent`, which drives a local `codex app-server` and relays approvals; `connectors.status` now reflects real presence. M3's planned feature blocks are all delivered, including the file browser, the device terminal, sharing and the admin console (user directory + instance switches, `role=admin` only, no hash material in responses, and no way to lock the instance out of its last admin). Session runtime control (`session.capabilities` / `session.steer` / `session.selections`, Ingress Protocol v1.3) and the session namespace (`/sessions/{id}/files*`, `/terminal*`, `runtime/{capabilities,catalogs,commands,steer,selections}`) are delivered on top of the same fs/terminal relays. Pairing codes are delivered as well: `POST /pairing/start` (logged in) mints a one-time `XXXX-XXXX` code, and `POST /pairing/claim` (unauthenticated — the code is the only credential) trades it for a connector token, so a device never has to be handed a `cxt_` token by hand. `GET /meta` (unauthenticated) reports the address the hub is actually reachable at, so a dev/proxy page origin never ends up inside a pairing command (`FARWORK_HUB_PUBLIC_URL` wins, and a malformed value refuses to start). The DSH bridge now ships in-repo as `packages/dsh-plugin` (`@farwork/dsh-bridge`), replacing the upstream plugin and its Python connector; it rendezvouses in `~/.farwork/dsh-bridge/` (the lock is per-directory, so the two can coexist) and v1 deliberately has no approval path. Still missing: the remaining 35 `501` paths (`/agents*`, `/usage*`, `/announcements*`, `/shares`, runtimes/devices, OAuth and email-code), and — importantly — the codex success path (assistant streaming, tool execution, a real approval round trip) is unverified because there are no Codex credentials on the test machine. Details in [docs/](./docs). Licensed under MIT.
