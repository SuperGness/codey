# 安装与使用（Windows x64）

先按照 [BUILDING.md](BUILDING.md) 生成便携 ZIP，或取得可信发布者构建的 ZIP。解压后执行：

```powershell
.\scripts\install.ps1 -Destination "$env:LOCALAPPDATA\CodeyAntigravity"
```

安装脚本校验所有文件及代理版本，只复制到空目录。它不会修改 Codey 的插件或账号配置。发布包不附带 OAuth 应用凭据；先在当前终端通过 `ANTIGRAVITY_CLIENT_ID` 与 `ANTIGRAVITY_CLIENT_SECRET` 配置你已获授权的 Google OAuth 客户端，再进入安装目录登录并启动代理。客户端需支持 Google Cloud Code/Antigravity scope 与本地回调；不能保证任意新建 OAuth 应用均被 Google 接受。已有 Pi 登录的 refresh token 必须使用同一个签发客户端配置。

不要把真实值写入插件配置、脚本或 Git；它们只由当前进程环境传给代理，未配置时登录/刷新会明确失败。以下值是占位符，运行前在本机替换：

```powershell
Set-Location "$env:LOCALAPPDATA\CodeyAntigravity"
$env:ANTIGRAVITY_CLIENT_ID = '<authorized-oauth-client-id>'
$env:ANTIGRAVITY_CLIENT_SECRET = '<authorized-oauth-client-secret>'
.\bin\antigravity-proxy.exe login --manual
.\start-proxy.ps1
```

`login --manual` 提供登录 URL；在同一个仍运行的终端粘贴登录完成后的回调 URL，不要把回调、令牌或账号文件发给别人。也可运行 `login` 使用本地回调。Google OAuth 属于代理，不使用 Codey 保存的 OpenAI 账号。

在 Codey 的插件管理中导入 `antigravity-router-0.9.0-windows-x64.codey-plugin`，核对来源后显式启用，并在线路列表选择 Antigravity 及所需模型。导入默认停用，原生插件不受沙箱隔离。无需给此本地线路添加真实 Google API Key。

代理默认仅监听 `127.0.0.1:8787`。自定义端口时用 `start-proxy.ps1 -Port <端口>`，并同步修改插件配置的 `baseUrl` 后重新启用。插件 `routeId` 默认为空，只注册线路；生命周期处理需填写准确线路 ID。`retryOnce` 默认关闭，重放可能重复工具执行或计费，只有明确需要时才开启。

启动脚本继承当前进程的 `HTTP_PROXY` / `HTTPS_PROXY`，并为回环请求补充 `NO_PROXY`；需要读取 Windows 当前用户的手动代理时使用 `-UseSystemProxy`。PAC 和代理认证不自动处理，请显式设置环境变量。启动失败时可直接运行 `bin/antigravity-proxy.exe serve` 查看错误。

```powershell
.\bin\antigravity-proxy.exe accounts list
.\bin\antigravity-proxy.exe usage
.\stop-proxy.ps1
```

启动和停止脚本检查可执行路径、PID 创建时间、端口归属及健康服务标识；遇到占用端口会拒绝操作。它们不会创建开机任务。默认账号及模型缓存位于 `~/.pi/agent`，启动脚本生成的图片与进程记录位于安装目录 `.runtime`；自定义 `-AuthPath` 时代理缓存放在该账号文件所在目录。账号文件含可刷新令牌，限制本地访问并保留备份。停用或卸载原生插件不会停止代理，也不会删除代理账号、图片或 Codey 保留的数据与日志。

代理支持 HTTP/SSE 和 Codey WebSocket Responses，以及显式搜索、图片、模型、usage 与 doctor 接口；拒绝带 Origin 的浏览器调用。远程图片只允许公有地址，图片镜像默认关闭；只有显式设置 `AG_IMAGE_MIRROR=1` 和 `HFSY_API_KEY` 才会上传。`AG_DEBUG` 只记录脱敏计数。可用 `ANTIGRAVITY_NO_EXTRA_TOOLS=1`，或分别用 `ANTIGRAVITY_NO_SEARCH_TOOL=1` / `ANTIGRAVITY_NO_IMAGE_TOOL=1` 禁用模型侧额外工具；显式搜索和图片命令仍可使用。
