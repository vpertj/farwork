# Farwork 路线图

M1 是本轮范围；M2–M4 为方向性规划，验收点会随实际约束收敛。技术栈既定：Rust（server / connector）+ Svelte（web），移动 App 用 Capacitor 包装静态 Web 产物并后置到 M4。

## M1 · 可行走骨架（本轮）

**目标**：Connector 上线 → 设备出现在列表 → 浏览器里对远程设备打开真终端并可交互。

交付物：

- Cargo workspace + 三个 crate：`farwork-protocol`（线协议类型）、`farwork-server`、`farwork-connector`。
- `apps/web`：Svelte + Vite + xterm.js，设备列表与终端页。
- REST：`GET /api/health`、`GET /api/devices`。
- WS：`/ws/connector`、`/ws/client`，含 `devices.watch` 与 `terminal.*` 全帧链路。
- 内存注册表（设备表 + stream 路由），`FARWORK_TOKEN` 可选鉴权。
- 文档与 CI：`README.md`、`docs/architecture.md`、`docs/roadmap.md`、`.github/workflows/ci.yml`。

验收点：

- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿。
- `pnpm run check` 与 `pnpm run build` 在 `apps/web` 通过。
- 三终端启动后，`GET /api/health` 返回 `{"status":"ok","version":"0.1.0"}`；`GET /api/devices` 中出现本机且 `connected: true`。
- 浏览器 <http://localhost:5173> 打开设备终端，键入命令有正确回显；改变窗口大小后 PTY 列宽跟随（`terminal.resize` 生效）。
- 关闭终端页或 WS 断开后，connector 侧 PTY 进程被回收，不留僵尸进程。
- 设备断连（杀掉 connector / 断网）时，在途终端以 **exit code `-1`** 结束，UI 明确显示「连接已断开」而不是当成正常退出；设备列表同步变为 `connected: false`。
- 设置 `FARWORK_TOKEN` 时，无 token / 错 token 的 REST 返回 401、WS 握手失败；未设置时放行并在启动日志中出现 warn。
- CI 的 `rust` 与 `web` 两个 job 在 push 到 main 与 PR 上都通过。

## M2 · ACP 会话、审批与文件浏览

**目标**：从"一个终端"升级为"一个带审批闸门的 Agent 会话"。

交付物：

- ACP（Agent Client Protocol）会话接入：会话创建、消息流、工具调用事件的结构化呈现，不再把 Agent 交互降级成裸终端。
- 审批闸门：Agent 的敏感工具调用（写文件、执行命令、网络访问）在服务端拦截，前端弹出审批卡片，用户同意/拒绝回传后才放行。
- 文件浏览：远端设备目录树、文件内容预览、下载；写操作一律走审批闸门。
- 持久化：内存注册表迁 SQLite（`devices` 表 + 会话/流元数据），协议字段保持不变。
- 审计日志：审批决策与工具调用落库，可按设备/会话检索。

验收点：

- 一次 ACP 会话能完成"提问 → Agent 提议工具调用 → 前端审批 → 执行 → 结果回填"的完整闭环，全程有事件记录。
- 拒绝审批时工具调用不执行，Agent 收到明确的拒绝原因并继续会话。
- 服务端重启后，设备列表、历史会话元数据与审批审计仍可查；`connected` 由活动连接推导，不出现脏状态。
- 文件浏览对超大文件与二进制文件不阻塞、不整块读入内存。
- 契约测试覆盖所有帧类型，协议字段与 M1 文档保持一致（新增帧需同步更新 `docs/architecture.md`）。

## M3 · 多设备中继与自托管鉴权

**目标**：从"单机开发可用"到"一个小团队自托管可用"。

交付物：

- 多设备中继：多 connector 并发在线下的稳定路由、连接复用、断线重连与流迁移；单实例可承载多设备多路终端。
- 自托管鉴权：账号体系/多用户 token、设备归属（谁能看哪台设备）、token 轮换与最小权限；`FARWORK_TOKEN` 退化为开发模式的便捷开关。
- 部署形态：单二进制 + SQLite 的自托管包、可选 TLS 终止（反向代理或内置）与 Docker 镜像。
- 可观测性：结构化日志、`/api/health` 之外的运行指标、连接与流数量。

验收点：

- 两台设备同时在线，同一浏览器开两路终端互不串流；杀掉其中一个 connector 后，其流在可接受时间内被清理且另一路不受影响。
- 多用户场景下，用户 A 无法订阅或打开用户 B 的设备（WS 与 REST 都拒绝）。
- 单实例在压测下（多路并发终端）不出现内存无界增长，断开后资源回收干净。
- 自托管部署文档跑通：一条命令起服务，反向代理下 WS 升级正常。

## M4 · Capacitor Android App（其后 iOS）

**目标**：用 [Capacitor](https://capacitorjs.com/docs/) 把同一套 Web 体验装进原生容器，Android 先行，移动端可随手接管远端会话。

技术栈：

- 复用 `apps/web` 的**静态 SPA 构建产物**：`@sveltejs/adapter-static`（`fallback: 'index.html'`，`ssr = false`）产出到 `apps/web/build`，Capacitor 只做原生容器与桥接，不重写 UI。
- Android 先行；iOS 复用**同一套 Web 产物**与同一套 JS 桥接代码，只多一层平台工程。
- 与 Web 共用同一套 protocol 客户端（`apps/web/src/lib` 中的帧编解码与连接逻辑），不复制一份。
- **API base 运行时可配置**：App 与 server 非同源，不能依赖 Vite dev proxy，必须走完整 URL；沿用现有 `PUBLIC_FARWORK_API` 约定（`apps/web/.env.example` 中"留空 = 同源"），App 内置设置页可覆盖并在本地持久化。

交付物：

- `apps/mobile`：Capacitor 工程（`capacitor.config.ts` + `android/` 平台目录），`webDir` 指向静态构建产物。
- 运行时配置层：API base 与 token 的设置页 + 本地持久化（Capacitor Preferences / 安全存储），与 Web 侧读取逻辑共用。
- 移动端交互适配：软键盘与终端尺寸联动（复用 `terminal.resize` 帧）、手势滚动回滚缓冲、可读字号。
- 原生能力接入：审批请求与会话结束的系统通知、前后台切换后的自动重连。
- 分发：签名 APK/AAB 构建流程与安装说明。

验收点：

- Android 模拟器/真机安装后，在设置页填入自建 server 的完整 URL（非同源）与 `FARWORK_TOKEN`，保存后重启 App 配置不丢。
- 连接成功后设备列表可见，且与 Web 端看到的是同一批设备（同一 `GET /api/devices` 结果）。
- 在 App 内对某台设备打开终端，键入命令有正确回显，输出按 base64 帧正常渲染（可输入/可输出）。
- 弹出软键盘后终端重新计算行列并发出 `terminal.resize`，PTY 列宽跟随（不出现错行）。
- 切后台再回前台，会话自动重连且回滚缓冲内容不丢；无有效 token 时给出明确错误提示而不是白屏。
