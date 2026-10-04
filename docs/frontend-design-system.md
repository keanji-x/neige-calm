# 前端设计系统使用规范

本次从 Orca 借鉴的是 token 的职责划分、组件复用和可执行的样式约束。
保留 Neige 现有的色系、圆角、布局和按底色分区的方式。

评审等级：L1。仅将已有间距值改为等值 token，并完善规范；不修改产品行为、
数据持久化、执行权限、token 名称或主题值。

## 唯一来源

- `fe/web/src/styles/tokens.css` 定义应用语义变量和浅色／深色主题。
- `fe/web/src/styles/public.ts` 定义公开 token 类型。
- `fe/web/src/styles/astryx-theme.css` 将应用变量映射到 Astryx。
- 组件样式消费这些变量，不另建色板、主题或并行组件系统。

## 职责

| 角色 | 已有 token | 用途 |
| --- | --- | --- |
| 页面与导航 | `--bg`、`--surface-rail` | 页面底色、导航底色 |
| 面板与浮层 | `--surface-card`、`--paper` | 内容面板、菜单和弹窗 |
| 文本层级 | `--text`、`--text-2`、`--text-3` | 正文、次要信息、元信息 |
| 必要分隔 | `--hairline`、`--hairline-strong` | 模块内部的分隔和控件边缘 |
| 交互 | `--overlay-hover`、`--overlay-active`、`--accent-soft`、`--accent` | 悬停、按下、选中与焦点 |
| 状态 | `--warn`、`--error`、`--success` | 需要处理、失败、成功 |
| 几何 | `--space-*`、`--radius-*`、`--row-h*`、`--control-h*` | 间距、圆角、行高与控件尺寸 |

## 使用规则

1. 保留现有视觉值。token 化是把重复值归入已有词汇，不顺带改色、压缩布局
   或缩小圆角。例如 2px 间距使用 `--space-1`，6px 使用 `--space-3`。
2. 底色已经区分区域时，不添加外框或额外边缘线。模块内部有必要的分隔可保留。
3. 使用语义角色选变量，不因为两个变量当前值相同就互换。选中与按下分别
   使用各自角色；正文、次要信息和禁用文字也分别选用对应角色。
4. 优先复用 Astryx 和本仓库 UI 原语；组件保留自身 CSS Module 和既有 layer，
   不从组件定义全局主题，也不靠跨层选择器改另一组件的样式。
5. 颜色有明确语义，状态色不作装饰；正文宽度、焦点可见性和触屏操作保持可用。
6. 新增或改名 token 必须说明现有词汇不足的原因，经过 styles owner，并同步
   公开类型、手写 inventory、两套主题、Astryx 映射（若消费）和对比度检查。
7. 保留有具体意义的尺寸：图表几何、协议颜色、动态数据色和经 owner 批准的
   第三方桥接值不应为了消除所有数字而机械地变成通用 token。

## 已有约束与本次范围

继续使用 stylelint、CSS layer／ownership 检查、token inventory 契约和真实浏览器
对比度矩阵。不新增并行 linter 或宽泛 allowlist。

本次恢复预览阶段的所有视觉改动，仅将原生报告分段控件、数据可视化标签控件、
上下文提示和聊天附件的 2／4／6px 间距替换为已有等值 token。

参考：`external/orca/docs/STYLEGUIDE.md`（5df67eff）。本项目继续使用 Astryx，
没有复制 Orca 的组件代码或资源。
