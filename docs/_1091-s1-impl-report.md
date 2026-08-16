# #1091 S1 实施报告 — cards boot composition / 解析 / 无头过滤

分支 `feat/1091-s1-boot`。**已 rebase 到 `origin/main` = `6e35e79e`**（F0）。提交四条：
`a2862378`（实现 + 两条 OWNERSHIP-CHANGE trailer）、`1fd252fb`（mutation manifest）、
`22645e58`（评审 R1 修复 F1–F9 + 第三条 trailer）、`686d24a2`（评审 R2 修复 G1–G5 + 两条 trailer）。
未 push，未开 PR。
§1–§6 记录首轮实现，§7 记录 R1 修复轮，§8 记录 R2 修复轮；冲突时以序号大的为准。

## 1. 首轮改动概览

首轮 23 files, +688 / -112，净增 **+576 行**（目标 450–700）。核心：
`systems/cards/builtins/` 新增 `register.ts`（`BUILTIN_CARD_ORDER` 八项 + `registerAvailableBuiltinCards`）、
`spec.ts`、`wave-report.ts`、`headless-filter.ts` 及各自的测试；
readonly `public.ts` 作唯一 re-export 出口、readonly `README.md` 按设计 §2 改写（各带一条 trailer）；
`app/cards.ts` 收窄为 `bootCards(registry)` 薄 wrapper，旧的八参 `registerBuiltinCards` 与
`app/cards.contract.test.ts` 一并删除；`production-app.tsx` 是唯一组装点，经 `AppRouterDeps.cards` 注入 router；
新增 `router/test-card-runtime.ts` 供 13 个既有调用点补必填参数（Required Param Sweep）。
首轮门禁数字见 §7（rebase + 修复后重跑，以那一组为准）。

## 3. oracle 迁移与 mutation selection（首轮口径，已被 §7 F4–F7 更新）

首轮把 `INV-CARD-182 / 201 / 226` 三条 pending → migrated（jsdom / web-dom），
`INV-CARD-180 / 181 / 225` 保持 pending；新增 mutation 条目
`cards-headless-filter-display-index`（defends `oracle:INV-CARD-226`，
patch 把两个分支的 `originalIndex` 换成过滤后的展示 index）。
最终 oracle 状态与 mutation 结果以 §7 为准。

## 5. §1.6 router deps：选了「真实消费」这条路

`AppRouterDeps` 新增必填 `cards: CardRuntime`（`{ registry, host }`），`createRouteTree` 解构后
把 `cards.registry` 透给 `WaveRoute` → `WaveRouteBody`，由它用 `partitionWaveCards` 算出 CARDS 面板的卡片列表：
剔除已解析的 spec/wave-report，未被认领的仍保留，再按 `originalIndex` 合并回原始 wire 顺序。
这是设计 §1.2/§3.1 明写的第一批出口（spec/wave-report「仅做 resolver/过滤」，不进右侧清单），
所以它是真实产品行为而不是为过 typecheck 造的消费点——没有用 `void deps.cards` 或下划线丢弃。
`host` 在 S1 还没有消费者（挂载进 grid 覆盖层是 S2），但仍与 registry 同点创建并注入，
以免出现第二个对 boot 顺序敏感的组装点；`router/public.tsx` 的类型注释写明了这一点。

## 6. 偏离 brief 之处

1. **新增 `fe/web/src/app/router/test-card-runtime.ts`（+18）**，brief 未列。`cards` 设为必填后有
   13 个既有 `createAppRouter`/`createRouteTree` 调用点要补参（Required Param Sweep）；共享一个
   *真实* booted runtime 比在 5 个文件里各写一份 stub 更不容易和生产漂移。
2. ~~inventory 陈旧条目未清~~ / ~~未 rebase~~ —— 均已在评审修复轮做掉，见 §7 F8 / F0。
3. **`INV-CARD-226` 的 authoritative_test 用了多个行号**：三段断言分散在不同 `it` 里，只指一行会漏掉一半语义。
4. 报告文件本身**未提交**，与仓库里其它 `docs/_*.md` 评审文档一致保持未跟踪，避免污染 PR diff。

## 7. 评审修复轮（提交 `22645e58`）

**F0 rebase.** `01fb49f1` → `origin/main` `6e35e79e`，两条提交无冲突自动应用。机器校验并集：
`git diff --stat origin/main..HEAD` 与 rebase 前的 `git diff --stat 01fb49f1..a139fbd3` **逐字节相同**；
全文 diff 只差 blob 哈希与 `public.tsx` 的 hunk 行偏移（±1，`diffdiff` 共 32 行全是 `index`/`@@` 行），
无任何内容行增减；`git merge-base --is-ancestor origin/main HEAD` 为真。缺失与多出双向均为空。

**F1 BLOCKER（唯一用户可见行为无测试）.** 新增 `fe/web/src/app/router/wave-cards-panel.test.tsx`（5 条，
jsdom/web-dom），驱动真实 `createAppRouter` + `bootTestCardRuntime()`（真 registry + 真 builtins）+ 真 wave 路由，
断言 `[data-nc-card-inventory]` 列表：spec/wave-report 消失、未被认领的 codex/terminal 仍在、
顺序仍是 kernel wire 顺序、全部无头时出空态。S1 没有任何有面的 entry，
所以「有面的卡不被过滤」用了一个 `file-viewer` fixture entry —— 它是该文件里唯一的替身。
**自证**：把 `public.tsx:836` 的 `cards={panelCards}` 临时改回 `cards={cards}`，
新文件 **3 failed / 2 passed**（红），而同一变异下 `web/src/systems/cards/` 的 **49 条全部仍绿** ——
正是评审指出的形状。改回后 5 条全绿。该文件已加入 mutation manifest
`cards-headless-filter-display-index` 的 `selection_paths`，并按实测把
`wave route CARDS panel [INV-CARD-226] renders exactly the surviving cards in the kernel wire order`
加进 `expected_red`（runner 做 expected/actual 集合相等，多一条少一条都红）。
同时把该文件写进 `INV-CARD-226` 的 `authoritative_test`（原来只指纯函数测试，正是「断言了函数没断言它被调用」）。

**F2 `HEADLESS_CARD_TYPES` 双向机器约束.** `register.contract.test.ts` 新增 3 条，对**生产 registry
的真实 entries**（`createCardRegistry()` + `registerAvailableBuiltinCards`）做 set-equality：
(a) 每个已注册 entry，`isHeadlessType(type)` 必须恰好等价于可观测事实「`component(...)` 返回 null
且 `defaultSize` 为 1×1」；(b) 列表里每个 type 必须真的被注册（这条挡「先加名字、entry 还没落地」
的隐形期）；(c) 列表 ⊆ `BUILTIN_CARD_ORDER`。**三次变异各自单独跑，各自变红**：
- 误加 `'terminal'` → `names only types that are really registered` 红（此前全仓 1301 条全绿）；
- 删掉真成员 `'wave-report'` → `classifies every registered entry by what it observably is` 红；
- 把 `SPEC_CARD_ENTRY.defaultSize` 改成 4×6（声明无头但实际有面）→ 同一条红。

**F3 spec 判别符三份拷贝.** `isSpecHarnessPayload` 经 `systems/cards/public.ts` 导出，
`public.tsx` 的两处手抄（`requestedCard`、`specCard`）改为调用它。public.ts 是 readonly，本轮提交带其 trailer。

