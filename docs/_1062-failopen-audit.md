# Issue #1062：fe fail-open 审计（第一刀）

审计基线：`446baa6c`（`origin/main`，2026-08-12）。本报告只登记事实和风险，不修改门禁。
issue 写“15 条 forbidden”，本基线实有 **12 条**；下表按代码的每个目标分支展开。

## 证据与判读

- **红**：单违规输入被门禁拒绝；**绿**：所列违规/失效输入返回 0。只有实际执行结果才使用红/绿。
- **E1（现有反例总跑）**：`npm run test:mutation:fixtures` 输出
  `mutation fixture e2e: 8/8 passed`；随后 Vitest 输出
  `Test Files 7 passed (7)`、`Tests 241 passed (241)`。verbose 输出逐项包含本文引用的测试名。
- **E2（双陈旧 wire，临时 clone）**：同时给
  `web/src/api/generated-events.ts` 和 `fe/core/api/generated/wire.ts` 首行加入相同文本后运行
  `npm run test:wire`：`WIRE_BOTH_STALE_EXIT=0`，无 diff 输出。
- **E3（CI 路径遗漏，临时 clone）**：只修改 `fe/ownership-manifest.mjs`，执行 CI 原命令
  `git diff --exit-code -- web/src/api/openapi.json web/src/api/generated.ts web/src/api/generated-terminal.ts web/src/api/generated-events.ts web/src/editor/types/`：
  `OPENAPI_UNLISTED_FE_EXIT=0 OUTPUT_BYTES=0`；`git status` 同时为 `M fe/ownership-manifest.mjs`。
- **E4（空/错根）**：直接调用导出函数：
  `checkCoreNoJsx('/tmp/does-not-exist', ...) => CORE_JSX_MISSING_ROOT_EXIT=0 OUTPUT=""`；
  `checkTopLevel('/tmp/does-not-exist') => TOP_LEVEL_MISSING_ROOT_EXIT=0 OUTPUT=""`；
  `checkDuplicationManifest('/tmp/does-not-exist') => DUP_MISSING_ROOT_EXIT=0 OUTPUT=[]`。
- **E5（readonly 事件路由）**：同一提交直接验证得到
  `[{"rule":"readonly-change-trailer",...}]`，但
  `ownershipCommitsForEvent('push', load) => []`；E1 也显示
  `skips loading trailer-range commits for push events` 通过。
- **E6（零 CSS）**：临时根只放合法 `web/src/styles/breakpoints.ts`、不放 CSS，运行 checker：
  `BREAKPOINT_NO_CSS_EXIT=0`。
- **E7（基线生成物）**：`npm run test:wire` 为 `WIRE_BASE_EXIT=0`；
  `npm run test:mock-drift` 为 `MOCK_BASE_EXIT=0`。
- **E8（调用链实查）**：`npm run lint` 调用 ESLint、eslint hygiene、tracked fixtures、breakpoint、
  ownership、test-tier、depcruise、top-level、core-no-jsx；`npm test` 调用 Vitest、wire、mock-drift；
  CI 的 `fe-unit` 跑 lint/build/test，`fe-browser` 跑 browser，`fe-mutation` 单独跑 mutation。

## 主表

