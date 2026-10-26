# Farwork 架构与线协议

本文是 M1 的**共享线协议契约**：`farwork-server`、`farwork-connector`、`apps/web` 三方必须严格一致，任何一方不得自行改名或改字段。

## 1. 组件职责

| 组件 | 位置 | 职责 | 对外 |
| --- | --- | --- | --- |
| `farwork-protocol` | `crates/farwork-protocol` | 两端共享的线协议类型：REST 响应模型、WS 帧枚举、base64 编解码约定。**唯一的协议真相源** | 库 |
| `farwork-server` | `crates/farwork-server` | 中枢。提供 `GET /api/health`、`GET /api/devices`、`/ws/connector`、`/ws/client`；维护设备表与 stream 路由；在两个 WS 通道之间转发 `terminal.*` 帧 | 端口 7300 |
| `farwork-connector` | `crates/farwork-connector` | 部署在远端设备的常驻进程。主动出站连 `/ws/connector`，上报设备信息；收到 `terminal.open` 就在本机起一个 PTY，双向转发 `terminal.data` | 出站连接 |
| `apps/web` | `apps/web` | 浏览器 UI。经 Vite 代理访问 `/api` 与 `/ws`；订阅设备列表；用 xterm.js 渲染终端并回传键入 | dev 端口 5173 |

数据流只有一条：**浏览器 ↔ server ↔ connector ↔ 远端 PTY**。server 不做内容解析，只按 `stream_id` 转发。

## 2. 端口与环境变量

| 端口 | 用途 | 说明 |
| --- | --- | --- |
| `7300` | server | REST + 两个 WS 端点，默认监听 |
| `5173` | web dev server | Vite 代理 `/api` 与 `/ws` → `127.0.0.1:7300`，`ws: true` |

| 环境变量 | 默认 | 行为 |
| --- | --- | --- |
| `FARWORK_TOKEN` | 未设置 | **已设置**：REST 需 `Authorization: Bearer <token>`，WS 需 query `?token=<token>`，不匹配则 401 / 握手失败。**未设置**：开发模式放行全部请求，并在启动时打印一条 warn |

## 3. REST

所有 REST 响应为 `application/json`。

| 方法 | 路径 | 鉴权 | 成功响应 |
| --- | --- | --- | --- |
| `GET` | `/api/health` | 设置了 `FARWORK_TOKEN` 时需 Bearer | `200` + 健康体 |
| `GET` | `/api/devices` | 同上 | `200` + 设备列表 |

### 3.1 `GET /api/health`

```json
{"status":"ok","version":"0.1.0"}
```

| 字段 | 类型 | 取值 / 说明 |
| --- | --- | --- |
| `status` | string | 固定 `"ok"` |
| `version` | string | 服务端版本，M1 为 `"0.1.0"` |

### 3.2 `GET /api/devices`

```json
{"devices":[{"id":"<uuid>","name":"<string>","platform":"macos|linux|windows","connected":true,"last_seen_ms":1700000000000}]}
```

| 字段 | 类型 | 取值 / 说明 |
| --- | --- | --- |
| `devices` | array | 设备数组，无设备时为空数组 |
| `devices[].id` | string (uuid) | 设备 id，由 connector 生成并稳定复用 |
| `devices[].name` | string | 设备名，正常为用户可读的主机名 |
| `devices[].platform` | enum string | 仅 `macos` / `linux` / `windows` |
| `devices[].connected` | bool | 当前是否有活跃 connector 连接 |
| `devices[].last_seen_ms` | number | 毫秒级 Unix 时间戳（示例 `1700000000000`） |

## 4. WS：Connector 通道 `/ws/connector?token=...`

connector 侧是**主动出站**的长连接；server 不接受来自 connector 的入站端口。

| 方向 | `type` | 字段 | 说明 |
| --- | --- | --- | --- |
| C→S | `hello` | `device`: {`id`: uuid, `name`: string, `platform`: `macos`\|`linux`\|`windows`} | 连接建立后发送的第一帧，完成设备注册 |
| S→C | `welcome` | `server_version`: string（`"0.1.0"`） | 握手回执 |
| S→C | `terminal.open` | `stream_id`: uuid, `cols`: number（示例 80）, `rows`: number（示例 24） | server 要求 connector 为本次会话开一个 PTY |
| C→S | `terminal.opened` | `stream_id`: uuid | PTY 就绪确认 |
| C→S | `terminal.data` | `stream_id`: uuid, `data`: string（base64，standard 字母表、带 padding） | PTY 输出上行 |
| S→C | `terminal.data` | `stream_id`: uuid, `data`: string（同上） | 用户键入下行 |
| S→C | `terminal.resize` | `stream_id`: uuid, `cols`: number（示例 100）, `rows`: number（示例 30） | server 把浏览器的 `terminal.resize` 转发给 connector，用于调整 PTY 行列（**内部帧**） |
| C→S | `terminal.exit` | `stream_id`: uuid, `code`: number（示例 0） | PTY 进程退出，上报退出码 |
| S→C | `terminal.close` | `stream_id`: uuid | server 通知 connector 关闭并清理该流 |
| S→C | `ping` | 无 | 心跳 |
| C→S | `pong` | 无 | 心跳回应 |

> **注 — `terminal.resize` 是内部帧**：它只存在于 server 与 connector 之间，作用是 server 把浏览器发来的 resize 转达给 connector。**浏览器不感知此帧**，Client 通道的公开契约因此零改动（见第 5 节，浏览器只在 `/ws/client` 上发 `terminal.resize`）。

