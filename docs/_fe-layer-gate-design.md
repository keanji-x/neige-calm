# fe 层序门禁设计（issue #997 目标 1「依赖方向单向且可 lint」）

基线：只读 worktree `.claude/worktrees/997-c1-today`，HEAD `71288fbd`（= origin/main）。
所有结论均在该 worktree 实跑得出；下文「事实」= 我实际运行/读到的，「推断」= 由事实外推。
注：该 worktree 有 3 个**先于本次调研存在**的未提交改动
（`fe/web/src/app/shell/{shell.module.css,sidebar.tsx,sidebar.test.tsx}`，非我所改）。
`git diff` 无任何 import 行增删，且三者都在 `app/`（顶层，可向下依赖任意层），
不影响 §3 的违规计数。

---

## 0. 结论摘要（先纠正任务前提）

**事实**：层序门禁**已经存在且已在 CI 生效**，不是靠 code review 兜底。它不在 `fe/eslint.config.js`、
也不在 `fe/tools/architecture/*.mjs`，而在 **`fe/.dependency-cruiser.cjs`**，由
`fe/package.json:31` 的 `lint:depcruise` 调用，`lint` 聚合（`package.json:29`），
CI 在 `.github/workflows/ci.yml:527`（job `fe-unit`）执行 `npm run lint`。
`fe/tools/architecture/` 里没有层序 checker，是因为 depcruise 已经做了这件事。

**事实**：`fe/eslint-rules/` 目录不存在。ESLint 侧的自定义规则统一放在
`fe/tools/architecture/*.mjs`，经 `tools/architecture/plugin.mjs` 注册为 `architecture/*` 插件规则，
在 `eslint.config.js:59-73` 按 files glob 启用。ESLint 负责**语法/副作用**类规则
（模块级可变状态、持久化、DOM 查询、`calm:` key、core 平台逃逸），
**不负责 import 图**——import 图全部交给 depcruise。这个分工是既有的，新门禁不应打破。

**事实**：`_1057` 评审人眼抓到的那条 `systems/events → app/events` 向上导入
（`docs/_1057-fe-events-live-design-review-subagent.md:61-63`）**是设计稿里的伪代码，不是已落地代码**；
我已用变异证明：即使它真的写成 `import type`，`systems-no-features-or-app` 也会红（见 §3.2 变异 B）。
评审只是把它提前到设计期抓住了，不构成门禁缺失的证据。

**因此本设计的真实任务不是「从零造门禁」，而是补三个已证实的洞**（§4），
以及给 §5 的评估结论。**现存违规数 = 0**（§3），门禁不需要 allowlist 起步。

---

## 1. 现有门禁形状（新增检查必须长成这样）

### 1.1 两条通道

| 通道 | 入口 | 覆盖 |
|---|---|---|
| depcruise | `package.json:31` `lint:depcruise` → `depcruise core web/src --config .dependency-cruiser.cjs` | 全部 import 图规则（含层序） |
| ESLint + 独立 `.mjs` checker | `package.json:30` `lint:js` → `eslint .` 然后串联 `node tools/architecture/check-*.mjs` | 语法/副作用规则 + 目录布局 |

独立 `.mjs` checker 的统一形状（`check-top-level.mjs`、`check-core-no-jsx.mjs`、
`check-breakpoint-literals.mjs`、`check-tracked-fixtures.mjs` 全同构）：

1. 导出一个纯函数 `checkX(rootPath = '.') → string`（空串 = 通过，非空 = 人读的错误行）；
2. 文件尾 `if (import.meta.url === \`file://${process.argv[1]}\`)` 做 CLI 自举，
   `console.error` + `process.exitCode = 1`；
3. 遍历用 `node:fs` 的 `readdirSync({ withFileTypes: true })` 递归，硬跳过 `node_modules`；
4. 不吃配置文件，白名单写成模块内 `Set` 常量并在旁边写理由。

### 1.2 fixture 机制（`tools/architecture/architecture.test.ts`）

- fixture 根：`tools/architecture/fixtures/<case-name>/{positive,negative}/`，
  目录内按真实仓库布局摆放（`core/...`、`web/src/...`）。
- 驱动：`architecture.test.ts:229` `for (const caseName of readdirSync(fixtures))` —— **自动发现**，
  新建目录即自动生成用例，不需要注册。