| 门禁 / 目标分支 | 守护的契约 | 可写成 fixture 的静默放行场景 | 实测 | 证据 |
|---|---|---|---|---|
| depcruise `core-no-web-layers`: core→app | core 不依赖 app | 只测 core→ui，app 分支后来改坏 | 红：规则；无独立分支反例 | E1 仅 `core-no-web-layers`（→ui） |
| 同上：core→features | 同上 | features alternation 被删 | 红：规则；无独立分支反例 | 同上 |
| 同上：core→styles | 同上 | styles alternation 被删 | 红：独立反例 | E1 `core-no-web-styles` |
| 同上：core→systems | 同上 | systems alternation被删 | 红：规则；无独立分支反例 | E1 仅 →ui/→styles |
| 同上：core→ui | 同上 | import 拼错为不存在模块，depcruise 无 `no-unresolvable` | 红：negative；positive 仍是坏路径 | E1 `core-no-web-layers`; positive 当前为 `../../../../core/good.ts` |
| `ui-only-core-type-whitelist`: ui→app | ui 不反向依赖 app | app alternation 被删 | 红：规则；无独立分支反例 | E1 只有 ui→core/domain |
| 同上：ui→features | ui 不反向依赖 feature | features alternation被删 | 红：规则；无独立分支反例 | 同上 |
| 同上：ui→systems | ui 不反向依赖 system | systems alternation被删 | 红：规则；无独立分支反例 | 同上 |
| 同上：ui→非白名单 core | ui 仅可用指定 core 类型 | 负向前瞻或扩展名锚点放宽 | 红 | E1 `ui-only-core-type-whitelist` |
| `systems-no-features-or-app`: systems→features | systems 不反向依赖 feature | alternation漏 features | 红 | E1 同名 fixture |
| 同上：systems→app | systems 不反向依赖 app | alternation漏 app | 红：规则；无独立分支反例 | E1 fixture 只测 →features |
| `features-no-app`: features→app | feature 不反向依赖 app | app 路径前缀改名 | 红 | E1 同名 fixture |
| `layers-no-main-entry`: core→main | core 不依赖组合根 | from alternation漏 core | 红：规则；无独立分支反例 | E1 fixture 只测 ui→main |
| 同上：features→main | feature 不依赖组合根 | from alternation漏 features | 红：规则；无独立分支反例 | 同上 |
| 同上：systems→main | system 不依赖组合根 | from alternation漏 systems | 红：规则；无独立分支反例 | 同上 |
| 同上：ui→main | ui 不依赖组合根 | from alternation漏 ui | 红 | E1 同名 fixture |
| `features-no-cross-domain` | feature 只依赖本域或更低层 | `$1` 回引/目录深度改变后同域判断失效 | 红 | E1 同名 fixture |
| `core-no-react`: react | core 与 React 解耦 | 包匹配只剩 react-dom | 红：React；无 react-dom 独立反例 | E1 `core-no-react` |
| 同上：react-dom | 同上 | 包匹配只剩 react | 红：规则；无独立分支反例 | 同上 |
| `no-barrel-index`: import | index 不承载 import 依赖 | dependencyTypes 漏 `import` | 红 | E1 `no-barrel-index-import-export` |
| 同上：export | index 不承载 re-export | dependencyTypes 漏 `export` | 红 | E1 `no-barrel-index` |
| 同上：8 个扩展名 | 所有 JS/TS 模块扩展一致受限 | 扩展名 alternation 漏一项 | 红：`.ts`、`.mts`；其余 6 项无独立反例 | E1 `no-barrel-index`, `-mts` |
| `cards-public-entry-only`: core consumer | 外部只从 cards/public.ts 进入 | from 漏 core | 红：规则；无独立反例 | E1 fixture 是 feature consumer |
| 同上：web consumer | 同上 | pathNot 范围扩大到整个 web | 红（静态与 dynamic import） | E1 `cards-public-entry-only/-dynamic` |
| `markdown-public-entry-only`: core consumer | 外部只从 markdown/public.ts 进入 | from 漏 core | 红 | E1 `markdown-public-entry-core` |
| 同上：web consumer | 同上 | from 漏 web 或 dynamic import | 红（静态与 dynamic） | E1 `markdown-public-entry-only/-dynamic` |
| `no-shared-directory` | 任意依赖不得指向 `shared/` | 新增未被 cruise 输入包含的根中建 shared | 红：已扫描根；新根未证伪 | E1 同名 fixture；E8 输入仅 core/web/src |
| `no-circular` | 已扫描依赖图无环 | 环位于未扫描新根，或边不可解析 | 红：已解析环 | E1 同名 fixture |
| `check-breakpoint-literals` | CSS media width 与 TS 断点常量一致 | CSS glob 为空 | **绿** | E6 |
| `check-core-no-jsx`: core | core 不含 JSX 文件 | core 路径拼错/改名/不存在 | **绿** | E4 |
| 同上：cards registry | registry 保持 `.ts` | CLI 只传 core，默认 cards 路径随 cwd/目录改名失配 | 红：现 fixture；错根绿 | E1 `cards-registry-no-jsx`; E4 |
| `check-top-level`: web/src | 仅允许约定顶层且无 shared 目录 | web/src 不存在/改名 | **绿** | E4 |
| 同上：core | 同上 | core 不存在/改名 | **绿** | E4 |
| `check-duplication-manifest`: unique symbol | INV-DUP 唯一实现位于 canonicalPath | core/web/src 根均不存在，扫描集合为空 | **绿** | E4；E1 public-symbol shapes |
| 同上：package import fence | 指定包只由 canonicalPath 引入 | 新语法形态或新源码根不被 AST 扫描 | 红：11 种形态；新根未证伪 | E1 package-import shapes |
| 同上：consumer canonical import | managed symbol 从 canonicalPath 引入 | computed source/property 等未建模形态 | **绿（明确列为非覆盖形态）** | E1 consumer shapes 中两类 `mustReject=false` |
| `check-eslint-hygiene`: nested config | 只有根 ESLint config | 新 config 扩展名/fixture 排除目录藏配置 | 红：已知 js/cjs/mjs/ts | E1 `eslint-config-root-only` |
| 同上：off/disableTypeChecked | 关闭规则必须紧邻 Reason | 规则值由 spread/变量间接构造，源码正则找不到声明 | 红：直接形态；间接形态未证伪 | E1 `eslint-no-off-shims` |
| `check-tracked-fixtures`: architecture | fixture 目录必须被 Git 跟踪 | 空文件而目录仍由别的文件判为 tracked | 红：未跟踪目录；空文件未证伪 | E1 architecture meta-test |
| 同上：mock fixtures/catalog | disk、Git、正负 catalog 三方一致 | 三方一起删除同一 fixture | 红：单边漂移；三方同陈旧未证伪 | E1 tracked mock fixture tests |
| ESLint `no-module-runtime-state` | 禁止模块级可变运行态 | 未建模的构造/赋值语法 | 红：广泛语法表；未知语法仍是风险 | E1 对应 49 个 verbose case |
| ESLint `no-direct-persistence` | 持久化只经 core key/注入端口 | API 经未跟踪别名/包装器调用 | 红：6 条直接/别名分支 | E1 对应 cases |
| ESLint `no-calm-key-outside-core-keys` | calm key 字面量只在 core/keys | 字符串拼接/变量数据流构成 key | **绿** | E1 `known-concatenation-escape` accepted |
| ESLint `no-class-dom-query` | 禁止 class selector DOM 查询 | selector 经运行时拼接/未知 helper | 红：动态值被拒；非调用语法未证伪 | E1 对应 12 cases |
| ESLint `no-core-platform-escape` | core 不绕过平台注入 | 非 `globalThis.fetch` 平台入口或未知动态加载形态 | 红：globalThis.fetch/import(); 其余未证伪 | E1 对应 cases |
| ESLint `no-create-context-outside-allowlist` | React context 只在显式文件 | React API 经未建模高阶包装调用 | 红：8 条调用形态 | E1 对应 cases |
| ownership entry/overlap | manifest entry 合法且恰一 owner | coverage 输入文件集合为空时，缺 owner 不出现 | 红：shape/overlap；空 existingFiles 对 coverage 绿 | E1 ownership tests |
| ownership coverage | 所有受管 tracked 文件恰一 owner | 新顶层不在硬编码 roots/controls | **绿（不进入 existingFiles）** | `repositoryFiles` roots 仅 core/mock/web/tools + 3 controls；E1 scope test |
| ownership readonly + trailer regex | readonly 变更逐 commit、逐精确路径批准 | push 事件直接返回空 commits | **绿（push）/红（PR）** | E5 |
| test-tier entry/location | migrated oracle 的 test tier 与项目能力相符 | oracle 文件非数组会被入口 `flatMap` 成空条目 | 红：字段/位置；非数组文件整体被忽略未证伪 | E1 checker tests；入口实码 |
| test-tier tracked tests | 每个 tracked test 恰属一个 Vitest project | 新测试后缀/扩展名不匹配入口正则 | 红：已知后缀；未知后缀未证伪 | E1 tier gate decisions |
| project-map Vitest config | config projects 有合法 name/include | `exclude` 非字符串数组被静默变成 `[]` | 红：name/include；坏 exclude 未证伪 | E1 vitest-projects；`stringArray` 行为 |
| project-map scripts/Playwright | 所有项目被脚本运行，e2e 根由 testDir 决定 | script 通过 wrapper/引号表达 project，正则解析不到 | 红：当前直接参数/testDir；wrapper 未证伪 | E1 vitest-projects |
| mutation manifest `defends` | 每条 mutation 引用存在的 oracle/arch-rule | 未知 namespace/id 或空 defends | **红** | E1 四个 `rejects invalid defends` |
| mutation manifest patch/paths | 单目标 patch 与 target 一致且路径 tracked | patch 语法合法但当前上下文不可应用 | **红** | E1 manifest tests；E1 fixture e2e 8/8 |
| mutation patch apply/revert/noop | patch 必须 check/apply/改字节/可回滚 | mode-only patch apply 成功但字节不变 | **红（judge）** | E1 `patch-noop cannot pass`; mode-only fixture |
| mutation `expected_red` | actual red 集合与声明精确相等且非空 | 少红、多红、零红、重复 expected/actual | **红** | E1 dead/under/over/duplicate verdict cases |
| mutation test infrastructure | 测试/报告异常不得伪装为语义红 | Vitest 非 0/1、报告坏、global/file error | **红** | E1 test-run/infrastructure/report cases |
| mutation PR selection | 目标或 selection_paths 相交才选；改 mutation infra 强制全选 | 新相关文件未列 selection_paths，selected=0 | **绿（普通 PR，设计如此）** | E1 `passes unrelated PRs`; exact-path selection test |
| mutation shards | 四片并集完整且互斥 | selected 数少于 shard 数导致空 shard | **绿（设计如此）** | E1 balanced partition + empty shard tests |
| `test:wire` | 两份 wire 产物字节相同 | 两个 checked-in 文件一起陈旧 | **绿** | E2 |
| `test:mock-drift` | mock 文件集/字节等于 OpenAPI+wire 再生成结果 | OpenAPI、wire 与 mocks 一起陈旧 | 基线绿；三方同陈旧无法独立 fixture 证伪 | E7；见下节 |
| CI `fe-unit` | lint/build/static+jsdom/wire/mock 全跑 | checker 未串入上述 npm scripts | 红：当前调用链完整 | E8 |
| CI `fe-browser` | browser project 真跑 Chromium | 新 browser-like 后缀未被 include | 红：当前 probe；新后缀未证伪 | E1 project tests；E8 |
| CI `fe-mutation` | PR 按交集、main 全量跑 mutation | 非 PR 非 push 事件所有 run step 都 skip | 当前 workflow 仅 PR/push；不可 fixture | E8；CI `if` 条件 |
| CI `openapi-drift` | Rust OpenAPI 及 TS 产物与提交一致 | 路径表没有任何 `fe/` 产物 | **绿** | E3 |

