# Features layer

## 放什么

Track、Area、Today、Report、Planner、Settings、Auth 等页面业务行为与端侧业务组合。

## 不放什么

不放独立资源生命周期/宿主协议、通用交互 primitive、平台无关模型，也不得经 `app` 中转跨域。

## 依赖方向

可依赖 `core`、`ui`、`systems`；feature 域之间禁止横向 import，共享模型下沉 core，组装关系上提 app。

## 契约模板

每个域公开窄入口，记录消费的 core/system 接口和行为契约；禁止域级 barrel，冻结面变更提交 change request。
