# deploy/ — farwork hub on the 1Panel host

一套 **image-only** 的 compose 部署：CI 构建并推送镜像，服务器 pull 后直接跑，
不在服务器上编译、也不把宿主机文件挂进容器。

- 镜像：\`ghcr.io/vpertj/farwork\`，由 \`.github/workflows/docker.yml\` 在 \`v*\` tag
  或手动触发时构建（amd64 + arm64），产出三个标签：\`latest\` / \`<git tag>\` / \`sha-<short>\`。
- 本目录只放部署描述：\`docker-compose.yml\`、\`.env.example\`（+ 你本地的 \`.env\`，不入库）。

## 1. 写 .env

\`\`\`sh
# 在服务器上
mkdir -p /opt/farwork-deploy && cd /opt/farwork-deploy
# 把 deploy/docker-compose.yml 与 deploy/.env.example 放进来
cp .env.example .env && chmod 600 .env
\`\`\`

然后填 \`.env\`（变量与含义见 \`.env.example\` 的逐条注释）。两条容易错的：

- \`DATABASE_URL\` 的主机名是 **PostgreSQL 容器名**（\`docker ps\` 里那个），不是
  \`127.0.0.1\` —— 在 hub 容器里 \`127.0.0.1\` 是 hub 自己。
- \`FARWORK_HUB_PUBLIC_URL\` 必须是对外域名（\`https://farwork.xacalc.cn\`）。它会被
  原样写进配对命令，写错就是「设备永久离线」那类事故。

## 2. 起服务

\`\`\`sh
cd /opt/farwork-deploy
docker compose pull          # 只拉 ghcr.io/vpertj/farwork:<FARWORK_TAG>
docker compose up -d         # 先跑 db-init，成功后才起 hub
\`\`\`

\`docker compose up -d\` 一定会先跑 \`db-init\`（\`depends_on … service_completed_successfully\`），
它做两件幂等的事：\`pg_roles\` 里没有就建 role（已存在则同步密码），\`pg_database\` 里没有就建
database。**可以反复执行**，也可以\`docker compose run --rm db-init\`单独重跑。
1Panel 的容器列表里它会显示为退出 0（\`restart: "no"\`）。

## 3. 验证三查

\`\`\`sh
cd /opt/farwork-deploy

# ① 容器在跑（STATUS 里的 (healthy) 来自镜像自带的 HEALTHCHECK）
docker compose ps

# ② 镜像命中：Config.Image 应为 ghcr.io/vpertj/farwork:<FARWORK_TAG>
docker inspect -f '{{.Config.Image}}' farwork-hub-1

# ③ 网络已接上：networks[].ipv4 不能为空。
#    为空 = 容器没接进 1panel-network，反代必然 502。
docker inspect -f '{{range $k,$v := .NetworkSettings.Networks}}{{$k}} {{$v.IPAddress}}{{end}}' farwork-hub-1
\`\`\`

再打一下健康端点与自报地址（后者就是写进配对命令的那个值）：

\`\`\`sh
curl -fsS http://127.0.0.1:13080/healthz
curl -fsS http://127.0.0.1:13080/meta
\`\`\`

hub 只监听 \`127.0.0.1:13080\`，公网入口是 1Panel 的反向代理站点
（\`farwork.xacalc.cn\` → \`127.0.0.1:13080\`，需开启 WebSocket 转发：\`/api/v2/**\` 里有
\`/dashboard/ws\`、\`/sessions/{id}/ws\`、\`/connector/ws\`）。

## 4. 升级 / 回滚

升级与回滚是同一个动作，只改 \`.env\` 里的 \`FARWORK_TAG\`：

\`\`\`sh
cd /opt/farwork-deploy
sed -i 's/^FARWORK_TAG=.*/FARWORK_TAG=v0.2.0/' .env   # 回滚就填上一个 tag / sha-xxxxxxx
docker compose pull
docker compose up -d
\`\`\`

数据库迁移是 hub 启动时自动应用的（\`sqlx::migrate!\`，迁移只加不改），所以
**回滚二进制不会回滚数据库结构**：旧版本必须能读懂新表结构，跨大版本回滚前先备份
（\`pg_dump\`）。\`.env\` 里的 \`FARWORK_HUB_SECRET\` 换了等于全员掉线，升级时不要动它。

## 5. 已知取舍

- hub 容器内以 root 运行：基础镜像 \`debian:bookworm-slim\` 里建了 uid 10001 的
  \`farwork\` 用户，但 compose 没有强制 \`user:\`——保持与镜像默认一致，避免宿主 uid 映射
  差异带来的权限问题。要收敛时在 \`hub\` 服务上加 \`user: "10001:10001"\` 即可（镜像里
  没有需要写权限的路径）。
- \`db-init\` 每次 \`up -d\` 都会再跑一遍。这是有意的：它同时承担「轮换密码」的职责，
  代价只有一次容器启动（< 2 秒）。
- 镜像拉取失败时 \`docker compose up -d\` 会把错误直接打出来；\`pull\` 与 \`up\` 分开执行，
  就是为了让「拉不到」和「起不来」两种故障在日志里可区分。
