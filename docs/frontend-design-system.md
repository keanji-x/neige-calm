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
- 等值替换保持现有视觉：2px、4px、6px 间距分别使用 `--space-1`、
  `--space-2`、`--space-3`。
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

## 集中主题配置

修改 `theme.config.json` 后，在 `fe/` 执行 `npm run theme:generate`。
`npm run theme:check` 检查 CSS 和宿主常量是否陈旧，lint 和 build 都会执行它。
组件直接使用语义 font token，颜色按状态独立选择；不运行 DOM 角色猜测，不加全局 `!important`。

| 用途 | 字号／行高／字重 | font token |
| --- | --- | --- |
| 普通界面、Area、卡片与任务名称 | 14／20／400 | `--type-ui`、`--type-content` |
| Track、按钮 | 14／20／500 | `--type-action` |
| Today、顶部报告标题 | 14／20／600 | `--type-page-title` |
| Unread、Pinned、Areas 分类 | 14／20／400 | `--type-navigation-label` |
| 报告正文，衬线宋体回退 | 16／26.4／400 | `--type-reading` |
| 任务目标，无衬线 | 16／26.4／400 | `--type-detail` |
| 报告章节，衬线 | 20／24／700 | `--type-chapter` |
| 表格内容 | 14／20／400 | `--type-table` |
| 表头与卡片分类 | 12／16／600 | `--type-label` |
| 数量、类型、状态说明 | 12／16／400 | `--type-metadata` |
| 代码与命令，等宽 | 12／20／400 | `--type-code` |

普通文字字距为 0，辅助分类标题使用 0.66px；布局与状态色仍由所属组件决定。
图表主指标保留独立 36px 数字角色，这是数值展示的例外，不扩展普通文字字阶。
终端 ANSI 色板与发送给 daemon 的 RGB 值也从本配置生成，但保持原协议值；
RGB 默认值仍由与 Rust 默认值对照的契约测试约束，本次不改变协议。

终端固定网格文字也在 `terminalText.fontSize` 中配置，保留既有 12.5px，避免本次改变 PTY 几何。
次级章节有独立衬线／600 角色：16px 与14px，不复用任务说明或表格字体。
