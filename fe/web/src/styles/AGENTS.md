# Styles layer

## 放什么

全局 layer 顺序、tokens、base、vendor 汇入、全局类 manifest 与具名 unlayered 例外清单。

## 不放什么

不放运行时逻辑、组件业务样式或未登记全局类；普通第三方 CSS 不得绕过 vendor entry。

## 依赖方向

这是非运行时 owner 层，不参与 TypeScript import 层序。组件样式使用 CSS Modules 并进入相应 `ui`/`features` layer。

## 契约模板

全局规则注明 layer、selector owner、理由和验证方式；例外精确到 selector+property+expiry，冻结后新增须走 change request。
