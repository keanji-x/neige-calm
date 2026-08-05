# #997 阶段 1 follow-up 归属

原始出处均为 issue #997 的“阶段 1 进展交接”comment；末节四条来自“P0c / P8a 已合入”comment。`关闭`表示已由 P0c/P8a 的现有实现与 fixture 覆盖，`阶段 2`表示进入指定 slice，`不修`必须给出理由。

## P0a（5）

| 原始条目 | 当前状态 |
|---|---|
| `globalThis.fetch` / dynamic `import()` 绕过 restricted globals | 关闭：P0c 语法形态表与绕过 fixture 已覆盖 |
| assertion map 缺项回退 caseName 导致恒真 | 关闭：P0c fixture 元测试要求每个 negative 文件产生本规则违规 |
| CI 使用未声明的传递依赖 `semver` | 关闭：P0c 已显式声明依赖 |
| `core/platform-independent.ts` 应移入 tools | 阶段 2 `core/platform-boundary`：实现迁移时移除测试占位 |
| `web/src/app` 缺 `.gitkeep` | 不修：目录已有受跟踪实现文件，空目录占位已无意义 |

## P0b（1 组 / 8 个逃逸面）

以下八项均已由 P0c 的模块状态三张语法形态表及正反 fixture 关闭：标识符传递、getter 返回、白名单 factory 内藏可变、tagged template、正则 `lastIndex`、React namespace 再别名、遮蔽全局、TS enum。

## P1（3）

| 原始条目 | 当前状态 |
|---|---|
| `Persistent<any>` 塌成 `never` | 阶段 2 `core/state-persistent`：补 any 的 fail-closed 类型契约 |
| `ContainsPersistent` 外层分配式包装冗余 | 阶段 2 `core/state-persistent`：与前项一起简化并跑 dts 变异 |
| 防第二个未 brand storage port | 阶段 2 `core/storage-ports`：architecture gate fail-closed |

## P3（2）

| 原始条目 | 当前状态 |
|---|---|
| `RADIUS.filter(root.has)` 是唯一存在性过滤 | 阶段 2 `styles/tokens`：统一缺 token 的失败策略 |
| isFinite 失败不指出 token | 阶段 2 `styles/tokens`：诊断信息带 token 名 |

## P5（5）

五项均归阶段 2 `core/markdown-parity`：转义竖线表格误报；缩进代码块内 ≥65 层 `>` 的保守代价；HTML block 状态机未完整覆盖 CommonMark；差分语料 `.some()` 守卫弱；诊断扫描器不跳过 HTML block。前三项先扩充与 legacy 的差分语料，后两项先强化守卫，再改扫描器。

## P6（5）

| 原始条目 | 当前状态 |
|---|---|
| 两处不可达防御码 | 阶段 2 `systems/cards-registry`：逐处证明后删除或保留注释 |
| `slotInitials` 告警文案 | 阶段 2 `systems/cards-registry` |
| cards JSX 门禁非递归 | 阶段 2 `architecture/cards-jsx`：覆盖 nested registry 绕过 |
| README 缺 constructor `host.resolve === null` | 阶段 2 `systems/cards-controller` |
| `FALLBACK_SIZE {4,6,3,3}` 早冻 | 阶段 2 `systems/cards-registry` |

## P7（8）

| 原始条目 | 当前状态 |
|---|---|
| 空 items 的 Tab 分支无覆盖 | 阶段 2 `ui/roving` |
| visibility 漏 opacity/aria-hidden/content-visibility | 阶段 2 `ui/focus-visibility`，真实浏览器验证 |
| falsy ReactNode 标题无 accessible name | 阶段 2 `ui/dialog` |
| 双 open Dialog inert 记账非栈感知 | 阶段 2 `ui/dialog` |
| Enter 非绝对路径守卫冗余 | 阶段 2 `ui/directory-browser` |
| 嵌套 Escape 缺真实 Menu-in-Dialog | 阶段 2 `ui/menu-dialog-integration` |
| 18 条 `ui/*` oracle 未认领 | 阶段 2 各 `ui/*` owner slice，ownership manifest 为入口 |
| README 缺 why | 阶段 2 `ui/docs`，随对应 primitive 实现补齐 |

## 工具链（1）

`npm test -- <paths>` 会把参数传给后续 wire diff：不改 package script；阶段 2 文档和自动化统一使用 `npx vitest run <path>`，全量仍跑 `npm test`。

## P8b-1 source-anchor 递减基线（阶段 2）

以下五批由 `fe/tools/oracle/anchor-baseline.json` 的 218 条已知欠债驱动。每修正一条，必须在同一提交删除对应基线行；基线只能下降，不能新增或换子类。