- 默认分支（`architecture.test.ts:161-183`）：`process.chdir(fixture)` 后调
  `dependencyCruise(['core','web/src'], { ruleSet: { forbidden: config.forbidden } })`，
  用的是 `fixture-config.cjs` —— 它 `require` 真实的 `.dependency-cruiser.cjs`
  并把 `exclude`/`tsConfig` 置空。**规则表是生产表，fixture 不复刻规则**（符合
  memory「test must drive production wiring」）。
- 断言（`architecture.test.ts:231-236`）：positive 退出 0、negative 退出非 0，
  且 negative 输出必须 `toContain` 期望串——默认期望串就是 case 名，
  例外在 `expectedViolation` map（`architecture.test.ts:187-227`）登记。
- `check-tracked-fixtures.mjs` 强制每个 fixture 目录必须被 git track（防空目录假通过）。
- ESLint 类规则另有 `architecture-rules.test.ts` + `tools/architecture/rule-fixtures/` 扁平 fixture，
  与层序无关。

**结论（推断）**：层序类新规则应写成 `.dependency-cruiser.cjs` 的 forbidden 条目
+ 一个 `fixtures/<rule-name>/{positive,negative}/` 目录，**零新代码**。
只有「不是 import 图」的部分（§4.3 的非运行时根）才需要新 `.mjs` checker。

---

## 2. 规则的精确定义

### 2.1 层序与既有规则的对应

层序（`fe/AGENTS.md:5`、`docs/_fe-architecture.md:19-53`）：
`app → features → systems → ui → core`，可向下跨层，不得向上，不得横向（feature 域间）。

| 契约 | 现有规则 | 位置 |
|---|---|---|
| core 不得触任何 web 层（含 styles） | `core-no-web-layers` | `.dependency-cruiser.cjs:4` |
| ui 不得向上；且 ui→core 仅三类白名单 | `ui-only-core-type-whitelist` | `:7` |
| systems 不得触 features/app | `systems-no-features-or-app` | `:8` |
| features 不得触 app | `features-no-app` | `:9` |
| 任何层不得 import `main.tsx` | `layers-no-main-entry` | `:10` |
| feature 域之间禁止横向 | `features-no-cross-domain` | `:11` |

`ui-only-core-type-whitelist` 比 AGENTS.md **更严**：ui 向下到 core 也只放行
`core/types/ids.ts`、`core/types/a11y.ts`、`core/state/types.ts` 三个文件，
对应 `docs/_fe-architecture.md:551`（§8 裁决 7）的三类白名单（branded ID / a11y 原语 / 基础设施类型）。
这是既有豁免约定，**新门禁沿用，不放宽**。

其余允许的向下边（`features→systems/ui/core`、`systems→ui/core`、`app→*`）无规则、也不该有规则。

### 2.2 type-only import 算不算违规

**表态：算。现状已经是这样，且必须保持。**

- 事实：`.dependency-cruiser.cjs:20` 设了 `tsPreCompilationDeps: true`，
  type-only 边进图并带 `dependencyTypes: ["local","type-only","import"]`；
  规则未写 `dependencyTypesNot: ['type-only']`，所以照常判违规。
  变异 B（§3.2）实测：`import type` 的 systems→app 被 `systems-no-features-or-app` 红掉。
- 理由：(a) `import type` 不产生运行时依赖但产生**编译期耦合**——上层类型一改，下层被迫跟着改，
  这正是 #997 目标 1 要消灭的「import 图看不见的真实耦合」的镜像；
  (b) `verbatimModuleSyntax: true`（`tsconfig.app.json`）下 `import type` 与 `import` 只差一个关键词，
  若豁免 type-only，任何违规都能一键降级为「合法」，规则失去封闭性；
  (c) `_1057` 那条正是 type-only 形态（`import type { SyncCursorPort }`），
  豁免 type-only 就等于放过它；
  (d) 唯一的既有 type 豁免是 `docs/_fe-architecture.md:552`（§8 裁决 6）——
  `no-module-runtime-state` 豁免 type-only **声明**，那是「声明不产生模块对象」，
  与「跨层 import 产生耦合」不是同一条推理，不能类推。

### 2.3 styles 与非运行时域

