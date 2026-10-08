# Codey

Codey 是 Codex 桌面客户端的增强启动器，集中管理模型线路、账号、会话和扩展，并提供用量分析与任务通知。

## 主要功能

- 线路与模型：管理官方和第三方线路，配置代理、同步账号可用模型，拖动调整线路和模型顺序，在任务中切换模型及思考强度；本地路由启用时，官方额度耗尽不会阻止切换到第三方线路。
- 远程压缩：可为确认兼容 OpenAI Responses 原生压缩协议的第三方线路手动开启远程压缩，能力变更按界面提示重启。
- 官方账号：管理多个 ChatGPT 账号，可自定义网关地址，分别查看额度和重置时间，并在同一对话中切换使用。
- 请求与用量：查询请求耗时、Token 和错误，分析用量趋势、模型占比及费用估算。
- 会话管理：查看任务状态，导入导出会话，删除指定轮次并恢复备份。
- 手机远程：通过局域网或内置 HTTPS 隧道按项目继续电脑上的 Codex 会话，拍照或选择照片发送，调整模型与执行设置，并管理 Codey 控制台。
- 扩展管理：管理 MCP、Skill 和可信 Codey 插件，支持配置、启停、导入导出、插件包拖拽导入及日志查看与清理。
- 插件线路：支持手动填写或按配置轮换密钥、绑定已保存账号，以及在官方请求前通过另一个账号取得请求状态；停用时仅保留已填写密钥的普通线路。
- 任务数量：插件可按需查询正在运行和失败的任务数量。
- 页面增强：改善插件市场与常用会话操作，支持精选插件离线恢复。
- 桌面操作：通过可选的 Codey Computer Use 插件读取应用状态并执行点击、输入和滚动。
- 提示词优化：一键优化输入内容，结果可继续编辑。
- 模型分工：为子代理角色、会话命名、提交消息等辅助任务指定模型。
- 文件工具：可选启用 FastCtx，读取、搜索文件并批量替换内容。
- 任务通知：通过飞书、企业微信、Telegram、ntfy 或微信 ClawBot 接收完成、失败和等待介入通知。
- 诊断与更新：检查和修复配置、恢复断线、清理诊断日志，提供宠物精简、渲染诊断和自动更新检查。
- 发布管理：系统设置可查看设备号，独立控制台支持指定版本打包、AI 更新日志、版本与设备管理、全量及灰度定向发布和历史全量回滚。

## 使用与注意事项

打开 Codey 会自动启动 Codex，点击左侧图标栏底部的 Codey 按钮进入控制台；设置是否需要重启，以界面提示为准。

- 仅支持可明确识别的 Codex 桌面客户端；启动可能重启所选客户端，请先结束正在运行的任务，多版本共存时需明确选择。
- 官方线路需添加官方账号；默认账号决定客户端登录身份，各线路使用各自账号。
- 启用本地路由后可为官方和第三方模型设置上下文预算，但不能扩大服务商容量；第三方能力与跨线路会话兼容性取决于服务商和 Codex 版本。
- 额度和费用均为估算，不代表实际扣费或官方限额。
- 安装包及原生插件仅使用可信来源；原生插件拥有与 Codey 相同的系统权限，Codey 不修改客户端安全开关，集成失败时会在确认清理后尝试原生启动。
- 桌面工具支持 macOS 14 及以上和 Windows，需在 Codey 插件市场设置中手动准备，再到 Codex 安装启用；macOS 需授予辅助功能和屏幕录制权限，升级后可能需要重新授权。
- 远程控制需手动开启并扫码配对，电脑须保持运行；局域网仅用于可信网络，内置临时隧道经过 Cloudflare，地址随重启变化，设备授权可随时撤销。
- 关闭自动更新检查后，若 Codex 升级导致 Codey 无法启动，需手动下载新版 Codey。

## 第三方声明

PPT Bridge 和 Excel Shield 插件的协议适配迁移自 Kaixxrua/excel-codex-bridge，原项目采用 Unlicense。

远程会话协议适配参考 [codex-mobile-bridge](https://github.com/try2love/codex-mobile-bridge)，采用 MIT 许可；可选隧道使用 [cloudflared](https://github.com/cloudflare/cloudflared)，采用 Apache-2.0 许可。

Astra Turn State 插件的预请求流程参考 [spumon1/SUCK_MY_ASTRA](https://github.com/spumon1/SUCK_MY_ASTRA)，原项目采用 MIT License，Copyright (c) 2026 boooot。

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