**F4 owner/runtime 漂移.** 采纳 subagent 口径：`source:` **保留 legacy 路径不动**（本仓惯例）；
`INV-CARD-182 / 201 / 226` 的 `owner_slice` 与 `runtime_layer` **成对**改为
`systems/cards/builtins` + `systems`（validator `runtime-owner-layer` 要求前缀相等）。
`owner-aliases.yaml` 第 6 条「cards/report 临时归属」改写为已定案并写明依据。
**额外一条**：`INV-CARD-181` 同样改（F6 把它翻 migrated 且 authoritative test 落在 systems 侧，
留 `features/spec/card` 就是新造一处 F4 所指的同类漂移）。

**F5 `INV-CARD-224` 空悬不变量.** 先读 `fe/tools/oracle/validator.ts:393-400` 确认 skipped 的合法组合是
`skip_reason` 非空 **且** `verification_owner` 必须为 `null`（非 skipped 则反过来，禁止出现 `skip_reason`）。
按此改 `migration: skipped` + `verification_owner: null` + `skip_reason`（守卫与 `no-module-runtime-state`
冲突；registry 已改实例持有，重复 type 覆盖提供实例级幂等）。

**F6 `INV-CARD-181` —— 对定稿设计的有意偏离.** 设计 §6-S1 明写 181 保持 `pending`，理由是
「原条目 tier 是 browser/e2e，本片无该预算」。读 validator 后确认可以翻：`kind: invariant` 对
`migration: migrated` 无额外限制，`authoritative_test` 只校验 location 合法，
`tools/test-tier/checker.ts:15-20` 的 `EXPECTED_PROJECTS.jsdom = ['web-dom','browser']` 接受
`spec.test.ts`（`web/src/**` → web-dom）。**偏离理由**：statement 就是对一个冻结对象的三次属性读取
（component 返回 null、1×1、kernel-minted-only），jsdom 完全可证；`spec.test.ts:42,48` 两条测试早已打了
`[INV-CARD-181]` 标签并合起来覆盖 statement 全部语义 + `claim === undefined`；
姊妹条目 `INV-CARD-201` 本片已按 jsdom 翻 migrated，181 不翻自相矛盾，且会留下
「标签认领了但 oracle 写 NONE」的两头不靠。故改
`test_tier: jsdom` / `verification_owner: unit` / `authoritative_test: …spec.test.ts:42,48` / `migration: migrated`。
未取 fallback（删标签）。

**F7 `CAP-CARD-183` 前提被废.** 读 validator 确认 `kind: capability` 与 `migration: skipped` 无冲突
（skipped 的三条规则只看 `skip_reason` / `verification_owner`，不看 kind），故取优先方案：
`migration: skipped` + `verification_owner: null` + `skip_reason` 写明「spec 已被 INV-CARD-226
过滤出所有清单，该 accessible name 没有消费者；生产取 `'Spec harness'`；将来 spec 重获可见行须重开本条」。

**F8 inventory 陈旧条目.** 删掉 `fe/module-file-inventory.yaml:53`（已删除的 `app/cards.contract.test.ts`）。
该文件自身 readonly，本轮提交带**第三条** trailer
`OWNERSHIP-CHANGE: fe/module-file-inventory.yaml — drop the entry for the deleted app/cards.contract.test.ts (#1091)`。
**自证 trailer 不是空转**：在临时分支上把本轮提交的 message 换成无 trailer 的一行，
`check-readonly-change-requests.mjs` 立刻两条红
（`changes frozen fe/module-file-inventory.yaml` / `fe/web/src/systems/cards/public.ts`）；带 trailer 时 0 violations。

**F9 空转断言 / 姊妹不对称.** 删掉 `wave-report.test.ts` 里在深等于之后不可能失败的
`expect(...create?.mode).not.toBe('generic')`；补上与 `spec.test.ts:51` 对称的
`expect(WAVE_REPORT_CARD_ENTRY.claim).toBeUndefined()`。因此 `INV-CARD-201` 的
`authoritative_test` 行号随之更新为 `:8,18,24`。

**「不改代码只记录」（N2）**：未动，留给 S4a。

### 修复轮门禁实际数字（Node v22.22.2，`fe/` 下）

- `npm run typecheck`（`tsc -b`）：exit 0，无诊断。
- `OWNERSHIP_BASE_SHA=origin/main npm run lint`：exit 0。`ownership manifest: 98 entries, complete coverage,
  no overlaps`；test-tier 通过；depcruise `✔ no dependency violations found (259 modules, 844 dependencies cruised)`。
- `npm test`：**Test Files 123 passed | 1 skipped (124)；Tests 1333 passed | 1 skipped (1334)**；mock-drift exit 0。
- `npm run test:browser`：**Test Files 7 passed (7)；Tests 13 passed (13)**。
- `node tools/mutation/run.mjs --shard 2/4`（本片的 `cards-headless-filter-display-index` 落在该 shard）：
  `selected: 66, ran: 17`，**17/17 `verdict.ok = true`**，其中本条的 `actual_red` 与新的四条 `expected_red`
  完全相等。跑前把未跟踪的 `docs/_1091-s1-impl-report.md` 移出工作树，跑后移回，
  `git status` 只剩这一个未跟踪文件。`npm run test:mutation:fixtures`：`8/8 passed`。
- 注：`--base origin/main` 在本仓选不出任何条目（`git diff --name-only` 给的是仓库根相对路径
  `fe/web/src/...`，manifest 里是 `web/src/...`，`selectedEntries` 永远不匹配 → `selected: 0`）。
  这是既有工具问题，本片不修，改用 CI 同款 `--shard`。

## 8. 评审 R2 修复轮（提交 `686d24a2`）

**G1 oracle 行号错位.** `INV-CARD-226` 引用的 `wave-cards-panel.test.tsx:116,128,137,142,149`
实测分别是空行 / `});` ×4 —— 五条全部没指到测试。改为 `129,141,150,155,162`（G3 改动后的最终行号，
逐行核对确为 `it(` 行）。同时把新的 declaration contract 两条 `register.contract.test.ts:118,133`
一并写进该条 `authoritative_test`——G2 之后，无头声明的双向机器锁在那里。
**顺手核对本片新增/修改的每一条 oracle 引用**：`spec.test.ts:42,48`、`wave-report.test.ts:8,18,24`、
`headless-filter.test.ts:22,48,61,75` 逐行确认全部落在 `it(` 行（`sed -n "${n}p"` 逐条打印比对），
只有 panel 那一条错位。

**G2 根因修：无头 = entry 自己的元数据（不再执行 component）.**

- `builtins/headless-filter.ts` 按 `lifecycle.ts:14-23` 的既有约定
  `declare module '../registry.js' { interface CardEntry { readonly headless?: boolean } }`；
  `spec.ts` / `wave-report.ts` 各自在 `component` 旁写 `headless: true`；
  `partitionWaveCards` 改为 `registry.get(card.type)?.headless === true`。
  `HEADLESS_CARD_TYPES` 与 `isHeadlessType` 删除，`public.ts` 不再导出前者。
- **可选 vs 必填：实证后选可选。** 冻结文件 `public.contract.test.ts` 在 `:32-46` 用
  `function entry(...): CardEntry { return { …没有 headless… } satisfies CardEntry; }`，
  `:123-125` 的 `make(...)` 同样 `satisfies CardEntry`。字段若必填，这两处对象字面量立刻
  typecheck 失败（`satisfies` 会检查必填成员缺失），而该文件 readonly、本片不应改；
  `validateEntry` 那一层还够不着就已经先在 tsc 挂了。故取 `readonly headless?: boolean`，
  缺省 = 有面。
