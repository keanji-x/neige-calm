# UI layer

## 放什么

Dialog、menu、focus、roving、directory-browser、schema-form fields，以及 core state 的 React hook wrapper 等交互原语。

## 不放什么

不放业务 domain 字段、页面流程、system 生命周期或 app provider。不得用 `core/domain` 作为后门。

## 依赖方向

只依赖 `core`；core 类型仅可从显式 `core/types/ids.ts`、`core/types/a11y.ts` 获取 branded ID 和无障碍原语类型，或从 `core/state/types.ts` 获取 `Persistent<T>`、codec、storage port 等基础设施类型。判据是类型不得携带业务 domain 字段；`core/domain` 始终禁止。

## 契约模板

Primitive 契约写清 props、role/name、focus/keyboard 行为及注入 port；业务无关且可独测，冻结后改动走 change request。

## 交互动效默认规则

新增或修改的位移、尺寸、进入与退出动画统一复用 `ui/motion/spring.ts` 的 Motion 物理弹簧；尺寸使用 `SizeMotion`，成对浮层使用 `useSpringPresence`。组件只提供目标状态和几何映射，不另设时长、缓动曲线、响应频率、距离倍率或弹簧求解器。共享手感由 UI owner 集中调整。

保持业务状态与焦点即时更新、文字不缩放、反向时继承速度，并覆盖取消、卸载及减少动态效果。颜色反馈、直接跟手和装饰循环沿用其已声明的独立契约；其他例外须在 owner 层说明原因，不静默分叉通用运动规则。