- `styles` 是有 owner 的**非运行时层**，不入五层序（`docs/_fe-architecture.md:52`）。
  它**不是** ui 之下的第六层。当前唯一被 core 侧禁止（`core-no-web-layers` 的 `styles` 分支，
  fixture `core-no-web-styles`）。
- 非运行时域（`tools/`、`mock/`、`e2e/` 与 `web/e2e/`）用 `verification_owner` 标记、不入五层
  （`docs/_fe-architecture.md:53`）。事实：depcruise 的 cruise 根只有 `core web/src`，
  这些目录既不被扫、也不被禁止成为**目标**（§4.3）。

### 2.4 建议新增的三条规则（精确定义）

```js
// A. styles 是叶子：非运行时层不得反向依赖任何运行时层
{ name: 'styles-no-runtime-layers', severity: 'error',
  from: { path: '^web/src/styles/', pathNot: '\\.(?:test|spec)\\.[cm]?[jt]sx?$' },
  to:   { path: '^(core/|web/src/(app|features|systems|ui)/|web/src/main\\.tsx$)' } },

// B. 运行时代码不得依赖非运行时域（mock / tools / e2e / web/e2e）
{ name: 'runtime-no-verification-domains', severity: 'error',
  from: { path: '^(core/|web/src/)', pathNot: '\\.(?:test|spec)\\.[cm]?[jt]sx?$' },
  to:   { path: '^(mock|tools|e2e|web/e2e)/' } },

// C. fail-closed：解析不出来的 import 不得静默通过
{ name: 'not-to-unresolvable', severity: 'error', from: {}, to: { couldNotResolve: true } },
```

C 的 `from: {}` 会同时覆盖 `core` 与 `web/src`，与 `no-circular`（`:17`）同形。
B 与 A 的 `pathNot` 排除测试文件是**必要的**：测试合法消费 `mock/` 或跨层契约（推断：当前无人这么写，
但 `test:mock-drift` 的存在说明 mock 是给测试用的）；若 owner 决定测试也走别的注入路径，
可去掉 `pathNot` 收紧——但那是另一个决定，本设计不替它做主。

---

## 3. 现存违规完整清单

### 3.1 清单：**0 条**（全量，非抽样）

两条独立通道互相印证：

1. 生产门禁：`node_modules/.bin/depcruise core web/src --config .dependency-cruiser.cjs`
   → `✔ no dependency violations found (184 modules, 507 dependencies cruised)`。
   图内含 **76 个 `.test.ts(x)` 模块**（除 A/B 明示的测试形状豁免外，仍受其余层序规则管辖）。
2. 我自己写的独立扫描器（正则抽 `import`/`export ... from`/裸 side-effect import/动态
   `import()`，自行做扩展名候选解析，自己实现层序模型），扫 `core` + `web/src` + `mock` + `tools`
   （`fe/e2e` 尚不存在，`playwright.config.ts:4` 的 `testDir: './e2e'` 目前无对应目录）：
   **459 个文件、690 条 import 边、0 条违规**。
   我的模型比生产规则宽（我把 `ui→core` 全部视为合法向下），仍为 0 → 两侧一致。

分类计数（事实）：向上边 0、feature 跨域边 0、systems 跨域边 0、`core→styles` 0、
运行时→非运行时（mock/tools/e2e）边 0。指向 `styles` 的边只有
`app→styles` 1 条（`web/src/app/theme/theme.browser.test.tsx:7` 的 `import '../../styles/tokens.css'`）
和 `styles→styles` 11 条。

补充事实：`tsconfig*.json` 均无 `paths`，`core`/`web/src` 内也没有任何非相对的内部 import
（grep `from '(core|web|@/|~/|mock|src)` 无命中），所以「只解析相对路径」的扫描是完备的，没有别名盲区。

**含义：三条新规则可以一次性开启，不需要 allowlist、不需要分阶段。**

### 3.2 可证伪性验证（变异，在 scratchpad 的临时副本上做，主仓与 worktree 未改动）

