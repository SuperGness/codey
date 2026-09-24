# Codey

Codey 是 Codex 桌面客户端的增强启动器，集中管理模型线路、账号、会话和扩展，并提供用量分析与任务通知。

## 主要功能

- 线路与模型：管理官方和第三方线路，配置代理、同步账号可用模型，拖动调整线路和模型顺序，在任务中切换模型及思考强度。
- 官方账号：管理多个 ChatGPT 账号，可自定义网关地址，分别查看额度和重置时间，并在同一对话中切换使用。
- 请求与用量：查询请求耗时、Token 和错误，分析用量趋势、模型占比及费用估算。
- 会话管理：查看任务状态，导入导出会话，删除指定轮次并恢复备份。
- 扩展管理：管理 MCP、Skill 和可信 Codey 插件，支持配置、启停、导入导出及插件日志查看与清理。
- 插件线路：启用声明了线路能力的插件后添加一条线路，密钥在线路中填写；停用时删除尚未填写密钥的线路，已填写密钥的线路会保留。
- 任务数量：插件通过 `codey.appserver.v1` 调用 `codey://getTasks`，查询当前正在运行和失败的任务数量；无人调用时不会主动统计。
- 页面增强：改善插件市场与常用会话操作，支持精选插件离线恢复。
- 提示词优化：一键优化输入内容，结果可继续编辑。
- 模型分工：为子代理角色、会话命名、提交消息等辅助任务指定模型。
- 文件工具：可选启用 FastCtx，读取、搜索文件并批量替换内容。
- 任务通知：通过飞书、企业微信、Telegram、ntfy 或微信 ClawBot 接收完成、失败和等待介入通知。
- 诊断与更新：检查和修复配置、恢复断线、清理诊断日志，提供宠物精简、渲染诊断和自动更新检查。

## 使用与注意事项

打开 Codey 会自动启动 Codex，点击 Codex 顶部的 Codey 按钮进入控制台；设置是否需要重启，以界面提示为准。

- 仅支持 Codex 桌面客户端；启动可能重启已有 Codex，请先结束正在运行的任务。
- 官方线路需添加官方账号；默认账号决定客户端登录身份，各线路使用各自账号。
- 第三方能力和跨线路会话兼容性取决于服务商与 Codex 版本；自定义上下文不能扩大服务商容量。
- 额度和费用均为估算，不代表实际扣费或官方限额。
- 安装包及原生插件仅使用可信来源；原生插件拥有与 Codey 相同的系统权限，Windows 注入修复可能请求管理员授权并影响签名校验。
- 关闭自动更新检查后，若 Codex 升级导致 Codey 无法启动，需手动下载新版 Codey。

## 第三方声明

    This product includes FastCtx
    (https://github.com/yc-duan/fastctx), Copyright (c) 2026 yc-duan,
    used under the Apache License 2.0.

    FastCtx is redistributed and/or modified here by the maintainer of
    this distribution. Any such change is that maintainer's own work
    and their sole responsibility. It is not endorsed by, not
    supported by, and not attributable to the author of FastCtx, who
    accepts no liability of any kind arising from this distribution or
    from anything built on top of it.

## 联系方式

Codey 由 [SuperGness](https://github.com/SuperGness) 创建和维护。集成、再分发、合作或其他事宜，欢迎联系：kimzane9991@gmail.com。

## 致谢

感谢 [linuxdo](https://linux.do/) 社区的讨论、分享与反馈。
