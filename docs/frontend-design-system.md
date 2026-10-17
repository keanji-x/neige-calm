# 前端设计系统

## 定义入口

- `fe/web/src/styles/theme.config.json`：字体、文字角色、原有深浅配色、侧栏密度与终端宿主值的唯一配置。
- `fe/web/src/styles/tokens.css` 与 `theme-values.ts`：生成后的 CSS token 与只读宿主常量。
- `fe/web/src/styles/public.ts`：公开 token 类型。
- `fe/web/src/styles/astryx-theme.css`：Astryx 主题映射。

## Token 用途

| 用途 | Token |
| --- | --- |
| 页面、导航 | `--bg`、`--surface-rail` |
| 面板、浮层 | `--surface-card`、`--paper` |
| 正文、次要信息、元信息 | `--text`、`--text-2`、`--text-3` |
| 内部分隔、控件边缘 | `--hairline`、`--hairline-strong` |
| 悬停、按下、选中、焦点 | `--overlay-hover`、`--overlay-active`、`--accent-soft`、`--accent` |
| 等待处理、失败、成功 | `--warn`、`--error`、`--success` |
| 间距、圆角、行高、控件尺寸 | `--space-*`、`--radius-*`、`--row-h*`、`--control-h*` |

## 使用

- 按语义用途选择 token；组件复用 Astryx 和本仓库 UI 原语。
- 等值替换保持现有视觉：2px、4px、8px 间距分别使用 `--space-half`、
  `--space-1`、`--space-2`。
- 界面沿用既有色系与圆角，通过底色层级区分区域；内部结构使用必要的分隔线。
- Token 变更由 styles owner 审核，同步公开类型、inventory、两套主题、
  消费方映射和对比度检查。

## 交互动效

新增或修改的位移、尺寸及浮层进出统一使用 `ui/motion/spring.ts` 的 Motion
物理弹簧。组件声明目标状态与几何，复用 `SizeMotion`、`useSpringPresence`
或共享播放适配器；响应与阻尼由 UI owner 统一维护，不单独调时长、曲线或
距离倍率。中途反向继承当前位置与速度，正文不做比例缩放。

浏览器原生播放负责呈现；尺寸动画结束和卸载后释放内联尺寸。普通输入、
业务状态和焦点即时更新，不等待动画。减少动态效果时跳过位移与尺寸动画。

简单颜色反馈沿用语义 CSS token，直接拖拽保持跟手，装饰循环保持 owner
声明的节奏；其他例外在 owning layer 明确说明，不另建通用运动算法。

## 唯一字体与间距尺度

采用 [Radix Themes 字阶](https://www.radix-ui.com/themes/docs/theme/typography) 与
[间距尺度](https://www.radix-ui.com/themes/docs/theme/spacing)，不迁移组件或新增运行时依赖。
`theme.config.json` 的 `typography.scale` 每个 step 只定义一次字号、行高与 em 字距；
profile仅引用step、family、weight；等值配方共用profile，语义role仅绑定profile ID。字体家族继续区分衬线阅读、sans 界面和等宽代码。

| Step | 字号 / 行高 | 字距 |
| --- | --- | --- |
| 1 | 12 / 16 | .0025em |
| 2 | 14 / 20 | 0 |
| 3 | 16 / 24 | 0 |
| 4 | 18 / 26 | -.0025em |
| 5 | 20 / 28 | -.005em |
| 6 | 24 / 30 | -.00625em |
| 7 | 28 / 36 | -.0075em |
| 8 | 35 / 40 | -.01em |
| 9 | 60 / 60 | -.025em |

普通界面、表格和列表内容使用 step2；阅读正文与输入使用 step3；工作标题与章节使用 step5。
语义不同的table、reading、input等role保持独立，即使绑定同一profile。
完整文字角色使用 `font: var(--type-role)` 与对应 `--type-role-tracking`。
局部字号、行高和字距修饰消费同源step投影，保留inherit、强调与数值格式语义，不重设其余字体属性。
Sidebar通过required NavigationRow spec选择caption或entry：caption使用metadata，entry使用content；深度只改变文字与选中背景缩进，不移动状态列。其他列表保留自身group语义，host不再分别覆盖font和tracking。

有限几何例外：编辑器代码保留12/20、等宽紧凑标签与固定双行数字槽保留12/12。
这些例外也只在 `typography.exceptions` 中成对定义。终端12.5px、ANSI色板、宿主RGB、
PTY与CodeMirror几何保持原契约。`tnum`、italic、strong 等继承格式与图标/单行控制居中
不重新指定字号；生成的CJK字体资产保持不变。原生数据报表仍保留其界面字体，移动报告
保留既有sans家族适配，局部字号和行高引用同一组标准step。

`spacing` 是唯一留白表：step1..9对应4/8/12/16/24/32/40/48/64px，另有0、px(1)、half(2)。
间距按原生Radix100%使用物理px，字体仍使用rem，文本放大不会改控件/阴影几何。
组件消费 `--space-*`；navigation的gap/inset也引用表中step，不重复存像素。
旧编号无兼容层。实际radius、glyph、命中区、布局宽度、阴影、模糊和协议几何不当作留白取整，
继续由原owner保留；drawer/seam/stack共用一次既有20px/28px安全边界声明。

在 `fe/` 运行 `npm run theme:generate` 更新 tokens、宿主常量和通用UI typography-scope生成样式。
`theme:check` 严格检查全部产物。build-time Zod Schema验证结构和所有profile/step/family/weight引用；公开字体、间距、导航类型与机器inventory同源生成，不手工重复角色名单。独立测试固定Radix尺度、字体栈、渲染与颜色/宿主边界。显式字体scope由UI适配器导出class，重新计算family相关role及投影，
避免root composite提前绑定字体；mobile-font在同一元素组合该class与自托管字体栈，无额外DOM。
Astryx发布的size/family/unitless-leading由实际使用的role投影，不能用长度代替leading倍率。

本轮不修改任何浅深配色、阴影、圆角配置、终端宿主值或交互机制。

Astryx组件内部spacing保留其已发布的独立协议，不按同名编号做全局改写；本轮仅迁移我们拥有的留白，避免改变外部组件固定几何。
