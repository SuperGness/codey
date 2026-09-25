# Cloudflare 发布管理 Worker

该目录提供独立部署的发布管理 API。Worker 负责管理员会话、版本与发布批次状态、设备机器号、发布目标进度及审计记录；D1 保存元数据，更新产物继续使用现有 R2 存储。新版本创建为 `draft`，不会自动推送。

## 部署

1. 创建 D1 数据库，将其 ID 写入 `wrangler.toml`，执行 `pnpm install`。
2. 配置密钥：`wrangler secret put SESSION_SECRET`。该值应为随机长字符串，不能提交到仓库。
3. 执行 `pnpm run db:migrate:remote`，然后插入管理员账号。先运行 `node scripts-hash-password.mjs '<长密码>'` 生成 PBKDF2 值，再将结果写入 `admins.password_hash`；不要把明文密码或哈希提交到代码。
4. 按需设置 `CORS_ORIGIN`，生产环境必须使用管理控制台的精确 HTTPS 来源，不能使用通配符。
5. 执行 `pnpm run deploy`。

## API 概览

- `POST /api/auth/login`、`POST /api/auth/logout`、`GET /api/me`：账号密码登录和 HttpOnly 会话 Cookie。
- `POST /api/versions`、`GET/PATCH /api/versions/:id`、`POST /api/versions/:id/archive`：版本管理。
- `POST /api/versions/:id/publishes`：使用 `Idempotency-Key` 创建全量、灰度或机器号定向批次；服务端会锁定版本的活动批次，防止并发发布。
- `GET /api/publishes`、`POST /api/publishes/:id/pause|resume|withdraw`：发布监控和控制。撤回只撤销尚未下载的目标，已下载或已安装目标保留其结果。
- `POST /api/devices/register`、`GET/PATCH /api/devices`、`PATCH /api/devices/:id`：登记设备、查看机器号并维护白名单；客户端生成的 `installKey` 用于重装后恢复同一设备记录。机器号不是认证凭据。
- `GET /api/updates/check?machineNo=...`、`POST /api/publishes/:id/report`：客户端使用本机保存的设备注册密钥（`x-device-key`）检查更新并回报下载、安装或失败状态；机器号只用于管理员选择目标，不承担认证职责。

发布状态为 `draft`、`pending`、`gray`、`full`、`paused`、`withdrawn`、`archived`；发布批次另有 `scheduled`、`running`、`paused`、`completed`、`withdrawn`、`failed`。灰度使用全部白名单设备或指定比例/数量，设备在下一次检查更新时获取可用批次，Worker 不会强制安装。

Cloudflare 的速率限制、Access/WAF 和日志告警应在部署环境中启用；Worker 本身不会记录密码、会话令牌或完整安装密钥。