握手示例：

```
connector                                   server
    |----- hello {device} --------------------->|
    |<---- welcome {server_version:"0.1.0"} ----|
    |<---- ping --------------------------------|
    |----- pong ------------------------------->|
```

## 5. WS：Client（浏览器）通道 `/ws/client?token=...`

| 方向 | `type` | 字段 | 说明 |
| --- | --- | --- | --- |
| C→S | `devices.watch` | 无 | 订阅设备变更 |
| S→C | `devices` | `devices`: array，元素结构同 `GET /api/devices` 的 `devices[]` | 订阅后**立即推送一次**，之后每次设备变更再推送 |
| C→S | `terminal.open` | `device_id`: uuid, `stream_id`: uuid, `cols`: number（示例 80）, `rows`: number（示例 24） | 请求在指定设备上开终端 |
| S→C | `terminal.opened` | `stream_id`: uuid | 开流成功 |
| S→C | `terminal.error` | `stream_id`: uuid, `message`: string | 开流失败（设备离线、connector 拒绝等） |
| C→S | `terminal.input` | `stream_id`: uuid, `data`: string（base64，standard padded） | 用户键入 |
| C→S | `terminal.resize` | `stream_id`: uuid, `cols`: number（示例 100）, `rows`: number（示例 30） | 终端尺寸变化 |
| C→S | `terminal.close` | `stream_id`: uuid | 主动关闭；server 回 `terminal.exit` |
| S→C | `terminal.exit` | `stream_id`: uuid, `code`: number（示例 0；`-1` = 设备断连导致的强制结束） | 流结束。`code >= 0` 为远端进程自身退出码；`code = -1` 由 server 在 connector 断线时为在途流补发 |
| S→C | `terminal.data` | `stream_id`: uuid, `data`: string（base64） | 终端输出下行 |

开流时序（三方）：

```
browser (client WS)          server            connector WS           remote PTY
    |-- terminal.open ---------->|                   |                     |
    |   {device_id,stream_id,    |-- terminal.open ->|                     |
    |    cols,rows}              |   {stream_id,cols,rows}                 |
    |                            |<-- terminal.opened|-- spawn PTY ------->|
    |<-- terminal.opened --------|   {stream_id}     |                     |
    |-- terminal.input --------->|-- terminal.data ->|                     |
    |   {base64}                 |                   |-- write stdin ----->|
    |<-- terminal.data ----------|<-- terminal.data -|<-- stdout ----------|
    |-- terminal.resize -------->|   (resize 应用到 PTY)                   |
    |-- terminal.close --------->|-- terminal.close->|                     |
    |<-- terminal.exit ----------|<-- terminal.exit -|<-- 进程退出 --------|
```

## 6. 编码、心跳与退出码约定

- **base64**：`terminal.data` / `terminal.input` 的 `data` 一律是 **standard 字母表、带 `=` padding** 的 base64（不是 base64url，不是无 padding）。因为 PTY 输出是任意字节，不能假定 UTF-8；两端都必须按字节数组处理后再编解码。
- **心跳**：server 发 `ping`，对端回 `pong`；该机制双向对称，即任意一侧收到 `ping` 都必须回 `pong`。Connector 通道按上表定义；Client 通道复用同一约定，用于探测半开连接。
- **stream 生命周期**：`stream_id` 由**发起方**（浏览器）生成并全程复用；server 只在收到 `terminal.exit` / `terminal.close` / 任一端断连时回收映射。未知 `stream_id` 的帧应被丢弃而不是断开连接。
- **退出码语义**：`code >= 0` = 远端进程自身退出（正常路径）；`code = -1` = **设备连接中断导致的强制结束**——connector 断线时，server 会为仍未关闭的每个 stream 主动向浏览器补发 `terminal.exit` 且 `code: -1`。前端必须把两者区分显示：`-1` 是异常断开，不可渲染成正常结束。

## 7. M1 取舍：为什么用内存注册表

M1 的可验证目标是"端到端链路是否走得通"，不是持久化。因此设备注册表与 stream 映射都放在进程内存里（`HashMap` + 一把异步锁）。

选择内存态的理由：

1. **零外部依赖**：不需要跑数据库、不需要迁移脚本，`cargo run` 就能验证骨架。
2. **状态本来就和连接绑定**：`connected` 与 stream 映射都是易失的运行时状态，进程重启后这些连接本来就不存在了，落盘反而要额外设计"启动时如何清理脏 connected 标记"。
3. **契约稳定优先**：REST 响应形状已经定死（第 3 节），换成持久化后端时前端和 connector 一行都不用改。

代价（明确接受）：

- 进程重启后设备记录与 `last_seen_ms` 全部丢失，需要等 connector 重连补齐。
- 无法水平扩展：多副本之间不共享注册表，两个副本会各自看到一半设备。M1 单进程部署，暂不触发。
- 没有历史数据：无法回答"这台设备上次什么时候在线"。

M2 落 SQLite 的方案要点：

- `devices` 表持久化 `id / name / platform / last_seen_ms`；`connected` 不进表，由当前活跃连接集合推导，避免脏状态。
- 单写者 + WAL 模式，写操作集中在一处，读走内存缓存的快照，保证 `devices.watch` 推送语义不变。
- 协议不动：第 3–5 节的字段一个都不改，持久化是纯服务端内部实现。
