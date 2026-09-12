# Codey

Codey 是 Codex 桌面客户端的增强启动器。它会启动 Codex，并在 Codex 页面内提供统一控制台，用于管理模型线路、任务辅助能力、通知、诊断和更新。

## 主要功能

- 线路与模型：统一管理官方账号和第三方服务，支持多线路、上游请求头覆盖、模型同步、默认模型、任务内切换、Fast 选项及第三方模型上下文设置。
- 请求日志：查看请求耗时、Token 用量、缓存和错误信息，支持筛选、统计与历史清理。
- 官方账号额度：显示套餐、剩余额度和重置时间，并结合请求记录估算周限与费用。
- 会话管理：显示任务时间与运行状态，支持会话导入、导出、整段或指定轮次删除及备份恢复。
- 插件与页面增强：改善插件市场、本地插件展示和常用会话操作，支持精选插件离线恢复。
- 提示词优化：一键优化输入框中的提示词，结果可继续编辑。
- 子代理协作：提供快速定位、深度检索、视觉分析、代码实施和视觉实施五类角色，可分别设置模型与思考深度。
- 文件工具：可选启用内置 FastCtx，支持文件读取、搜索、查找和批量替换。
- 消息通知：通过飞书、企业微信、Telegram 或微信 ClawBot 接收任务完成、失败和等待介入通知。
- 诊断与保护：提供健康检查、断线恢复、诊断日志和崩溃报告清理，以及宠物精简与渲染诊断选项。
- 更新管理：检查并安装 Codey 更新。

## 使用方式

打开 Codey 后会自动启动 Codex，点击 Codex 顶部的 Codey 按钮进入控制台。设置是否需要重启，以界面提示为准。

## 注意事项

- 仅支持 Codex 桌面客户端；启动时可能重启已有 Codex，请先结束正在运行的任务。
- 官方线路需要登录 Codex；第三方服务能力及部分增强效果取决于账号、服务商和 Codex 版本。
- 额度与费用估算仅供参考，不代表实际扣费或官方周限；自定义上下文设置不能扩大服务商的容量限制。
- 跨线路切换可能受会话历史兼容性限制，请按界面提示处理。
- macOS 可能拦截未签名安装包，请确认来源可信后再运行。

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
