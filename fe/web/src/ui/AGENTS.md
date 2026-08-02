# UI layer

## 放什么

Dialog、menu、focus、roving、directory-browser、schema-form fields，以及 core state 的 React hook wrapper 等交互原语。

## 不放什么

不放业务 domain 字段、页面流程、system 生命周期或 app provider。不得用 `core/domain` 作为后门。

## 依赖方向

只依赖 `core`；core 类型仅可从显式 `core/types/ids.ts` 与 `core/types/a11y.ts` 获取 branded ID 和无障碍原语类型。

## 契约模板

Primitive 契约写清 props、role/name、focus/keyboard 行为及注入 port；业务无关且可独测，冻结后改动走 change request。