1. a11y-contract + ui-primitives：逐条对拍并纠正 source；不能定案的条目保留在递减基线，记录 A/B 搜索证据。
2. app-dataflow + gates-types：同一标准。
3. cards-terminal + pages-shared：同一标准。
4. capabilities-e2e：同一标准。
5. 收口结构化标识符配置与不支持格式登记，更新鉴别力并复跑完整变异与三门。

每批验收均须提交逐条对拍表、该批红项前后计数，以及随机把 source 改为首个引用文件 `:1-3` 的鉴别力结果。只有提取器误提取的普通词可按 `(id, identifier)` 登记；同一误提取词累计三次时改进 `extractStatementIdentifiers`，不得继续登记。混合引用须逐项说明哪些不支持格式的位置未受机器检查。

## P0c / P8a 合入后新增（4）

### 变异证据未入库（阶段 2 验收阻断风险）

现状：历史 `scratchpad/mut-*.sh` 被 gitignore，CI 无法重放，报告可能与当前实现脱钩。候选方案：

1. 推荐：阶段 2 新建 tracked `fe/tools/mutation/`，每个 checker 提供可复跑 mutation manifest + runner；CI 分片运行。
2. 保留私有脚本，但 CI 只校验 manifest target 存在；成本低，仍无法验证“确实变红”。
3. 接入通用 mutation framework；覆盖最完整，但引入依赖与运行成本最高。

归属：阶段 2 `tooling/mutation-evidence`，采用方案 1 前不得把自报变异表当作唯一验收证据。本 slice 仍按 brief 使用私有 `scratchpad/mut-p8b1.sh`，并以 `cmp -s` 守卫每次变异确实生效。

其余三项均归阶段 2 `styles/layer-audit`：统一 `@layer ui { @layer {} }` 与 media 内匿名层判定；合并重复的 layer-import fixtures；把例外文件中 fail-closed 的无 layer import 独立命名为 `import-without-layer`。

## CAP-APP-076 拆条欠债

`CAP-APP-076` 当前包含两半可分语义：仅在 `?testMounts=1` 时暴露 theme driver，以及卸载时仅删除仍由自身持有的 driver。现有权威 browser 路径 `fe/web/src/app/theme/theme.browser.test.tsx:53` 覆盖前半（条件暴露）；后半由 jsdom successor 保护测试 `fe/web/src/app/theme/public.test.tsx:68` 覆盖。后续应在 oracle 层拆成两条契约，各自保留单一 tier、单一权威证据路径；本轮保持 `CAP-APP-076` 的 browser `authoritative_test` 不变。该 migrated 契约目前没有登记的靶向变异证据，拆条时需分别补齐。

## CAP-APP-006 / INV-APP-009 overlay 几何契约欠债

`CAP-APP-006` 与 `INV-APP-009` 退回 `pending`：`ui/dialog` 的 scrim/panel 样式尚未迁入 `fe/`，当前无法锁定 overlay 的视口几何与 hit-testing。解锁条件：将 `ui/dialog` 的 scrim/panel 样式迁入 `fe/` 并登记进 `web/src/styles/global-classes.yaml`；届时恢复几何与 hit-testing 断言，并将两条契约翻回 `migrated`。

此外，`CAP-APP-006` 的“面板内恰好一个可聚焦出口”尚无独立变异证据：三个既有变异中，去掉 `hideTitleRow` 会先红在焦点断言，未执行到 `toHaveLength(1)`。后续需用只增加第二个可聚焦出口的靶向变异验证这一半。

## INV-APP-008 fail-open 正向证据缺口

现有测试只在 `/api/version` 失败落定前确认 children 存在，落定后仅断言未清 cache；因此把 error 分支改成隐藏 children 仍无法稳定打红，不能登记为有效变异。后续应让 rejected query 明确落定（例如等待 error 状态的可观察信号），再正向断言 children 仍在且 overlay 不存在；靶向变异应只把 error 分支改为返回 `null`，并只打红这条 fail-open 测试。

## INV-APP-070 descope：主题镜像与唯一写入者

该契约保持 `pending`，整条旧规则及 22 个 fixture 已撤。前三次 AST 谓词都在实测中同时 fail-open 和误报。绕过语料包括：局部 `dataset` 变量、变量属性名、拼接属性名、模板 `outerHTML`、动态 dataset key、`Object.defineProperty`、对象 patch + `Object.assign`、别名 + `Reflect.set`、`attributes.setNamedItem`、绑定后的 `setAttribute`、`insertAdjacentHTML`、JSX `<html data-theme>`；第三次还可被一个根句柄写 N 次、`window['document']`、`globalThis['docu' + 'ment']`、ref/event 的 `ownerDocument`、`parentElement` 上溯和 import helper 返回句柄绕过，标识符不出现即完全 fail-open。

