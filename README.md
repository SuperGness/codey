# Codey

Codey 是 Codex 桌面客户端的增强启动器。它会启动 Codex，并在 Codex 页面内提供统一控制台，用于管理模型线路、任务辅助能力、通知、诊断和更新。

## 主要功能

- 启动与状态：自动启动或重启 Codex，展示版本、应用位置、运行状态和功能生效情况。
- 线路与模型：统一管理官方账号和第三方服务，支持多线路同时使用、模型同步与手动维护、全局默认模型，以及在同一任务中切换模型；还可按线路声明网页搜索、自动审核等兼容能力。关闭本地路由后，Codex 会直接使用当前线路，Codey 仅保留只读模型管理。
- 请求日志：可选择记录线路请求，并在独立页面查看概览统计、搜索、筛选和分页查看线路、模型、状态、耗时、Token 用量与错误摘要，支持便捷复制 ID 提示反馈；历史日志可单独清理。
- 官方账号额度：可在 Codex 账户区域显示套餐、剩余额度和重置时间。
- 会话增强：优化任务时间和运行状态展示，支持会话导入、导出、整段或指定轮次删除，以及最近备份恢复；页面状态异常时会尝试重新同步。
- 插件与页面增强：修复插件市场和本地插件展示，提供可离线恢复的精选插件内容，并改善常用会话操作和页面体验。
- 提示词优化：可使用已配置线路或独立服务一键优化输入框中的提示词，结果仍可继续编辑。
- 子代理增强：提供快速定位、深度检索、视觉分析、代码实施和视觉实施五类角色，可分别选择模型与思考深度，并限制不安全的并发写入；纯只读协作期间，主任务仍可安全查询文件、网页和数据库信息。
- 上下文工具：可选启用内置 FastCtx，提升长任务中的文件读取、搜索、发现和批量替换体验；检测到已有 FastCtx 配置时不会重复加载。
- 消息通知：支持飞书、企业微信、Telegram 和微信 ClawBot，可为任务完成、失败或等待介入分别配置多个通知渠道。
- 稳定性与存储保护：提供健康检查、会话恢复、诊断存储统计与清理，并可按平台启用 Trace、Crashpad、Windows 性能保护、宠物精简和渲染诊断选项。

## 使用方式

打开 Codey 后，它会自动启动 Codex。进入 Codex，点击顶部的 “Codey” 按钮即可打开控制台。保存设置后，可立即生效的功能会直接更新；需要重启的项目会在界面中提示。

## 注意事项

- Codey 只面向 Codex 桌面客户端，不覆盖命令行版本。
- 启动 Codey 时，如 Codex 已在运行，Codey 可能先关闭并重新启动它；正在运行的任务会被中断。
- 官方线路依赖 Codex 的官方登录状态；第三方线路的可用功能取决于对应服务和账号。
- 远程第三方服务必须使用安全地址；本机服务可使用本地地址。
- 删除会话或轮次属于高风险操作。重要内容请先导出，不要把自动备份当作唯一副本。
- 部分增强能力会随 Codex 版本和所选线路而变化，请以控制台提示为准。
- macOS 未签名安装包可能被系统拦截；可使用`xattr -dr com.apple.quarantine /Applications/Codey.app`跳过校验。

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
