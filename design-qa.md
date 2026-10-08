# 远程输入框与模型设置界面验收

final result: passed

## 对照范围

- 来源：[输入框](C:/Users/Administrator/AppData/Local/Temp/codex-clipboard-5ae699eb-204f-4f4d-b02e-bc7df23ee6fe.png)、[思考滑条](C:/Users/Administrator/AppData/Local/Temp/codex-clipboard-2b58c0af-3793-47ba-ae74-b1a51b34da5d.png)、[高级设置](C:/Users/Administrator/AppData/Local/Temp/codex-clipboard-0f0bbab4-8d71-4e64-b872-7e0d93a6b5a5.png)、[附件菜单](C:/Users/Administrator/AppData/Local/Temp/codex-clipboard-ee5ba62e-7882-42d6-8853-27282424a00a.png)。
- 实现全图：[输入框](output/playwright/remote-composer-light.png)、[思考滑条](output/playwright/remote-composer-power-light.png)、[高级设置](output/playwright/remote-composer-advanced-light.png)、[附件菜单](output/playwright/remote-composer-attachments-light.png)。
- 局部对照：[输入框](output/playwright/remote-composer-detail.png)、[思考滑条](output/playwright/remote-composer-power-detail.png)、[高级设置](output/playwright/remote-composer-advanced-detail.png)、[附件菜单](output/playwright/remote-composer-attachments-detail.png)。
- 可交互预览：[远程工作区模拟数据](http://127.0.0.1:1431/codey/tests/remote-workspace-browser.html)。
- 视口：390 × 844 深浅色、320 × 568 窄屏、390 × 450 键盘可视区域模拟、1440 × 900 桌面。
- 对照状态：空输入、Fast 开启、完全访问、长模型名与 Max 档位、思考面板、高级设置、附件菜单。

## 视觉检查

来源与实现图片在同一次图像输入中按状态成对检查。来源是不同尺寸的局部截图，按组件宽度归一化比较；全图检查弹层位置与页面关系，局部检查圆角、图标、字号和滑条。网页截图不含 iOS 系统键盘，背景会话使用模拟数据。

| 检查面 | 结论 |
| --- | --- |
| 字体 | 沿用系统无衬线字体，输入与设置为 16px，模型摘要 18px；高级设置中的长模型名使用 14px，极长名称截断，原生选项保留完整名称。 |
| 布局 | 紧凑圆角输入框、左右图标分组、胶囊滑条、底部圆角面板与双行附件菜单符合参考比例；高级面板约为宽度的 1.18 倍，受可视高度限制。 |
| 色彩 | 浅色使用近白面板、灰色设置组、黑色滑条与白色滑块；完全访问为橙色。深色沿用现有主题，弹层与控件有独立对比度。 |
| 图标与图像 | 使用现有 Tabler 矢量图标；Fast 仅在 priority 档位显示实心闪电。仪表盘、语音波形与相机使用库中最接近的图标，未引入位图或额外依赖。 |
| 文案 | 输入提示为“向 Codex 提问”；面板使用“高级 / 模型 / 智能 / 速度”，快速模式显示“快速”；附件显示“相机 / 照片”。 |

弹层在窄屏、键盘高度和多行草稿下保持在可视区域内，内容过高时内部滚动。模型、档位和速度使用原生选择器，继续复用既有远程设置协议。

## 修正与剩余差异

- 对照后缩短输入框高度、调整高级面板比例与附件菜单间距，修正长模型名和图标呈现。
- 滑条拖动时本地预览、松手后单次提交，键盘同步期间保留焦点；模型档位按强度排序，失败时恢复已确认档位。
- 无待处理的 P0/P1/P2 视觉或交互问题。
- 按需求，相机与照片只显示占位反馈；参考图中的语音入口同样是占位，不请求摄像头、相册或麦克风权限。
- P3：系统字体、原生选择菜单和库图标笔形会与 iOS 参考略有差异，思考面板增加轻微底色以保证会话背景上的可读性。

## 验证清单

- [x] 拖动与键盘调整、单次提交、模型切换后的兼容档位、Fast 显隐。
- [x] 附件占位无网络写入，弹层关闭、焦点恢复与高级面板焦点约束。
- [x] 设置失败恢复、断线保护、单档位、空档位与无序目录。
- [x] 原有发送、补充、停止、草稿隔离、新建会话、断线重连与未确认指令保护。
- [x] 相关 42 项测试、类型检查与前端构建；完整会话和输入框专项浏览器验证。

验证边界：浏览器交互使用模拟接口；未在实体 iPhone 上验证系统键盘与原生选择菜单，也未修改真实桌面会话设置。
