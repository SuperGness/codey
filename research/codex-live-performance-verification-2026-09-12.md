# 当前 Codex 性能补丁验证

结论：已确认部分性能策略在当前实例生效；不能确认最近新增的全部补丁已加载，也没有足够数据量化启动或运行提速。

验证对象：macOS，Codex 26.903.71938 / build 8576，主进程 PID `35516`，2026-09-12 11:52:33 启动。Codey PID `35453`。

## 实际运行证据

| 项目 | 结果 | 证据及边界 |
| --- | --- | --- |
| `--require` 主进程注入 | 已确认执行 | 启动器包含 require 与独立 marker 参数；执行 marker 的 PID 为主进程 `35516`。主进程没有 Inspector 参数。marker 仅证明 IIFE 执行，不代表每个后续补丁均已命中。 |
| app-server analytics | 已确认禁用参数 | app-server PID `35760`、`52687` 均有 `analytics.enabled=false`。其中 `35760` 直属当前主进程。 |
| 30 秒诊断心跳 | 已观察到停止 | 主页面持续聚焦、可见 `65001 ms`，`electron-app-state-snapshot-request` 的 heartbeat 和其他请求均为 `0`。结合原生 30 秒定时器源码，这是该功能停止的运行证据。 |
| Trace 日志写盘保护 | 数据库防护已存在 | `.codex/logs_2.sqlite` 和 `.codex/sqlite/logs_2.sqlite` 均存在 `block_log_inserts`，定义为在 `logs` 插入前执行 `RAISE(IGNORE)`。只读查询 schema，未插入测试数据。 |
| Sparkle / updater | 本次启动已关闭 | 当前 PID 对应应用日志第一行记录 `enableSparkle=false enableUpdater=false`。 |
| 页面资源改写 | 已确认运行标记 | renderer 中 `__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__ === true`。它证明资源改写有执行效果，不用于代替性能补丁的逐项证据。 |
| 宠物浮窗 | 当前配置为关闭 | `.codex-global-state.json` 中 `electron-avatar-overlay-open=false`；不能据此证明预热补丁或浮窗 show/hide 节流已执行。 |
| Crashpad 保护 | 未验证到超限清理 | 两个 pending 目录分别为 81 字节、0 字节；远低于 512 MiB 阈值。目录较小不能证明巡检任务运行。 |
| Windows WMI 优化 | 平台不适用 | 当前系统为 macOS，未验证 Windows Worker 拦截。 |

## 离线兼容性验证

从磁盘上的 Codey 构建提取 IIFE，在独立 Node 进程中对当前安装应用的真实主进程文件回放编译钩子；只改写临时副本、校验语法，不执行 Codex 应用源码。

回放设置为 `disablePet=true`、`CODEY_DISABLE_MACOS_CHILD_PROCESS_SAMPLER=true`，没有应用路由覆盖配置。因此它验证指定性能策略的源码匹配，不等于复现本次启动的全部配置。

| 项目 | 离线结果 | 当前实例仍缺少的证据 |
| --- | --- | --- |
| CES analytics 主进程和共享 transport | 两部分均匹配，无该项失败记录 | 主进程实时匹配状态 |
| macOS 子进程采样 | 精确采样调用被移除 | 本次启动脚本版本和实际采样调用次数 |
| 宠物 overlay 预热 | 目标 prewarm 方法插入提前返回 | 本次启动脚本版本和实际调用状态 |
| 焦点触发的插件刷新节流 | 目标监听器包装成功 | 运行中的抑制次数 |
| app-state heartbeat | 原生定时器被移除 | 已有上表的独立运行观察佐证 |

主进程样本包括 `main-Bkkz0ENj.js`、`src-B6LqG3ek.js`、`src-KMpTO78a.js`、`window-all-closed-DnjtB60s.js`；全部通过改写后的语法检查，最终 optional patch failures 为空。

最初把 20 个 build 文件全部走主进程钩子时，CES 与 thread-title 报错。逐文件定位确认：报错仅在将 `worker.js` 人工当作主进程模块回放后出现；主进程样本本身没有这些失败。这一人工回放结果不能视为当前实例的运行故障。

## 版本与验证限制

`lsof` 显示运行中 Codey 的可执行文件 inode 为 `197702585`，检查时同路径磁盘文件 inode 为 `197772940`，磁盘文件修改时间为 14:42:53，晚于 Codey 的 11:52:15 启动时间。两份二进制不同。

离线模板在 14:33:59 从当时磁盘构建提取，SHA-256 为 `1a8e45f1e3fad948fc49c23413dbdaca11661c0ef24a04f573196116a663a3f5`。没有保留原始 require 文件或本次启动模板哈希，不能证明这个模板与正在运行的 IIFE 完全一致。

现有接口没有暴露主进程 `__CODEY_CODEX_STARTUP_PATCH__` 的详细状态。9229 是页面 CDP 端口，不能用页面中的全局变量判断主进程补丁是否缺失。隐藏浮窗节流、焦点刷新抑制次数以及最近新增的编译读取优化，目前均不作运行成功结论。

本轮没有重启、重复注入、修改 app.asar 或 fuse，也没有改变项目功能代码。心跳观察用的临时监听器在结束时移除。未做开关补丁的 A/B 测试，所以不报告 CPU、内存或启动耗时的收益百分比。

## 证据文件

- [require 执行 marker 日志](/Users/kim/.codex-session-delete/codey.log:11647)
- [当前 Codex 启动日志](/Users/kim/Library/Logs/com.openai.codex/2026/09/12/codex-desktop-cfefa8e6-b534-4642-a97e-620f4361099b-35516-t0-i1-035233-0.log:1)
- [65 秒心跳观察](/var/folders/_7/2qc482g55k7gxrbpj762ysfh0000gn/T/codey-live-perf-b08ooty4/heartbeat-observation.json)
- [参数与数据库防护核验](/var/folders/_7/2qc482g55k7gxrbpj762ysfh0000gn/T/codey-live-perf-b08ooty4/live-guards-verification.json)
- [逐文件离线回放结果](/var/folders/_7/2qc482g55k7gxrbpj762ysfh0000gn/T/codey-live-perf-b08ooty4/offline-main-chunks-verification.json)
- [二进制身份与模板哈希](/var/folders/_7/2qc482g55k7gxrbpj762ysfh0000gn/T/codey-live-perf-b08ooty4/provenance.json)

以上 JSON 位于系统临时目录，可能被系统清理；本报告保留了用于判断的主要结果。
