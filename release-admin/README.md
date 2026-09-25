# Codey 发布管理系统

这是与桌面客户端并行部署的 Cloudflare 发布管理系统。Worker API 使用 D1 保存管理员、版本、设备、发布批次和审计记录；更新安装包和清单继续使用现有 R2。管理界面位于 `web/index.html`，可作为 Cloudflare Pages 静态站点部署。

先部署 Worker：在 `worker/wrangler.toml` 填入 D1 数据库 ID，执行 `pnpm install`、`pnpm run db:migrate:remote` 和 `pnpm run deploy`。通过 `node worker/scripts-hash-password.mjs '<管理员密码>'` 生成哈希，再将管理员记录插入 D1；明文密码、哈希、会话密钥和 Cloudflare API key 都只放在本地环境变量或 Wrangler secret 中。

再部署 Pages：执行 `wrangler pages deploy release-admin/web --project-name codey-release-admin`，将 Pages 来源配置为 Worker 的精确 HTTPS 地址。若页面域名和 Worker 域名不同，在 Worker secret/vars 中设置 `CORS_ORIGIN` 为页面完整来源。客户端构建时设置 `CODEY_RELEASE_ADMIN_URL=https://<worker-domain>`；未设置时保留现有 R2 `latest.json` 更新行为。

Worker 不需要 Queues：设备在下一次检查更新时领取已经创建的发布目标，批次进度由设备回报更新。设备使用本地保存的注册密钥完成 API 鉴权，机器号只用于管理员选择设备，不承担认证职责。

生产环境应在 Cloudflare 控制台为 Worker 配置精确 CORS、WAF 和 API 速率限制，并分别创建开发、测试、生产 D1 数据库与 R2 前缀。撤回批次只停止尚未下载的设备，已下载或已安装设备的结果保留在审计和目标记录中。