## 需要补的反例（按严重度）

1. **P0**：修 `openapi-drift` 产物清单，加入实际由生成链影响的 fe wire/mock 产物；用“仅 fe 产物漂移”CI fixture/脚本证明红。
2. **P0**：替换 `test:wire` 的“双 checked-in diff”为单一权威源生成比较；加入“两侧同陈旧”反例。`test:mock-drift` 同理需固定不可同陈旧的上游锚。
3. **P0**：readonly 在 `push` 事件不得把提交集合清空；加入 main push 修改 readonly、无 trailer 的红例。
4. **P0**：为 depcruise 每个 alternation 建独立负例：core→app/features/systems、ui→app/features/systems、systems→app、core/features/systems→main、react-dom、8 扩展名、cards core consumer。并修正 core-no-web-layers positive 的 `../../../core/good.ts`，增加“依赖确已解析”的断言。
5. **P1**：`core-no-jsx`、`top-level`、duplication checker 对必需扫描根不存在/零文件 fail closed；各加 missing-root 与 empty-root fixture。
6. **P1**：ownership coverage 不再用固定 roots 静默排除新 fe 顶层；加入 `fe/new-domain/file.ts` 反例。
7. **P1**：test-tier 对非数组 oracle、未知测试后缀/扩展、坏 `exclude` fail closed；加入入口级 fixture（不能只测纯函数正常输入）。
8. **P1**：mutation selection 增加“相关新文件但未列 selection_paths”报警或可审计兜底；保留普通无关 PR 的零选择绿，但证明分类正确。
9. **P2**：breakpoint checker 对 CSS 空集合报警；tracked fixture catalog 增加独立锚，防三方一起删。
10. **P2**：为正则/AST checker 固化当前明确 escape（calm-key 拼接、computed consumer source/property）为命名的风险 fixture，决定应拒绝还是正式声明非契约。

