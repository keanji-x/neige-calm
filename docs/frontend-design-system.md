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
