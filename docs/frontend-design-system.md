# 前端设计系统

## 定义入口

- `fe/web/src/styles/tokens.css`：语义变量和浅色／深色主题。
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