## 无法用 fixture 证伪的门禁

- **两个/三个产物一起陈旧**：repo 内 fixture 只能证明“给定当前输入能重生成一致输出”，不能证明 checked-in OpenAPI/wire 本身等于 Rust 真源。必须执行真生成器或引入独立摘要/来源锚；这是结构性风险，不是多加一个同源 snapshot 能解决。
- **CI job 是否实际被 required-check policy 强制**：workflow fixture 可证明 job 定义与命令，不能证明 GitHub branch protection/ruleset 的外部配置。需导出 ruleset/API 证据定期审计。
- **未来目录名、语法和测试扩展全集**：有限 fixture 不能证明开放世界的完备性。可做的是对必需根/允许扩展采用显式 manifest 并对未知项 fail closed。
- **mutation `expected_red` 的业务充分性**：精确集合判决能证明声明与一次运行一致，不能证明测试断言真的守住业务契约（#1057 T-B4 即此类）。必须由独立 oracle/人工语义审查或更高层端到端反例承担。
- **PR-only 与 push-only 路由的托管平台语义**：纯函数 fixture 可证明分支选择，不能模拟 GitHub 提供的真实 SHA、event payload 与 merge queue；需要 workflow 集成运行记录。

结论（事实）：当前已有反例质量明显提高，E1 的 241 项和 mutation 8/8 覆盖了大量语法及判决分支；但 E2--E6 仍实测得到五类 fail-open。推断：优先消除“同源产物互比”、事件整段跳过和缺根即成功，比继续扩充普通正例收益更高。