| 变异 | 边 | 形态 | 结果 |
|---|---|---|---|
| A | `systems/cards/_mutA.ts → app/cards.ts` | value import | 红 `systems-no-features-or-app` ✅ |
| B | 同上 | **`import type`** | 红 `systems-no-features-or-app` ✅（type-only 被覆盖） |
| C | `features/today → features/cove/palette.ts` | value | 红 `features-no-cross-domain` ✅ |
| D | `ui/_mutD.ts → systems/cards/public.ts` | `import type` | 红 `ui-only-core-type-whitelist` ✅ |
| E | `ui/_mutE.ts → core/domain/conversation.ts` | `import type` | 红 `ui-only-core-type-whitelist` ✅ |
| F | `core/_mutF.ts → web/src/styles/breakpoints.ts` | value | 红 `core-no-web-layers` ✅ |
| G | `ui/_mutG.ts → ./totally-missing-module.ts` | 无法解析 | **绿**（`resolved: "unknown"`，0 规则命中）❌ |
| K | `styles/_mutK.ts → features/cove/palette.ts` | value | **绿** ❌ |
| M | `styles/_mutM.ts → app/router/navigation.ts` | value | **绿** ❌ |
| L | `ui/_mutL.ts → mock/generated/operations.ts` | value | **绿** ❌ |

A–F 证明既有层序门禁真的会红（含 type-only）；G/K/M/L 就是 §4 的三个洞。

---

## 4. 三个已证实的缺口

- **GAP-1 fail-open：无法解析的 import 静默通过（变异 G）**。config 里没有 `no-unresolvable`
  类规则。任何 depcruise 解析不动的 specifier（写错的相对路径、`.js` 扩展、
  非字面量模板）都会绕过**全部**层序规则。目前靠 `tsc -b` 兜底，但那是另一个 job，
  且 `mock/`、`tools/` 不在 `tsconfig.app.json`/`tsconfig.core.json` 的 include 内。
  违反 memory「fail-closed fence semantics」。→ 规则 C。
- **GAP-2 styles 出边完全无管束（变异 K/M）**。`web/src/styles/` 下有真实 TS
  （`breakpoints.ts`、`font-stack.ts`、`public.ts`），它 import features/app 无人拦。
  非运行时层反向依赖运行时层等于把「有 owner 的样式契约」变成业务代码的下游。→ 规则 A。
- **GAP-3 运行时可以 import mock/tools（变异 L）**。`mock/generated/operations.ts` 是
  `mock:generate` 的产物，被生产代码 import 会把 fixture 打进 bundle。→ 规则 B。

次要（**不建议本轮做**，记录以免反复讨论）：
`systems/*` 域之间的横向依赖无规则（我构造的 systems 跨域边确实不红）。
`AGENTS.md:5` 只对 feature 域下了禁令，`docs/_fe-architecture.md:29-36` 也只在 features 框里标了
「域之间禁止横向依赖」。systems 目前只有 `cards`、`events` 两个域且无横向边——
在没有 owner 裁决前加规则属于「弱化/强化契约的分叉」，应走 §「变更申请」流程。

---

## 5. 正反 fixture 的具体形状

沿用 `fixtures/<rule-name>/{positive,negative}/` 自动发现约定，每个文件保持
既有 fixture 的极简风格（`systems-no-features-or-app` 的 negative 只有两个文件、共两行）。

```
fixtures/styles-no-runtime-layers/
  negative/web/src/styles/bad.ts        import '../features/inbox/value.ts';
  negative/web/src/features/inbox/value.ts   export const value = 1;
  positive/web/src/styles/good.ts       import './sibling.ts';
  positive/web/src/styles/sibling.ts    export const value = 1;

fixtures/runtime-no-verification-domains/
  negative/web/src/ui/bad.ts            import '../../../mock/generated/value.ts';
  negative/mock/generated/value.ts      export const value = 1;
  positive/web/src/ui/good.ts           import '../../../core/types/ids.ts';
  positive/core/types/ids.ts            export type Id = string;

fixtures/not-to-unresolvable/
  negative/web/src/ui/bad.ts            import './missing.ts';
  positive/web/src/ui/good.ts           import './present.ts';
  positive/web/src/ui/present.ts        export const value = 1;
```

三点注意（否则 fixture 是假的）：

1. `runtime-no-verification-domains` 的 negative 需要 `mock/` 目录进 fixture，
   而 `architecture.test.ts:163` 的 cruise 入口写死 `['core','web/src']` 并按 `existsSync` 过滤——
   `mock/` 作为**目标**会被跟随进图，无需成为入口，**不用改 harness**（推断，需实现时实跑确认）。