- **contract test 不执行任何 component。** `register.contract.test.ts` 的 headless 段改为：
  按 `BUILTIN_CARD_ORDER` 八项写死的 `HEADLESS_BY_TYPE` 期望表（依据 oracle 的
  `INV-CARD-181` / `INV-CARD-201`，不是从 entry 反读），(a) 断言表的键集恰好等于 tuple
  ——后续 slice 落 entry 时不可能「没人决定过」；(b) 对生产 registry 的每个已注册 entry
  断言 `entry.headless === true` 与表一致；(c) 用一个 4×6、类型名不可能被特判的
  `declared-headless-fixture` 断言 `partitionWaveCards` 读的确实是这个字段而不是类型名。
- **为什么 S3c 带 Hook 的 terminal entry 不会再抛异常**：新测试只读对象属性
  （`entry.headless`、`entry.type`），从不调用 `entry.component`。React 的 Invalid hook call
  只在函数组件于 renderer 之外被调用时抛出；既然没有任何调用点，entry 用不用
  `useSyncExternalStore` / `useEffect`、是不是 class component 都与本 contract 无关。
  旧写法 `entry.component({})` 恰恰是那个调用点。顺带堵上旧启发式的洞：
  `rendersNothing && isOneByOne` 是合取，一个 `() => null` 但 4×6 的无头 entry 漏登记时
  两边同时为 false、元测试仍绿，而它会在 CARDS/grid 里占一个空白槽；现在无头是显式声明，
  漏声明直接被 (b) 抓住。
- **自证（两次变异各自单独跑，跑完各自还原）**：
  - 删掉 `SPEC_CARD_ENTRY` 的 `headless: true` → **6 failed / 15 passed**：
    `register.contract.test.ts` 1 条（`declares headless on exactly the entries that are headless`）、
    `headless-filter.test.ts` 2 条（`binds originalIndex before filtering…`、
    `drops resolved spec and wave-report cards from both branches`）、
    `wave-cards-panel.test.tsx` 3 条（`drops the resolved headless cards…`、
    `renders exactly the surviving cards in the kernel wire order`、`shows the empty state…`）。
  - 给非无头的 `panel-surface-fixture` 加上 `headless: true` → **2 failed / 19 passed**：
    `renders exactly the surviving cards in the kernel wire order`、
    `keeps a card with a surface, so the filter is headless-only and not adapter-only`。
  两个方向都红，且第二次证明的是「误加 = 删卡」这条今天没有别的守卫的路径。

**G3 路由 fixture 撞名 + 名不副实.** 两个都修，取「换名」这条（首选方案）：
类型名 `file-viewer` → `panel-surface-fixture`（不在 `BUILTIN_CARD_ORDER` 里），
kernel kind `file-viewer` → `panel-surface`，标题 `Viewer` → `Surface`；
`component` 从 `() => null` 改为真正渲染 `<div>{\`surface for ${id}\`}</div>`。
注释写明理由：registry 按 type 覆盖且 fixture 在 `bootTestCardRuntime()` 之后注册，
沿用 tuple 内的真实类型名会在 S3c/viewer epic 落地当天静默 shadow 掉真 entry 而毫无信号；
以及「有面的卡」这个 fixture 存在的理由要求它真的有面。
`headless-filter.test.ts` 的 `surface-fixture` 本来就在 tuple 之外，注释里点名对照。

**G4 `CAP-CARD-183` 措辞 + 归属.** 不推翻 skip 决定。skip_reason 改为只陈述今天的事实
（spec entry 声明 `headless: true`、`partitionWaveCards` 据此剔除、fe 里该 accessible name
零消费者、生产取常量 `accessibleName: () => 'Spec harness'`，锚 `spec.ts:45`），
并显式写明「S2 的 grid 尚未写，没有任何机器手段保证它会从 `partitionWaveCards` 的 visible
分支渲染；S2 必须走 visible 分支，否则本条前提重新成立须重开」——不再把未来说成既成事实。
补需求后继者：用户看到的 spec 名字来自 conversation 行（`app/router/public.tsx:181` 的
`conversationNameFrom` 命名路径），所以这是「废弃旧需求」而非「删掉需求」。
归属与同 family 的 181/182 成对改为 `owner_slice: systems/cards/builtins` + `runtime_layer: systems`。

**G5 README.** 新增一条契约描述 `CardEntry.headless?: boolean`（合并方式、为何可选、
为何不靠执行 component、双向机器锁在哪），`partitionWaveCards` 那条改写为读该字段；
导出列举补上 `isSpecHarnessPayload`。README 与 `public.ts` 都 readonly，本次是**新提交**，
因此 `686d24a2` 自带两条新的逐路径 trailer（旧提交的 trailer 不覆盖新提交）。

**mutation manifest.** G2 改了 `partitionWaveCards` 的过滤实现，
`cards-headless-filter-display-index` 的 patch 上下文（原含 `if (isHeadlessType(card.type)) return;`）
已失效；重新生成 hunk（`@@ -73,14 +73,14 @@`，上下文改为新的 `registry.get(...)?.headless` 三行注释 + 判断），
`expected_red` / `selection_paths` 不变。只替换了 manifest 里那一行的 `"patch"` 字符串，
文件其余格式零改动（`git diff --stat` = 1 insertion / 1 deletion）。

### R2 修复轮门禁实际数字（Node v22.22.2，`fe/` 下）

- `npm run typecheck`（`tsc -b`）：exit 0，无诊断。
- `OWNERSHIP_BASE_SHA=origin/main npm run lint`：exit 0。`ownership manifest: 98 entries,
  complete coverage, no overlaps`；readonly trailer 检查对 `686d24a2` 无告警（自证条目仍红：
  fixture `frozen.txt` 负例 RED、approved 正例 GREEN）；test-tier 通过；
  depcruise `✔ no dependency violations found (259 modules, 845 dependencies cruised)`。
- `npm test`：**Test Files 123 passed | 1 skipped (124)；Tests 1333 passed | 1 skipped (1334)**；
  mock-drift exit 0。（总数与 R1 相同：`register.contract.test.ts` 的 headless 段
  R1 是 3 条、R2 也是 3 条（旧的 `classifies…observably is` / `names only types that are
  really registered` / `names only types the order tuple knows about` 被
  `decides headlessness for every type…` / `declares headless on exactly the entries…` /
  `filters on that declaration, not on the type name` 取代），G3 只改 fixture 不增删用例，净 0。）
- `npm run test:browser`：**Test Files 7 passed (7)；Tests 13 passed (13)**。
- `node tools/mutation/run.mjs --shard 2/4`：exit 0。`total: 66`，`ran: 17`
  （shard 过滤为 `index % 4 === 1`；本片条目在数组下标 65，`65 % 4 === 1` 落在本分片），
  **17/17 `verdict.ok = true`**，`cards-headless-filter-display-index` 的 `actual_red`
  与四条 `expected_red` 完全相等。跑前把未跟踪的 `docs/_1091-s1-impl-report.md` 移出工作树
  （runner 要求干净树，且会就地改写源文件），跑后移回；runner 自身的
  `mutation runner left worktree dirty` 检查也通过。
- `npm run test:mutation:fixtures`：`mutation fixture e2e: 8/8 passed`。
- 结束状态：`git status --porcelain` 只有 `?? docs/_1091-s1-impl-report.md` 一条。

## 9. 评审 R3 修复轮（提交 `cac78b39`）

R3 双通道（Codex + 独立 subagent，互不可见）**均无 BLOCKER**，剩三条小项，本提交收口。
本节由 orchestrator 补写：修复 agent 在提交之后、跑 lint 之前被看门狗判定停滞，
未来得及写本节；下述数字全部由 orchestrator 亲自复跑得到，非 agent 自述。

### H1 —— 无头声明在生产注册路径上编译期必填

