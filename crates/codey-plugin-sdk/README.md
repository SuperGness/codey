# Codey Plugin SDK

此 SDK 用于独立开发 Codey 原生 Rust 插件，不依赖 CPA 或 Codex 插件市场。插件编译为 `cdylib`，通过 `export_plugin!` 导出唯一入口 `codey_plugin_entry_v1`。Rust trait 仅在插件内部使用，动态库边界采用 ABI v1 的 C 布局函数表和字节缓冲区，返回内存由插件自己的释放函数回收。

实现 `Plugin::create` 与 `Plugin::invoke` 后即可使用导出宏。宿主串行调用同一个实例；构造函数和回调应及时返回。自行创建的后台任务必须在实例销毁前停止。SDK 与宿主都会把 ABI 入口的 panic 转成错误，避免破坏插件管理；无法隔离段错误、进程退出或死循环，因此只应安装可信插件。

## 安装包

`.codey-plugin` 是包含 `manifest.json`、动态库及根目录 `config.json` 配置模板的 ZIP。每个包对应一个操作系统和架构。manifest 包含插件 ID、名称、semver 版本、`abiVersion`、`platform`、`arch`、`entry`、`librarySha256`、`capabilities` 和 `headerNames`。平台使用 `macos`、`windows` 或 `linux`，架构使用 Rust 名称，例如 `aarch64` 或 `x86_64`。

导入只检查文件，不执行动态库；用户启用后才加载。SHA-256 仅校验完整性，不证明发布者身份。安装版本和配置独立保存；已启用实例保持原配置和权限，更新后需停用再启用或重启 Codey。动态库映射保留到进程退出，以避免卸载后仍有回调访问代码；Windows 可能需要重启后才能删除曾加载的 DLL。

## 插件目录与持久数据

每个插件使用 Codey 用户状态目录下的 `codey-plugins/installed/<plugin-id>/`。程序制品位于 `versions/<version-uuid>/`，持久数据位于 `data/`，日志位于 `logs/`，唯一运行配置为插件目录中的 `config.json`。首次安装复制包内模板，升级保留已有配置。配置及启用状态由宿主统一管理。

实现 `Plugin::create(config, context)` 接收配置及必填的 `PluginContext`，后者包含绝对路径 `plugin_dir`、`data_dir`、`log_dir` 和 `plugin_id`。ABI 初始化输入固定为 `{"config":...,"context":{"pluginId":...,"pluginDir":...,"dataDir":...,"logDir":...}}`，缺失配置或上下文时拒绝初始化。宿主不会修改全进程工作目录或环境变量。

升级、停用、重新启用及重启均保留 data 和 logs。卸载默认保留配置、数据与日志，重装相同 ID 后继续使用；选择清理数据才删除插件目录。仍在执行或销毁的实例会阻止卸载，避免清理后回调重新写入。库映射可能使 Windows 文件清理需要退出 Codey 后再进行，清理失败会明确返回残留路径。

使用 `context.data_dir.join("state.json")` 保存业务数据；应使用固定文件名、限制文件大小，并自行处理并发和原子更新。`context.log("fixed_event")` 写入 `logs/plugin.log`，宿主生命周期事件写入 `logs/host.log`。每种日志最多保留当前文件与一个备份，每个文件上限 1 MiB，单条事件上限 4096 字节；日志 helper 不会重建缺失目录。避免在每次路由请求成功时同步写日志，不写入配置、凭据或请求内容。日志 helper 使用同目录的锁文件协调跨版本实例及进程的写入和轮转；无法取得锁时返回错误，由调用者决定忽略或稍后重试。该约定要求所有写入方使用此接口。

这些目录是可信原生插件遵守的存储约定，不构成文件系统沙箱；宿主无法阻止插件自行访问其他目录或绕过日志轮转。

## 配置与扩展

配置文件使用严格的 UTF-8 JSON 对象，最多 1 MiB，不支持 `//` 或 JSONC 注释。配置编辑器以 JSON 文本外观展示已有对象、数组和字段，字符串带双引号，数字和布尔值不加引号；说明以灰色小字显示在配置项上方且不可选中或编辑，字段名固定；标量及不含对象的值数组可直接编辑，值数组支持增删元素，含对象的数组按字段逐项编辑。保存仅替换修改过的值，保留其余原文，并检查文件是否被其他编辑器修改。新增字段、调整对象结构或修正无效文件需在文件中完成后重新加载。插件不再提供配置 schema 或 HTML 配置页；字段默认值写入模板，业务约束由插件初始化时校验。配置保存在当前用户的私有目录中，目前没有独立密钥存储服务。

任意对象层级（包括数组内的对象）可添加 `_comments` 对象，其键名不限，每项值必须是说明字符串，例如 `"_comments": {"value": "请求头的值"}`。宿主统一校验，并在传给插件前移除这些注释字段；文件原文仍保留说明，插件无需自行处理。只有精确的 `_comments` 字段受此约定约束，其他下划线字段及字符串内容保持原样。只修改说明无需重新启用；修改业务参数后，运行中的插件需重新启用才生效。

显示说明时优先使用同级字段名，再查找祖先对象的点分隔字段名；数组路径省略下标，例如 `stateConfigs.model` 可说明每项的 `model`。真实的同名含点字段优先，存在歧义时应把说明放在嵌套对象自身的 `_comments` 中。没有对应字段的说明保留在文件中，不生成可编辑项。

请求扩展统一声明 `request.lifecycle.v1`，使用 SDK 的 `lifecycle` 协议类型。宿主在发送前、收到响应头、等待恢复及请求结束时调用插件；插件可修改授权的请求头，或返回等待、受限重发及终止动作。多个插件按 ID 排序执行，返回值整体验证后才应用。处理异常时默认终止请求，可显式声明异常时继续。

其他管理方法由插件自行定义，通过 `invoke_codey_plugin` 调用。`request.beforeSend`、`request.afterHeaders`、`request.resume`、`request.completed`、`request.failed` 和 `request.cancelled` 只由宿主调度，管理接口会拒绝同名调用。完整的权限、事件、动作及传输边界见 [请求生命周期协议](REQUEST_LIFECYCLE.md)。

声明 `appserver.call.v1` 后，插件发送 `{"schema":"codey.appserver.v1","call":"codey://getTasks"}` 查询正在运行和失败的任务数量。未列入 `schema/appserver.v1.json` 的调用不会执行。

声明 `provider.route.v1` 后，宿主在启用时调用 `provider.describe`。返回对象包含 `name`、`baseUrl`、`upstreamProtocol`（`openaiResponses`、`openaiChatCompletions` 或 `anthropicMessages`）、至少 1 个且最多 32 个 `models`，以及可选 `headers`。线路名最多 15 个字符。请求头不能携带密钥，名称限制与生命周期相同。密钥由用户填写在线路上。该调用不能通过管理接口进入。插件不提供自己的传输实现。

## 示例

`examples/plugins/header-demo` 演示配置、`ping` 方法和请求头扩展。在仓库根目录执行：

```sh
cargo build -p codey-plugin-header-demo
python3 scripts/package-plugin.py --library target/debug/libcodey_plugin_header_demo.dylib --config examples/plugins/header-demo/config.json --output /tmp/header-demo.codey-plugin --id dev.codey.header-demo --name 请求头示例 --version 0.1.0 --capability request.lifecycle.v1 --header x-plugin-demo
```

示例命令的动态库路径适用于 macOS；其他平台使用对应扩展名。若设置了 Cargo target-dir，应替换制品路径。打包工具不覆盖已有输出，省略 `--config` 时写入空对象模板。示例只添加测试请求头，首次验证建议使用本地模拟上游。