2. 三个 case 的 negative 输出必须包含规则名。默认期望串就是 case 名 →
   规则名与目录名同名即可，**不需要往 `expectedViolation` map 加条目**。
3. fixture 目录必须 `git add`（含每一层目录都要有 tracked 文件），否则
   `check-tracked-fixtures.mjs` 会红。

## 6. 改动文件清单

| 文件 | 改动 |
|---|---|
| `fe/.dependency-cruiser.cjs` | forbidden 数组新增 3 条（A/B/C），插在 `no-circular` 前 |
| `fe/tools/architecture/fixtures/styles-no-runtime-layers/**` | 新增 4 个文件 |
| `fe/tools/architecture/fixtures/runtime-no-verification-domains/**` | 新增 4 个文件 |
| `fe/tools/architecture/fixtures/not-to-unresolvable/**` | 新增 3 个文件 |
| `fe/tools/architecture/README.md` | 记录三条新规则的判定边界与已知逃逸 |
| `fe/AGENTS.md` | §分层概览补一句：styles 是叶子，非运行时域不得被运行时代码 import |

**零新增 `.mjs` checker、零 ESLint 规则、零 `npm run` 脚本改动**——
`lint:depcruise` 与 `architecture.test.ts` 的自动发现把新规则接完。
不需要 allowlist（§3.1 已证违规为 0），不需要分阶段。

## 7. mutation 加 typecheck 通道的评估（顺带项）

事实：`fe/tools/mutation/run.mjs:73` 只跑 `npx vitest run --reporter=json`；
`runner.ts` 的 `judgeMutation`/`parseVitestReport` 全部围绕 vitest 的 `failedTestIds` 建模，
`expected_red` 是 test id 数组。当前 `manifest.json` 21 条，target 仅 6 个文件，
无一条 target 是 `.dependency-cruiser.cjs` 或 tsconfig。

**结论：不要加第二条 `tsc -b` 通道。改用「把类型级契约表达成 vitest 测试」，成本约为 1/10。**

理由（事实）：`architecture.test.ts:135-141` 的 `core-platform-types` / `core-platform-node-types`
两个 case **已经**在 vitest 里以编程方式跑 TypeScript
（`ts.createProgram` + `ts.getPreEmitDiagnostics`，断言诊断码 `2584`/`2591`），
并已被 `expectedViolation` map 登记。也就是说**类型级 gate 已经可以被 vitest 观测、
因此已经可被现有 mutation harness 证伪**——所谓「不可证伪」只对「没有写成 vitest 用例的类型契约」成立。

两条路径的成本对比（推断）：

| 方案 | 改动面 | 风险 |
|---|---|---|
| 加 `tsc -b` 通道 | `run.mjs` 加一次 spawn；`runner.ts` 的 `MutationRunResult`/`judgeMutation`/`expected_red` 全部要引入第二种「红」的身份（诊断码 or 文件:行），`runner.test.ts` 大面积跟改；每条 mutation 多一次全量 `tsc -b`（21 条 × 冷启） | 高。改的是判决核心，且 `expected_red` 的双命名空间会污染现有 21 条的语义 |
| 类型契约写成 `ts.createProgram` 的 vitest 用例 | 新增 fixture + 一个 `it()`，复用现有 `core-platform-types` 形状；mutation manifest 照常填 test id | 低。判决核心零改动 |

附带发现（本轮不解决，但影响 mutation 覆盖层序门禁）：`run.mjs:44` 的 `defends` 命名空间
`arch-rule` 是从 `architecturePlugin.rules`（6 条 ESLint 规则）取的集合
（`runner.ts` `validateManifest`），**depcruise 规则名不在合法命名空间内**，
所以层序规则无法出现在 `defends` 里。层序规则的可证伪性目前由
`architecture.test.ts` 的正反 fixture 承担（negative fixture 就是永久变异体），
这在我看来是足够的、也更便宜；若将来要把 depcruise 规则纳入 manifest，
只需在 `run.mjs:44` 的 namespaces 里加一个从 `.dependency-cruiser.cjs` 的
`forbidden.map(r => r.name)` 派生的 `dep-rule` 集合——约 3 行。