`CardEntry.headless` 在接口上仍是可选（缺省＝有面，fail-open）。
**不能**在 `registry.ts` 的 `validateEntry` 里强制：`public.test.ts:29-30,:37-38` 与
`public.contract.test.ts:32-46,:123-125` 两个**冻结**文件都构造不含该字段的 entry
且都真的 `register()`，加守卫会把两个冻结文件打红；该层也没有信号能区分 built-in 与契约测试 entry。

改为只约束生产注册路径：`builtins/register.ts` 新增 helper，参数类型为
`CardEntry<Card> & { readonly headless: boolean }`；两个 registrar 都经它注册。
可选属性不可赋值给必填属性，故漏写即 typecheck 错误。冻结文件不经过该路径，不受影响。

**orchestrator 自证**：删掉 `SPEC_CARD_ENTRY` 的 `headless: true` 后 `tsc -b` 报

    register.ts(69,49): error TS2345: Argument of type '...' is not assignable to
    parameter of type 'CardEntry<Readonly<{ type: "spec"; id: string; }>>
    & { readonly headless: boolean; }'

还原后 exit 0，工作树与提交逐字节一致。

注意分层：helper 只挡「漏写」，挡不住「写反」（有面的卡声明 `true`）——后者由
`register.contract.test.ts` 的 `HEADLESS_BY_TYPE` 表与 set-equality 兜住。两层各挡一半，注释已写明。

### H2a / H2b —— 两处「声称了但没证明」的注释

- `wave-cards-panel.test.tsx`：fixture 注释原称「它真的渲染所以有面」。
  S1 的 wave 路由不挂载任何 card component（路由把 slot 映射回 `slot.wire`，
  面板渲染 wire 的 `title ?? kind`），该 JSX 是死代码——证据来自测试自身：
  行顺序断言读到的是 `'Surface'` 而非 `surface for card-surface`。注释已改为陈述真实依据。
  改名那一半（`panel-surface-fixture` 不在 `BUILTIN_CARD_ORDER` 内，避免遮蔽 S3c 真 entry）保留。
- `headless-filter.ts`：原注释称「`resolve` returned this card, so the entry exists」——
  registry 不保证这一点（`resolve` 直接返回 `fromKernel` 结果，从不校验 `result.type === entry.type`）。
  **未改动 `resolve` 语义**（核心解析路径已经三轮评审确认稳定，收益不抵风险）；
  注释改为陈述真实依据（生产 entry 的窄泛型标注把 `type: Card['type']` 与 `fromKernel` 绑死），
  并在 `register.contract.test.ts` 新增不变量锁住。

**orchestrator 自证**：把 spec 的 `fromKernel` 强转成返回 `type: 'wave-report'` →
`[INV-CARD-180] leaves the shared codex kind…` 与新增的
`[INV-CARD-226] resolves each probe back to the entry that owns it` **两条转红**；还原后全绿。

### 门禁（orchestrator 复跑，非 agent 自述）

- `npm run typecheck`：exit 0，无诊断。
- `OWNERSHIP_BASE_SHA=origin/main npm run lint`：exit 0；`ownership manifest: 98 entries, complete coverage, no overlaps`；
  depcruise `no dependency violations found (259 modules, 849 dependencies cruised)`。
- `npm test`：**Test Files 123 passed | 1 skipped (124)；Tests 1335 passed | 1 skipped (1336)**（较 R2 净增 2，即新增的两条不变量）。
- `npm run test:browser`：**Test Files 7 passed (7)；Tests 13 passed (13)**。
- `node tools/mutation/run.mjs --shard 2/4`：exit 0，`ran: 17 / total: 66`，**17/17 `verdict.ok = true`**，
  本片条目 `expected_red == actual_red`。跑前移出未跟踪报告、跑后移回。
- oracle 六条引用（`wave-cards-panel.test.tsx:136,148,157,162,169` 与
  `register.contract.test.ts:182`）逐行核对，全部落在真实 `it(` 行。
- 本提交只碰一个冻结文件 `systems/cards/README.md`，自带对应 OWNERSHIP-CHANGE trailer；无 `Co-Authored-By`。

## 10. 移交后续切片的三条（不在 S1 范围）

1. **S2 的 grid 必须从 `partitionWaveCards` 的 visible 分支渲染**，不得直接遍历原始 `cards`；
   `CAP-CARD-183` 的 skip_reason 明确把这条写成 S2 的义务，违反则该条目前提复活、必须重开。
2. **S3c / file-viewer 落地时必须处理 `wave-cards-panel.test.tsx` 的 `panel-surface-fixture`**，
   换成真实 entry；当前它只是「S2 grid 将来真会挂载的有面 entry」的替身。
3. **oracle 的 `authoritative_test` 引用没有任何机器校验指向真实测试行**
   （`tools/oracle/validator.ts:195-219` 只校验路径存在与行号越界；`source` 有 anchor 校验，`authoritative_test` 没有）。
   本片因此两次出现「引用指到空行 / `});`」且门禁全绿。修法约 5 行：被引文件名匹配 `*.test.*` 时
   要求该行匹配 `/^\s*it[.(]/`。属 oracle 工具链，另开 issue。
4. **S4a 注意**：若真实 codex entry 采用 `claim: {mode:'exact', kind:'codex'}`，
   `resolve` 会在插入顺序全扫**之前**先试 exact claim，tuple 里 codex→spec 的先后就成了语义死字，
   README「顺序是 business semantics」会退化为纯文档。

## 11. 评审 R4 修复（提交 a5b20313）

### R4 BLOCKER：`registerBuiltin` 不是真正的生产注册门

R3 引入的 helper 只约束了「今天手写的这两行」。registrars 的值类型是
`(target: CardRegistry) => void`，后续 slice 写
`terminal: (target) => { target.register(TERMINAL_CARD_ENTRY); }` 照样 typecheck 通过；
而 `register.ts` 的注释把它描述成 production door——**守卫可被静默绕过 + 注释声称不可绕过**，
正是本仓禁止的假门形状。

### 改法：registrar 从闭包改成名义类型，`registry.register` 只剩一个调用点

`fe/web/src/systems/cards/builtins/register.ts`：

```ts
class BuiltinRegistrar {
  readonly #register: (target: CardRegistry) => void;
  private constructor(register: (target: CardRegistry) => void) { this.#register = register; }
  static of<Card extends RegisteredCard>(
    entry: CardEntry<Card> & { readonly headless: boolean },
  ): BuiltinRegistrar {
    return new BuiltinRegistrar((target) => { target.register(entry); });
  }
  run(target: CardRegistry): void { this.#register(target); }
}

const registrars: Partial<Record<BuiltinCardType, BuiltinRegistrar>> = {
  spec: BuiltinRegistrar.of(SPEC_CARD_ENTRY),
  'wave-report': BuiltinRegistrar.of(WAVE_REPORT_CARD_ENTRY),
};
for (const type of BUILTIN_CARD_ORDER) registrars[type]?.run(registry);
```

关键点：

- `#register` 私有字段让 `BuiltinRegistrar` **名义化**——TS 里任何对象字面量 / 箭头函数 /
  `Object.assign` 结果都不可赋值给它；构造器 `private`，所以 `of` 是唯一产出表达式。
- 因此 map 的每个 slot 只能由 `of(entry)` 填，`of` 的参数把 `headless` 收成必填。
- **泛型没有退化**：entry 在 `of` 内被自身窄 `Card` 捕获，`registry.register` 拿到的仍是
  per-entry 的窄类型，不是整个 `RegisteredCard` 联合（见下方探针 E）。
  这也是没有采用「map 直接持 `CardEntry<RegisteredCard>` entry」方案的原因：`component`
  的参数位是逆变的，`CardEntry<SpecCard>` 本就不可赋值给 `CardEntry<RegisteredCard>`。