第三次谓词还误报 `document.addEventListener`、`document.querySelector`、只读 `document.documentElement.dataset.theme`（与 `INV-APP-071` 明确允许的同步读直接冲突），以及把写入抽成模块级函数再由 effect 调用的纯重构。本轮的局部 effect 形状检查只修正了最后一种误报中的 `function` 声明；模块级箭头函数 helper `const applyTheme = (r) => {...}` 再由 effect 调用仍会误红，因为谓词只在 `while (!ts.isFunctionDeclaration(current))` 中上溯，拿不到箭头函数名而被 filter 掉。本轮因此删除“唯一写入者”断言，只保留不宣称唯一性的局部 effect 形状检查。

新谓词自身还有两条实测边界。其一，等价的 `document.documentElement.setAttribute('data-theme', resolved)` 会误红：`writes` 为空并触发 `not.toEqual([])`，说明拼写枚举问题仍以“只认 `*.dataset.theme =`”的形式存在。其二，它对“写哪个元素” fail-open：effect 内改成 `const el = document.createElement('div'); el.dataset.theme = resolved;` 后仍绿，尽管根节点永不更新；真实根节点由 `public.test.tsx` 的行为断言纵深保护，这条结构断言本身并不锁定 `documentElement`。

后续推荐先对结果下断言：在 jsdom 用 `MutationObserver` 观察 `documentElement` 的 `data-theme`，验证一次 `resolved` 变化恰好产生一次 attribute mutation、卸载后不再产生；别名、`window['document']`、`ownerDocument` 和延迟写都应由结果暴露。跨文件部分再配模块归属式 eslint 规则。该方案由 review 提出但**未经实测**，后续独立切片必须先主动证伪再采用，不能作为既定施工前提。

## INV-APP-059 descope：React Query 默认 retry

`INV-APP-059` 退回 `pending`：statement 主语是 React Query 的默认 retry 策略，但 `fe/` 尚无 `defaultOptions`；`retryUnless401` 只作为 `ServerCompatGate` 的 per-query option 使用。当前既保留纯函数真值表，也断言该 query 传入同一个函数，但这些证据不能迁移“默认策略”契约。`INV-APP-060` 已补“移除 export”的靶向变异条目。

R15 新增的 `ServerCompatGate passes retryUnless401 as its query retry option` 是源码 AST 级断言，并非运行时引用验证。它封住的是 `retry` 被换成别的值或删除这一类退化；其直接动机是曾将该项改为 `retry: 3`，全库 210 passed、零红。已知局限有两面：即使别名仍指向同一个函数，断言也会误红；反向实测中，在 `ServerCompatGate` 新增一个不显式写 `retry` 的 `useQuery` 仍绿，该 query 会采用 React Query 默认三次重试（包含 401），正是 `INV-APP-060` 的 `why` 所述退化路径，因为断言只收集显式 `retry`；用同名局部 `const retryUnless401 = () => true` 遮蔽也仍绿。更好的形态（本轮不实现）是行为化断言：注入一个恒返回 401 的 `fetchVersion`，断言它只调用一次；该方案与引用或别名无关，直接验证真实重试行为，但**未经实测**，采用前必须先主动证伪。它只补“策略确实被挂上”这一独立于契约状态的证据洞，不改变 `INV-APP-059` 仍为 `pending`：statement 讲的是默认策略，而 `fe/` 无 `defaultOptions`。

## migrated authoritative_test 行号扫尾

全库 16 条 `migrated` 中有 14 条的 `authoritative_test` 仍只指向 `:1` import 行；R15 只修了当次改动碰到的三处。`tools/test-tier/checker.ts` 只校验 tier/project 映射，不校验行号是否实际落在断言上，因此门禁永远抓不到这类假权威证据。后续需开独立扫尾，逐条核对这 14 条及其多文件引用并回填真实断言行。

## E2E-CAP-THEME-011 system 模式 browser 证据缺口

权威 browser 测试目前只覆盖 `light` / `dark`，statement 明写的 `system` 尚未覆盖；后续应在 browser project 补 system 跟随 `matchMedia` 且镜像到根 dataset 的断言。

另有证据层级错位：`E2E-CAP-THEME-011` 的 `test_tier` 是 browser，authoritative 指向 `theme.browser.test.tsx:1`（import 行而非断言）；两条 defends 它的变异，其 `selection_paths` 与 `expected_red` 却全部落在 jsdom 文件，权威 browser 路径从未被变异验证。后续需修到真实 browser 断言行，并对该路径补靶向变异。
