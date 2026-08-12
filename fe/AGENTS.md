# 前端架构

## 分层概览

运行时依赖单向流动：`app → features → systems → ui → core`。层级可以向下跨层，但不得向上导入；feature 域之间不得互相导入。`styles` 是有明确 owner 的非运行时层。

## 放置规则

跨平台、平台无关逻辑放在 `core`；浏览器组装与行为放在 `web/src` 下的 `app`、`features`、`systems` 和 `ui`。`web/src` 根目录只能有 `main.tsx`。不得新增 `shared` 目录或 barrel 文件。

`index.*` 文件不得包含任何 import/export 依赖。这是有意采用的严格入口规则，不仅禁止 re-export barrel；需要显式入口时使用有语义的文件名（如 `public.ts`）。

## 变更申请

冻结接口与全局样式契约在实现期间只读。接口不足时应暂停并向 owner 层提交变更申请，由 orchestrator 决策并广播；agent 不得分叉或静默放宽契约。

## 验证

每道架构门禁都需要正反 fixture。检查应静态且明确；不得弱化规则，新增 allowlist 必须限定到窄路径并记录理由。

新增或修改任何门禁时，必须同时说明它会静默放行的场景，并提供一个单违规 fixture，实测证明该场景现在会红。规则的每个目标分支都必须有独立反例；每条规则只有一个 fixture 不够：若某个 alternation 分支被删掉，只覆盖另一分支的 fixture 仍会全绿。

fixture 方法论有固有边界：同源产物互比（两侧可能一起陈旧）、CI job 是否真正被 required check 强制、开放世界的目录名与扩展名全集，以及 mutation 的 `expected_red` 是否真正守住业务契约，都不能靠 fixture 证伪，必须由独立 oracle、人工语义审查或真实生成器执行承担。

模块级静态数组必须运行时冻结；`as const` 只提供类型只读性，不改变运行时对象。应写成 `Object.freeze(['a', 'b'] as const)`。模块状态规则的 pure-factory 白名单仅接受经正反 fixture 证明不会产生可变模块对象图的具体 API；扩充时须在 `tools/architecture/no-module-runtime-state.mjs` 登记 import 来源与导出名，并补误报回归及反向变异。