### 自证 1：绕过写法必须是 typecheck 错误（`tsc -b`，逐条贴原文，还原后 exit 0）

探针 A —— 绕过工厂直接 `target.register(...)`：

```
web/src/systems/cards/builtins/register.ts(99,5): error TS2322: Type '(target: any) => void' is not assignable to type 'BuiltinRegistrar'.
web/src/systems/cards/builtins/register.ts(99,16): error TS7006: Parameter 'target' implicitly has an 'any' type.
```

探针 B —— 经工厂但 entry 没有 `headless`：

```
web/src/systems/cards/builtins/register.ts(101,32): error TS2345: Argument of type '{ type: "spec"; component: () => null; defaultSize: { w: number; h: number; minW: number; minH: number; }; title: () => string; accessibleName: () => string; create: { mode: "kernel-minted-only"; }; }' is not assignable to parameter of type 'CardEntry<RegisteredCard> & { readonly headless: boolean; }'.
  Property 'headless' is missing in type '{ … }' but required in type '{ readonly headless: boolean; }'.
```

探针 C —— 用对象字面量 `{ run: … }` 伪造 registrar：

```
web/src/systems/cards/builtins/register.ts(110,5): error TS2741: Property '#register' is missing in type '{ run: (target: CardRegistry) => void; }' but required in type 'BuiltinRegistrar'.
```

探针 D —— 用 `Object.assign` 伪造：

```
web/src/systems/cards/builtins/register.ts(112,5): error TS2322: Type 'object & { run: (target: CardRegistry) => void; }' is not assignable to type 'BuiltinRegistrar | undefined'.
  Property '#register' is missing in type '{ run: (target: CardRegistry) => void; }' but required in type 'BuiltinRegistrar'.
```

探针 E —— 证明泛型仍是逐 entry 的窄类型（entry 的 `fromKernel` 改成返回 `'wave-report'`）：

```
web/src/systems/cards/builtins/register.ts(98,35): error TS2345: Argument of type '{ fromKernel: () => { readonly type: "wave-report"; … }; type: "spec"; … }' is not assignable to parameter of type 'CardEntry<{ readonly type: "wave-report"; readonly id: "x"; }> & { readonly headless: boolean; }'.
      Type '"spec"' is not assignable to type '"wave-report"'.
```

若泛型退化成整个联合，探针 E 会通过；它红说明 `Card` 是逐 entry 推断的。

**挡不住的一种写法（如实说明）**：在 `register.ts` 内部显式写
`terminal: (((target) => { target.register(E); }) as unknown as BuiltinRegistrar)`。
没有任何语言内 brand 能挡住 `as unknown as`。它是对本文件的可见、可评审的编辑，不是沉默的遗漏；
且下面第二条运行期断言会在「用它塞进一个漏声明 `headless` 的 entry」时照样红。
早期尝试过 `unique symbol` brand + `Object.assign` 的方案，那一版**探针 D 能编译通过**
（brand 符号在本文件作用域内可直接伪造），因此换成了私有字段的名义类型。

### 自证 2：漏声明独立于决策表也能红

`register.contract.test.ts` 的生产 registry 逐 entry 断言新增一条，与 `HEADLESS_BY_TYPE` 无关：

```ts
expect(
  entry.headless,
  `${entry.type} must state its headlessness explicitly; absent is the fail-open default`,
).toBeTypeOf('boolean');
```

相关性错误注入（同时改两处、同一方向）：删掉 `SPEC_CARD_ENTRY` 的 `headless: true`
**并且**把 `HEADLESS_BY_TYPE.spec` 改成 `false`。结果：

```
AssertionError: spec must state its headlessness explicitly; absent is the fail-open default: expected undefined to be type of 'boolean'
 Test Files  1 failed (1)
      Tests  1 failed | 9 passed (10)
```

只有新断言红——旧的 `entry.headless === true` 对比 `false` 依旧绿，正好证明没有这条时两边全绿。
还原后 10/10 绿。

### 注释与文档按实际强度重写

- `register.ts` 的 `BuiltinRegistrar` doc 明确分三段：**类型挡住什么**（漏 `headless`、跳过工厂）、
  **类型挡不住什么**（本文件内显式 `as unknown as`）、**测试挡住什么**（写错方向 + `typeof` 兜底）。
- `headless-filter.ts` 的 `headless` 字段注释同步为「工厂 + 名义类型，两种绕法都是 typecheck 错误，
  显式断言不在覆盖内」。
- `register.contract.test.ts` 的段注释原写「两种错误在类型层都是沉默的」——改动后漏写那半已经不沉默，
  改为如实描述并说明为什么仍要一条运行期断言。
- `systems/cards/README.md` 第 12 条同步（该文件冻结，本提交自带对应 OWNERSHIP-CHANGE trailer）。

### 随改

`INV-CARD-226` 的 `authoritative_test` 里 `register.contract.test.ts:118,133,182`
因本轮增行改为 `126,149,198`，逐行核对全部落在真实 `it(` 行。

### 门禁（实际数字）

- `npm run typecheck`：exit 0。
- `OWNERSHIP_BASE_SHA=origin/main npm run lint`：exit 0（eslint / ownership 98 entries /
  stylelint / depcruise **259 modules, 849 dependencies, no violations**）；
  提交后单独复跑 `lint:js`，readonly trailer 检查在含 README 改动的提交上仍 exit 0。
- `npm test`：**Test Files 123 passed | 1 skipped (124)；Tests 1335 passed | 1 skipped (1336)**，
  mock-drift exit 0。
- `npm run test:browser`：**Test Files 7 passed (7)；Tests 13 passed (13)**。
- `node tools/mutation/run.mjs --shard 2/4`：exit 0，`selected: 66 / ran: 17 / total: 66`，
  **17/17 `verdict.ok = true`，0 条 false**。跑前移出未跟踪报告、跑后移回，
  `git status` 只剩 `?? docs/_1091-s1-impl-report.md`。
- 本提交碰的唯一冻结文件是 `systems/cards/README.md`，自带 OWNERSHIP-CHANGE trailer；无 `Co-Authored-By`。

---

## 评审轮 5（收口 + 变基）

### 背景

「注释宣称的保证强于实现」已连续三轮成为评审发现，因此本轮把两处过度声称改到可证事实，
而不是带着它上线。**纯注释/文档改动，`register.ts` 逻辑一行未动。**

### (a) `BuiltinRegistrar.of` 不是可证的唯一产出点

先实测再改字：在 `register.ts` 里临时插入探针并跑 `tsc -b` 与 vitest，三条反例全部证实
**无需任何断言**即可填 registrar 槽位：

| 写法 | 为何能过类型 | 启动时行为（实测） |
| --- | --- | --- |
| `Object.create(BuiltinRegistrar.prototype)` | `lib.es5.d.ts` 声明为 `any` | 抛 `TypeError: Cannot read private member #register from an object whose class did not declare it` |
| `structuredClone(BuiltinRegistrar.of(entry))` | 声明 `T => T`，静态类型保留 | 原型未随克隆保留（实测 `proto === BuiltinRegistrar.prototype` 为 `false`），抛 `TypeError: run is not a function` |
| `Object.assign(Object.create(BuiltinRegistrar.prototype), { run })` | `any & {...}` 塌成 `any` | **跑通并真的注册**（探针里 `registered=["spec"]`） |

负对照同样实测为红，证明「结构性写法被挡住」这半仍然成立：

- 裸箭头函数 → `TS2322: Type '(t: CardRegistry) => void' is not assignable to type 'BuiltinRegistrar'`
- 带 `run` 的对象字面量 → `TS2741: Property '#register' is missing`
- `Object.assign({}, { run })` → `TS2741: Property '#register' is missing`
- `class Sub extends BuiltinRegistrar {}` → `TS2675: Cannot extend a class ... constructor is marked as private`

新措辞据此分层：**结构性值被 `#private` 名义类型挡住；显式断言与 `any`/恒等 `T` 的运行时造对象 API 挡不住**；
其中 `Object.create` 与 `structuredClone` 两条启动即抛、不会静默上线，
`as unknown` 断言与 `Object.assign` over prototype 两条能跑通，
兜底的是 `register.contract.test.ts` 对生产 registry 的逐 entry 运行时断言
`typeof entry.headless === 'boolean'`。

### (b) 泛型不加宽只是今天两个常量的性质

实测：`declare const FUTURE_ENTRY: CardEntry & { readonly headless: boolean }` 传给
`BuiltinRegistrar.of` **通过 typecheck**（`CardEntry` 默认泛型即 `RegisteredCard`）。
同时验证今天两个 entry 确实是窄的——条件类型探针取 `'NARROW'` 通过、改成 `'WIDE'` 报
`TS2322`。措辞因此收窄为「因为两个 entry 都是 `satisfies CardEntry<SpecCard>` /
`CardEntry<WaveReportCard>` 的字面量常量，不是工厂的保证」。

### 变基到 `origin/main` = `96826e04`

`git fetch origin main` 后 `git rebase origin/main`，7 个 commit **零冲突**。

机械核验（不靠「看起来成功了」）：

- `git merge-base --is-ancestor origin/main HEAD` → 退出 0。
- 变基前 `git diff 6e35e79e..HEAD` 与变基后 `git diff origin/main..HEAD`
  用 `cmp` 比对 **逐字节相同**（连 hunk 偏移都没动），`--stat` 亦逐行相同：
  26 files changed, 1184 insertions(+), 141 deletions(-)。
- 语义冲突检查：`96826e04`（#1098 切片 3）在 `fe/` 下只碰
  `core/api/generated/{openapi.json,wire.ts}`、`mock/generated/operations.ts`、
  `tools/dev-mock/{check-contract.mjs,server.mjs}` —— 与本切片文件集**完全不相交**，
  这也正是 hunk 偏移为零的原因。逐项确认：
  - **wave 路由**：#1098 无任何 `web/src` 改动，不碰 `app/router/public.tsx`。
  - **card registry**：#1098 是后端懒铸卡 + 生成类型，未新增 `CardDataMap` 增强，
    也未声明任何 `claim`；本切片 spec / wave-report 两个 entry 均刻意不带 `claim`，无 kind 争用。
  - **新增 Event**：无。`wire.ts` 只 +42 行一个 `CoveConversationSummary` 类型，
    因此不触发 zod / invalidationPolicies / goldens 计数联动；`test:wire` 与
    `test:mock-drift` 实跑 exit 0 佐证。
  - **inventory / mutation manifest**：#1098 未碰 `module-file-inventory.yaml` 与
    `tools/mutation/manifest.json`，本切片对两者的改动无对手。

### 门禁（本轮实际数字，全部在变基后的 HEAD 上跑）

- `npm run typecheck`：exit 0。
- `OWNERSHIP_BASE_SHA=origin/main npm run lint`：exit 0，**提交之后**才跑，
  readonly trailer 检查放行本轮 README 改动；ownership manifest **98 entries**，
  depcruise **259 modules / 849 dependencies, no violations**。
- `npm test`：**Test Files 123 passed | 1 skipped (124)；Tests 1335 passed | 1 skipped (1336)**。
- `npm run test:browser`：**Test Files 7 passed (7)；Tests 13 passed (13)**。
- `node tools/mutation/run.mjs --shard 2/4`：exit 0，`shard 2/4`，
  `selected: 66 / ran: 17 / total: 66`，**17/17 `verdict.ok = true`，0 条 false**
  （含 `cards-headless-filter-display-index`：4 条 expected_red 与 actual_red 完全一致）。
  跑前把未跟踪的本报告移出工作树、`git status --porcelain` 为空后才启动，跑完移回。

收尾 `git status` 只剩 `?? docs/_1091-s1-impl-report.md`。本轮提交
`docs(#1091) S1 评审轮5` 自带 `OWNERSHIP-CHANGE: fe/web/src/systems/cards/README.md` trailer，
无 `Co-Authored-By`。

## 修复轮 7（R6 双路并归收尾）

裁决后只做三件事，均不改行为；Codex 报的 plain_chat BLOCKER 不采纳（S1 正确行为），转 S4a 移交项。

- **G-A `builtins/headless-filter.ts`**：`headless` 的 JSDoc 是「registrar 门禁」的第三份副本，
  `f4156bb1` 只更新了 `register.ts` 与 `README.md`，它仍写着「只有显式 `as unknown as` 不被覆盖」。
  改成：类型挡住的是**漏写**与**普通结构性替身**；「这道门不严密，也不声称严密」——显式断言之外还有一个
  **开放集合**的运行时造对象 API 无需断言即可填槽，其中若干条**能跑通**而非启动即抛；细节与真正承重的
  逐 entry 运行时断言指向 `register.ts` 的 `BuiltinRegistrar` 注释，不再复述。
- **G-B `builtins/register.ts`**：删掉「All five escapes」的计数，把逃逸列表明确写成**开放的举例**
  （"a sample, not an enumeration, and no count of it means anything"），并补两条实测：
  `Object.setPrototypeOf({ run(){} }, BuiltinRegistrar.prototype)` 无断言即 typecheck 通过且**能跑通**
  （自有 `run` 遮蔽原型方法，`#register` 永不被读）、`Reflect.construct(BuiltinRegistrar, [register])`
  返回 `any` 且 `private constructor` 运行时已擦除，造出**真实例**（`instanceof` 成立）。
  据此删掉「三条里两条启动即抛、不会静默上线」这句安慰性结论，改为
  "So \"it would throw at boot\" is not a property of this gate"。承重的那句保持不变并点明它是**唯一**
  仍然成立的保证：每条逃逸都是本文件里可见、可评审的改动，绝不是静默省略。
  README 同一段（readonly，带 trailer）作同向收口：加「开放集合 / 至少 / 举例不是穷举」并补上两条新逃逸。
- **G-C 编译期负向固定（`register.ts`）**：把 map 类型提名为 `BuiltinRegistrarMap`，加
  `AssertTrue<SlotRejectsStructuralRegistrars>`（导出仅为规避 TS6196 / `no-unused-vars`，无人 import）。
  两条 arm 都必要：放宽成裸箭头时 `?.run()` 本身也会红，但放宽成 `{ run(t): void }` 在加固定之前
  **完全静默**。`@ts-expect-error` 未采用——等价类型层断言即可，且不触碰 ts-comment 规则。

**自证（拆门→变红→还原）**，`npx tsc -b`：

- 基线：exit 0。
- 放宽为 `BuiltinRegistrar | ((t: CardRegistry) => void)`：
  `register.ts(155,61): error TS2344: Type 'false' does not satisfy the constraint 'true'.`
  （附带 `(175,23): error TS2339: Property 'run' does not exist on type ...`），exit 2。
- 放宽为 `BuiltinRegistrar | { run(t: CardRegistry): void }`：**只有**
  `register.ts(155,61): error TS2344: Type 'false' does not satisfy the constraint 'true'.`，exit 2。
- 还原：exit 0。

逃逸行为的运行时自证（等价 JS 复现 `#register` 私有字段语义）：`Object.setPrototypeOf` 与
`Reflect.construct` 两条均成功 `register(...)`（后者 `instanceof` 为 true），
`Object.create` 抛 `Cannot read private member #register from an object whose class did not declare it`，
`structuredClone` 抛 `run is not a function` —— 与注释逐条一致。

## 修复轮 8：`architecture.test.ts` fixture 用例显式 timeout（CI 解阻塞）

**症状**：CI `fe-mutation 3/6` 在本分支三轮红两轮，判 `over-red`，多红的永远同一条
`architecture fixtures markdown-micromark-attributes-import: accepts the positive and rejects the negative fixture`。

**事实链**：

1. `fe/tools/mutation/runner.ts:154` —— 只要改了 `tools/mutation/` 下任何文件就返回全部 66 条。
   本片新增了 manifest 条目，于是这条平时不跑的条目被拉进本片。
2. `fe/tools/mutation/run.mjs:74` —— runner 对每个条目跑一次**完整套件**（`npx vitest run`，无路径过滤），
   `actual_red` 是全套件失败集；因此任何位置的间歇失败都会记成该条目的 over-red。
   （`selection_paths` 只是元数据，不参与过滤。）
3. `fe/tools/architecture/architecture.test.ts` 的 fixture 用例此前**没有显式 timeout**，吃 vitest 默认 5000ms。
   本机空闲实测：`markdown-micromark-attributes-import` **2090ms**（占预算 42%），
   同组兄弟 `markdown-micromark-import` **400ms**、`markdown-micromark-template-import` **284ms** ——
   差异来自它是第一个 micromark case，独扛 `new ESLint()` + flat config 解析的冷启动
   （`cruise()` 的每个 ESLint 分支都新建实例）。
4. **对照实验**：main 的同一分片（`31930728575` 的 mutation-report-3）跑的是同样 11 条、包含该条目、**全过**；
   本地跑 shard 3/6 也 **11/11 全过**。差别是本片 +34 条测试（含 5 次完整 router 渲染）抬高了并行负载。

**结论**：既有脆弱性（空闲即用掉 42% 预算），本片只是把它推过线；**不是断言失败**。

**改动**：`fe/tools/architecture/architecture.test.ts` 的 fixture 循环 `it(...)` 加第三参
`fixtureCaseTimeoutMs = 30_000`，并把上述实测数字与「每 case 新建 ESLint 实例 + 解析 flat config」的
原因写进注释。该 `it` 覆盖循环内**全部** case（含 `dup-*`、`core-markdown-node-import`、
`test-module-runtime-state-exemption`），一处即可。**没有**加 vitest retry，**没有**跳过或放宽任何断言，
只放宽时钟。`fe/tools/architecture` 在 `fe/module-file-inventory.yaml:24` 为 `readonly: false`，无需 trailer。

**自证**：`npx vitest run --project platform-independent tools/architecture/architecture.test.ts --reporter=verbose`
→ **67 passed (67)**，该条用例 **2045ms**；30000ms 对实测最差 2090ms 有约 **14x** 余量（默认 5000ms 仅 2.4x）。

**后续（本轮不做）**：真正的根因是每个 case 都新建 ESLint 实例（冷启动 2090ms vs 复用后约 300ms）；
复用单个 ESLint 实例可大幅提速整个文件，属 oracle/architecture 工具链，另开 issue。

---

## Rebase 到 `a541329e`（#1098 切片 4）+ plain_chat 交互重查

基线从 `57115651` 移到 `a541329e`（`feat(#1098) 切片 4：cove 对话前端接线`）。9 条提交全部重放，
新 HEAD `eecd2f56`。

### 1. 冲突逐条

只有 **1 处**真冲突，在重放第 1 条（`3556700c`）的 `fe/web/src/app/router/public.tsx`：

- **冲突点**：双方都在同一个 `/**` 之后插入内容。切片 4 在 `SpecConversationScope` 之下新增了
  `ConversationPanelSource` / `OpenTarget` / `ConversationDraft` / `DraftEdit` 四个类型及其文档；
  本片在同一位置新增了 `CardRuntime` 的文档 + 类型。git 只能看到"同一个 `/**` 开头，两种续写"。
- **解法**：保留双方——先切片 4 的四个类型（含完整注释），再补一个独立的 `/**` 开头接上本片的
  `CardRuntime` 文档与 `export type CardRuntime`。没有任何一侧被丢弃或改写。
- 其余 7 处改动（`AppRouterDeps.cards`、`createRouteTree` 解构、`waveRoute` 的
  `cardRegistry={cards.registry}`、`WaveRoute`/`WaveRouteBody` 签名、`panelCards` memo、
  `cards={panelCards}`）由 git 自动合并，落点正确。

`public.contract.test.tsx`、`spec-conversation.test.tsx`、`fe/tools/mutation/manifest.json`
**自动合并无冲突**（第 1、2 条提交各自 auto-merge）。后 7 条提交（评审轮次）零冲突。

**两处 `isSpecHarnessPayload` 谓词**：第 1 条提交本来就还是内联谓词（`git show 3556700c -- public.tsx`
里没有 `isSpecHarnessPayload`），是后续评审轮才换的共享函数；rebase 后终态两处都是共享函数，
`grep -rn spec_harness fe/web/src` 生产代码零手抄副本（只剩 `builtins/spec.ts` 这一处定义 + 测试夹具）。

### 2. 并集机器校验

**方向 A —— 本片 28 个文件的改动一条不少**

- `git diff origin/main..HEAD --name-only` 与 `git diff 57115651..e09d2618 --name-only`
  逐行 `diff` → **完全一致，28 个文件**。
- 对 28 个文件逐个做 diff-of-diff（去掉 `diff --git` 头两行后比较）：**26 个字节级完全相同**，
  2 个有差异，逐条说明：
  - `fe/tools/mutation/manifest.json`：唯一差异是 hunk 头
    `@@ -1039,5 +1039,24 @@` → `@@ -1059,5 +1059,24 @@`。原因：切片 4 之前的 main
    把 3 条已有条目改长了 20 行，追加点后移。**内容零差异**，属重放位移，非丢失。
  - `fe/web/src/app/router/public.tsx`：差异全部是 hunk 头行号/上下文标签
    （`@@ -277,…` → `@@ -411,…` 等 7 处），外加 import hunk 里多出一行**上下文**
    `import { mintIdempotencyKey } from './idempotency-key.ts';`（切片 4 的行，作为 context 出现）。
    `+`/`-` 行**逐字节相同**。属冲突解决/位移，非丢失。

**方向 B —— 切片 4 的改动完整在树里**

- 由方向 A 反推即成立：`origin/main..HEAD` 的逐文件 diff 与本片原始 diff 内容全等，
  说明 HEAD = `a541329e` + 恰好本片那些增删，`a541329e` 里任何未被本片碰过的行都原样保留。
- 交集 4 文件里 `public.tsx` 单独复核：切片 4 的
  `ConversationPanelSource`/`OpenTarget`/`ConversationDraft`/`DraftEdit`、
  `'rows'` intent、`kind:'elsewhere'|'card'` 的 `useConversationPanel` 调用、
  `coveConversationCardId`/`coveConversationsQueryOptions` 接线全部在。
- `git diff a541329e..HEAD -- fe/tools/mutation/manifest.json` 是**纯追加**：只多出本片
  `cards-headless-filter-display-index` 一条。
- manifest 条目集合校验（按 `mutation_id`）：`57115651` 65 条，`a541329e` 65 条
  （切片 4 **没有**新增条目，只改了 3 条已有条目的正文），`e09d2618` 66 条，HEAD **66** 条；
  `HEAD == main ∪ 本片新增`，缺失 0、多出 0。**双方条目都在**。
- 全树 `grep -rn '<<<<<<<|>>>>>>>' fe/web/src fe/tools` → 无残留标记。

### 3. plain_chat 交互重查（R6 BLOCKER 复审）

**结论：不阻断 S1。**理由不再依赖"fe 侧没有 cove 通路"（该前提确已失效），而是三条独立的事实。

**(1) plain_chat 卡走不到任何产品内的 `partitionWaveCards` 调用点。**
`partitionWaveCards` 只有一个消费者：`fe/web/src/app/router/public.tsx:1415`，在 `WaveRouteBody` 内，
只在 `/wave/$waveId` 渲染时执行。plain_chat 卡挂在 cove 的隐藏 chat wave 上
（`crates/calm-server/src/routes/cove_conversations.rs:5`、`routes/waves.rs:819-886`，
`purpose = 'cove-chat'`）。能产生 wave id 并 `go({name:'wave'})` 的产品入口有 6 个，
其中 4 个的 id 来自被服务端过滤掉 chat wave 的列表
（`crates/calm-server/src/routes/waves.rs:382-387` `user_visible_wave`，用于
`list_waves_by_cove:368` 与 `list_waves_window:455`）：
`public.tsx:1134`（Today）、`public.tsx:1282`（CovePage）、
`fe/web/src/app/shell/sidebar.tsx:403,474`（侧栏），外加 `public.tsx:1230`（新建 wave 的 POST 响应，
用户建的 wave 永远没有 purpose）。
剩下两个理论入口都到不了 chat wave：
- **backlinks**（`public.tsx:1464` ← `src_wave_id`，服务端 `routes/waves.rs:1891` 不过滤 purpose）：
  backlink 源必须是某个 wave 的 `wave-report` 卡正文里的 `neige://wave/<id>` 链接。
  chat wave 的 report 卡永远是空的 —— plain_chat 线程以 `ThreadConfig::NoMcp` 启动
  （`crates/calm-server/tests/spec_harness_adapters.rs:737-780`），agent 拿不到任何 neige 工具，
  写不了 report block；UI 也没有任何入口能打开 chat wave 去手写。空 report ⇒ 永不成为 backlink 源。
- **别的 wave 的 report 里引用 chat wave id**：agent 侧没有任何列 wave 的工具或路由能吐出 chat wave id
  （`waves_by_cove` 的非路由调用点只有 `report_backlinks.rs:142`、`routes/coves.rs:317` 删除 cove、
  `cove_conversations.rs:541`，都不面向 agent）。
剩下的只有**手敲/粘贴 URL**，那不是产品路径。

**(2) cove 对话列表走服务端 `'rows'`，不读 wave detail 的 cards。**
`public.tsx:1179` 用 `coveConversationsQueryOptions`（`app/providers/queries.ts:116-124`，
`GET /api/coves/{id}/conversations`），`public.tsx:1186-1199` 以 `kind:'rows'` 喂 panel。
`ConversationListIntent` 的注释（`public.tsx:95-101`）明说 `'rows'` 时不查 registry。
切片 4 还专门装了两道闸：`public.tsx:214-233`（服务端列出的行**永不** `registry.remember`，
注释直接点名"否则 Today 会把用户带进隐藏 wave，这一道就是全部防御"）与
`public.tsx:970-979`（`'rows'` 路由打开行只开抽屉，不导航）。

**(3) 没有新界面把 plain_chat 卡当普通卡渲染 —— 且 S1 对它零改变。**
今天注册的 built-in 只有 `spec` 和 `wave-report`（`systems/cards/builtins/register.ts:170-173`），
没有 `codex` adapter，所以 plain_chat 卡 `registry.resolve` 返回 null，落 `unknown` 分支
（`builtins/headless-filter.ts:91-93`）。而 CARDS 面板就是一个标题清单
（`features/wave/page/public.tsx:148-158`）。**S1 之前** `cards={cards}` 传的是全量 wire，
plain_chat 卡本来就会作为一行出现；**S1 之后** `cards={panelCards}` = visible+unknown 按
`originalIndex` 复原，plain_chat 仍在 unknown 里，**同一行、同一位置**。S1 只**移除**了 spec 与
wave-report 两行。也就是说：即使有人手敲 URL 进了 chat wave，看到的东西 S1 前后完全一致 ——
本片没有为 plain_chat 增加任何曝光面。

**(4) `isSpecHarnessPayload` 在新结构下仍只命中真 spec。**
定义在 `systems/cards/builtins/spec.ts:25-27`，判据是 `payload.spec_harness === true`。
plain_chat 卡的 payload 是 `{"schemaVersion":1,"harness_profile":"plain_chat"}`
（`crates/calm-server/src/operation/spec_harness_start_adapter.rs:490`），**没有** `spec_harness` 键 ⇒ false。
两者互斥还有服务端背书：chat wave 上的 Spec 角色被显式 403
（`crates/calm-server/src/routes/cards.rs:1252-1257`），
且 cove 会话列表的判据是 `harness_profile = 'plain_chat'`
（`crates/calm-server/src/routes/cove_conversations.rs:601`），与 `spec_harness` 正交。
切片 4 重构后的两个调用点 `public.tsx:1335`（`requestedCard`）与 `public.tsx:1382`（`specCard`）
仍是共享函数，未退回手抄谓词。

**留给 orchestrator 的一条观察（非 S1 阻断项，属 #1098）**：
`GET /api/waves/{id}` 不做 `user_visible_wave` 过滤，且 wave detail wire 不带 `purpose`
（`fe/core/domain/wave.ts:22-33`），所以前端即使想拦也拦不住 —— 手敲 chat wave 的 URL 能渲染出
标题为 "Cove chat" 的 wave 页。这是 #1098 的边界，S1 前后行为一致。

## 附：CI `fe-mutation` 超时预算（PR #1114 解封）

`fe-mutation` 是 6 路分片矩阵；每片把自己的 manifest 条目交给 `tools/mutation/run.mjs`，
而该 runner 对**每一条** mutation 条目都跑一遍**完整** vitest 套件（裸 `npx vitest run`；
manifest 里的 `selection_paths` 只是元数据，不做筛选）。最差的 shard 3 有 11 条条目
⇒ 11 次全量套件。因此该 job 的耗时随**套件总规模**增长，与本 PR 改动大小无关。

实测（shard 3/6）：

| 来源 | 耗时 |
| --- | --- |
| 本分支最新一次运行 | 15m16s —— 被 15 分钟 job timeout **CANCELLED** |
| origin/main 近期 | 12m58s |
| origin/main 近期 | 13m06s |
| origin/main 近期 | 14m17s |
| origin/main 近期 | 14m50s |

即：main 上早已贴着 15m 天花板（最近一次只差 10 秒），这是**共享的、既有的**预算问题，
不是本切片引入的。近期两处套件增长（#1098 切片 4，已在 main；以及本切片）把 shard 3 顶过线。

处置：`.github/workflows/ci.yml` 中 `fe-mutation` 的 `timeout-minutes` 由 15 提到 25，
并在原地留下带实测数字的注释。分片数、runner、任何测试、其他 job 的 timeout 均未改动。

更耐久的修法（**本 PR 不做**，另开 issue）：把分片数从 6 提到 8 —— 单片成本随套件增长，
抬 timeout 只是买时间，降低每片条目数才是压住斜率的办法。
