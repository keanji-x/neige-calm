# `_fe-architecture.md` 独立 review — 第二轮（subagent 通道）

日期：2026-08-02 ｜ 只读。本轮实读：修订版全文、`docs/oracle/*.yaml` 全量（脚本统计 1126 条）、
`owner-aliases.yaml`、`SCHEMA.md`、`api/events.ts` 全文、`app/eventBridge.tsx:160-240`、
`eventBridge.test.tsx:165-192`、`pages/report-outline.ts`、`cards/builtins/file-viewer-markdown-toc.tsx`、
`shared/components/SchemaForm.tsx:105-135`、`cards/registry.ts:60-75`、`calm.css` 的 `.cm-*` 全部 14 处、
全仓 `querySelector/closest` 调用点。

## 总体判断

**修订是实质性的，四个阻断项已解决三个半。** 剩下的不是"方向错"，是**落法层面的三个具体缺陷**
（§4.7 的运行时守卫欠一半、§8 裁决 2 的归属与代码不符、§4.2 unlayered 例外缺约束）
外加一批**修订引入的新不一致**（§6 冻结 `core/tokens` 与 §4 把 tokens 放 `styles/` 直接打架等）。

**结论：还不能进阶段 1 冻结，但差距是"半天到一天的编辑工作 + 三条裁决"，不是再来一轮设计。**
最硬的剩余缺口不在正文，而在 §6 八项接口**仍无产出文件路径**、§7 **仍无所有权表**——
而 owner_slice 归一化已经完成了，这两项现在**没有任何东西挡着**。

## 四个阻断项的解决状态

| # | 第一轮阻断项 | 状态 | 说明 |
|---|---|---|---|
| 1 | `core` 定义自相矛盾（JSX） | ✅ **解决** | §4.8 裁决不允许 JSX，理由（黑名单必漏 + 无真实跨端用例）成立且我核对了代码 |
| 2 | `events` 归属两处矛盾 | ⚠️ **半解决** | 三分法本身正确；但保序落法（"未 set 版本就 start() 抛"）**只覆盖 INV-APP-019 三段顺序中的一段**，见 §1 |
| 3 | "只有 core 跨端"混淆纯逻辑与平台逻辑 | ✅ **解决** | §4.6 改判据 platform-independent，7 条行号我逐条核对，全部属实 |
| 4 | owner_slice 未规范化 | ✅ **解决**（本轮实测） | `owner-aliases.yaml` 148 键覆盖 148 个 distinct 值，**0 未覆盖**；且 21:11 全量重标已落地：1126 条全部带 `runtime_layer`/`verification_owner`/`test_tier`，owner_slice 收敛为 114 个规范值，**无一个不带层前缀** |

---

## 1. §4.7 的运行时守卫**不成立**（本轮最重要的发现，读代码得出）

`INV-APP-019` 锁的是**三段顺序**：`setSyncEventVersion → subscribe → start`
（`eventBridge.test.tsx:175-191` 用 `invocationCallOrder` 逐对断言 set<sub、sub<start）。
文档提出的守卫是"未经 `setSyncEventVersion` 就 `start()` 会抛"。**它只封住 set→start 这一段。**

- **`subscribe` 完全不在守卫内。** `setSyncEventVersion(7) → start() → subscribe(['*'])` 会**顺利通过**守卫，
  而这正是 #215 要防的形状（socket 在 topic 装好前就开了）。
- **`setSyncEventVersion` 早于 `subscribe`** 这条（#198 concern 2 的原始诉求，见
  `app-dataflow.yaml:316-321`）也不被守卫覆盖。
- 守卫**与 `INV-APP-020` 无关**：020 要求 bridge 是**唯一** `start()` 调用方。第二个调用方在版本已设后调
  `start()`，守卫静默放行（`start()` 本身 `if (this.ws) return`，`events.ts:293-297`）。
- 守卫**与 `INV-APP-001` 无关**：bridge 必须在 `ServerCompatGate` 内部是渲染树结构约束，
  运行时守卫看不到，仍需 `providers.test.tsx:374,396` 那类结构测试。
- **守卫会打破 `INV-APP-021` 明写的测试逃生口**：`events.ts:624-626` 原文
  "For tests that need a connected stream without the bridge in scope, construct `new EventStream(url)`
  directly and call `start()`"。加了抛异常守卫后这些测试**全部炸**。
- 实现细节坑：`setSyncEventVersion(version: number|null)` 接受 `null` 且对非法值**静默 return**
  （`events.ts:278-286`），"是否调用过"必须用独立的 `versionConfigured` 标志，不能看 `syncEventVersion !== null`。

**建议落法**（三选一，不要只靠抛异常）：把三段合成一个不可分调用
`stream.arm({ syncEventVersion, topics }).start()`，`arm()` 返回一个只暴露 `start()` 的句柄；
或 `start(opts: {syncEventVersion:number; topics:string[]})` 一次性传参，顺序在实现内固定。
这样 019 的三段序**在类型层就不可倒置**，同时保住 021 的逃生口（`arm` 后直接 `start`）。
守卫方案仅作补充。文档 §4.7 那段警告要相应改写，并把 020/021/001 三条各自的承接测试写明。

**另一条 §4.7 没提的硬约束**：`GATE-APP-079`（`app-dataflow.yaml:1452-1470`）要求
`import.meta.env.DEV` 短路**内联写在 EventBridge effect 的调用点**，"重构成单函数调用前必须
`grep __neigeEvents__ web/dist/assets/*.js` 复验"。§4.7 恰好把它抽成了独立的 `app/events/dev-trace.ts`
——这就是被禁止的那种重构。要么在 §4.7 注明"调用点短路必须留在 EventBridge 内，dev-trace.ts 只放
buffer 实现"，要么写明该 gate 的复验步骤。

## 2. §4.8 禁 JSX：裁决正确，`core/markdown` 边界能划清，但**收敛口径与 oracle 冲突**

**边界能划清（读代码确认）**：`report-outline.ts` 全文 104 行**零 JSX、零 DOM**；
`file-viewer-markdown-toc.tsx` 的 `extractHeadings`（:30-87）同样纯，JSX 只在 `MarkdownToc`（:96-152）。
所以 parse/normalize/outline 下沉 `core/markdown`、render 留端侧，**物理上干净**。

三个必须写进文档否则 agent 会做错的点：

1. **`report-outline.ts:2` `import type { ReportBlock } from '../cards/builtins/wave-report'`**
   —— 下沉 core 之前 `ReportBlock` 必须先进 `core/domain`。这是 §6 冻结顺序的一条隐藏依赖，表里没有。
2. **INV-DUP-005 要求"保留两种 id 前缀方案各自的锚点稳定性"**（`pages-shared.yaml:3806-3820`），
   而 §3 病灶 5 写的是"各收敛为单一模块 / 唯一 public entry"。两者只有在
   `core/markdown/outline` 导出**一个内核 + 两个具名策略**（`reportHeadingId` = `<blockId>-h<n>`、
   `tocHeadingId` = `md-h-<i>`；H1-H2 vs H1-H4）时才同时成立。文档必须写死这句，
   否则 agent 会"统一 id 方案"把锚点全改了。
3. **病灶 5 的 lint 规则有洞**：规则写"禁止其他模块直接 import `react-markdown`/remark/rehype"，
   但 `report-outline.ts:1` 用的是 **`mdast-util-from-markdown`**，`file-viewer-markdown-toc.tsx` 用的是
   **手写正则**（`HEADING_RE`/`FENCE_RE`/`SETEXT_RE`）。**两个现存重复实现都不会被这条规则捕获。**
   包名清单至少要加 `mdast-*`/`micromark*`/`unified`；手写解析器只能靠 §8 裁决 5 的硬编码清单挡。

## 3. §3 十一条检查：修订后基本可实现，两处仍有绕过

- **§3.2 `no-module-runtime-state` ✅**。定义精确了，`events.ts:628` 的 `let _shared`、`:559` 的
  `let probeInFlight` 都被"顶层 `let`"捕获（我核对了行号，正确）；`Object.freeze` 白名单解决了误报；
  `declare module` 豁免正确（`GATE-CARD-083/084` 确实存在）。承认"无法零误报 + 需 allowlist" 是诚实的。
- **病灶 7 双向 set 相等 ✅ 闭合。** 唯一补充：CSS Modules 里的类不能进这个 manifest，
  比对源必须限定为 `styles/**` 与 `:global()` allowlist，否则每加一个 module 类就红。文档没写限定域。
- **病灶 8 "解析静态 selector" ⚠️ 仍有洞（读代码得出）。** 全仓 20 个 `querySelector/closest` 调用点里，
  **一半以上实参是跨模块常量**：`MODAL_SELECTOR`（`wheelRouter.ts:117`）、`XTERM_ROOT_SELECTOR`（:141）、
  `CARD_SHELL_SELECTOR`（`useCardVisibilityFocus.ts:16`）、`FOCUSABLE_SELECTOR`（`Dialog.tsx:133`）。
  "解析静态 selector" 需要跨模块常量折叠，ESLint 做不到；落到"动态一律报错"就是这四处全部要改封装 locator——
  可行，但文档要明说这是**四处改造工作量**，不是零成本规则。
  **更硬的一点：`file-viewer.tsx:135` 查的是 `.cm-scroller`，第三方类，无法改 `data-*`。**
  "一律 `data-*`"必须开第三方 allowlist，否则规则上线即无法满足。

## 4. §4.1/§4.2：六个坑基本正确，但 ④ 与 ⑤ **互相矛盾**，且 unlayered 例外缺关键约束

**新引入的内部矛盾**：④ 说"带 `!important` 时早层赢（反转）"，⑤ 说"未分层 CSS 仍压过一切分层声明"。
按 CSS Cascade 5，important 反转后 **unlayered important 是优先级最低的一档**——
⑤ 只对**普通声明**成立。原文是无条件表述，会让 agent 在 unlayered 例外文件里写 `!important`
以为更保险，实际**反而输给任何分层的 `!important`**。⑤ 必须加限定语。

**关于"`.cm-*` unlayered 例外会不会反压 `ui`/`features`"——是真实风险，但可被约束掉。**
我读了 `calm.css` 全部 14 处 `.cm-*`：`:2140-2145`、`:5089-5096`、`:5318-5347`、`:5546-5547`。
**每一处的 key selector 都是 `.cm-*`**（`.report-code .cm-scroller` 这种形态），
所以爆炸半径天然限制在 CodeMirror 内部节点上。**只要加一条 stylelint 规则即可闭合**：

> 该例外文件里每条规则的**最右侧复合选择器必须含 `.cm-` 类**（祖先选择器不算）。

文档现在只说"走具名 unlayered 例外文件 + manifest 登记"，没有这条，agent 完全可以往里写
`.file-viewer-code-wrap { ... }`，那就真的压过整个 `features` 层了。**建议直接写进 §4.2 检查 ①。**

**第二个未提的坑**：这 14 处的祖先选择器 `.report-code` / `.wave-report-files-code-wrap` /
`.file-viewer-code-wrap` / `.file-viewer-merge` 都是**应用自己的全局类**。新架构下它们会变成
CSS Modules 的哈希类名，**unlayered 全局文件将无法引用**。这些 hook 必须改成 `data-*` 或进
§3 病灶 7 的全局类 manifest。文档没说，这是"上线即失效"级别的遗漏。
另注：`.cm-panels.fv-code-search-panels-empty` 里的 `fv-*` 是应用在运行时加到 CM 节点上的
（`file-viewer-codemirror.tsx:52-55`），同样要进 manifest。

## 5. §8 裁决：3/4 站得住，**裁决 2 与代码不符**，裁决 5 清单被截断

**裁决 2（DirectoryPicker）❌ 归属错了（读代码得出）。**
文档写"拆 `ui/directory-browser` + `features/wave/create/DirectoryPickerField`"。但实际：
`DirectoryPicker` 这个**字段**的唯一消费方是 `SchemaForm.tsx:119-126`，触发条件是
`field.type === 'directory' || 'file'`；而该字段类型由**卡片 create schema** 定义
（`cards/registry.ts:63-72`：codex 的 `cwd`、file-viewer 的 path）。**它和 wave create 没有专属关系。**
把它放进 `features/wave/create`，`ui/schema-form`（owner-aliases 已把 `shared/schema-form` 归到这里）
就要**向上 import features**——直接违反 §2 的依赖方向。
且 `owner-aliases.yaml` 里根本没有 `features/wave/create/DirectoryPickerField` 这个值：
`shared/directory-picker` → `ui/directory-browser`，`shared/schema-form` → `ui/schema-form`。
**文档与已冻结的别名表不一致。**
**建议改判**：`ui/directory-browser`（收 `listDir` port）+ `ui/schema-form/fields/DirectoryField`
（同层，也收 port），port 的实参由 `app` 或消费方 feature 注入。注入 `listDir` 这一半是对的，保留。

**裁决 5（硬编码清单）方向对，但清单被截断。** 文档写 `INV-DUP-001..006`，
实测 `pages-shared.yaml` 里有 **INV-DUP-001..010**：007 `readHostThemeRgb` 两份、
008 EditableTitle 两份（且"Cove 版有 #288 合成 click 抑制器、Wave 版没有"必须保留）、
009 WaveRow pin 两份、010 "Delete wave?" 文案两份。**把 007-010 漏在阻断门外，这四组重复会原样长进新仓。**
改成 `INV-DUP-001..010`。

裁决 1、3、4 我核对了依据（`editor/README.md:19-21,24` 原文确认 "folder-level split"、"deferred"、
"Scaffold only"；`GATE-CARD-083/084` 存在），**成立，无需再议**。

## 6. 修订引入的新问题（悬空引用 / 前后不一致）

按严重度排序，全部是文档内部可核对的事实：

1. **§6 第 4 项冻结 `core/tokens`**，但 §2 的 `core` 内容清单里没有 tokens、§4 把 tokens 定义成
   **CSS `@layer tokens`**、`owner-aliases.yaml:159` 把 `design-system/tokens` 归到 **`styles/tokens`**
   （实测 57 条 `runtime_layer: styles`）。**三处打架。** 要么 §6 改成 `styles/tokens`，
   要么明说"token 值定义在 core、CSS 变量投影在 styles"并给出两侧的文件路径。
2. **§4.5 目录树的 `features/` 漏了 `spec`**（第 236 行：`wave · cove · today · report · settings · auth`），
   而 §2 层图里有 spec、别名表里 `features/spec/*` 有 6 个 slice、78 条 oracle。直接补上。
3. **`core/types` 是悬空引用**：§2 层图与 §9 待定 2 都提"`ui` 可依赖 `core/types` 白名单"，
   但 §4.5 的 `core/` 下只有 api/domain/schemas/keys/state/markdown/events，**没有 `types/`**。
4. **`runtime_layer: none` 的 96 条在磁盘上无处安放**：§2.3 定义了这个值，§4.5 的目录树里
   既没有 `tooling/` 也没有 e2e/lint 的位置。§7 要求"每文件恰好一个 owner"，
   这 96 条（lint 自定义规则、stylelint、e2e 基建、CI、ts-rs）是**最容易并发冲突的共享可写区**，
   必须在 §4.5 给出目录 + 在 §7 给独立 owner。（第一轮我提的 `tooling/` 建议只被 schema 吸收了一半。）
5. **`enforcement` 与 `verification_owner` 是同一件事的两个字段**：§2.3 定义
   `verification_owner: e2e|unit|lint|css|build|architecture|review-waiver`，§3.3 又定义
   `enforcement: e2e|unit|lint|architecture|review-waiver`（真子集）。
   实测重标只写了 `verification_owner`，`enforcement` **0 条**。§3.3 应直接引用 `verification_owner`。
6. **§3 病灶 5 写"markdown pipeline 5 份"，§4.8 与裁决 1 写"4 份"**，INV-DUP-004 原文是"四处"。统一成 4。
7. **§7 的阻断表述已过期**："完整所有权表依赖 owner-aliases.yaml 的归一化结果……归一化未完成前 §7 不可验收"
   —— 归一化 21:11 已完成（1126/1126）。这句要改成"所有权表待生成"，否则读者会误以为还卡在上游。
8. **`SCHEMA.md` 没跟着改**：它仍写"字段全部必填"并只列旧 9 个字段、`migration: 固定填 pending`，
   而实际 yaml 已有 `runtime_layer`/`verification_owner`/`test_tier`、3 条 `migration: skipped` + `skip_reason`。
   SCHEMA.md 是提取纪律的单一事实源，必须同步。
9. **`mock/`（从 openapi.json 生成）无 owner、不在任何层**，§4.5 只画了目录。生成物也要有 owner 与再生成命令。
10. 排版：`## 4.5` 是 h2 而 `### 4.1/4.2` 是 h3，导致 4.6-4.8 在大纲上挂在 4.5 之下；`§3` 有 3.2/3.3 无 3.1。

## 7. 能否进入阶段 1 接口冻结

**不能，但只差一批可枚举的动作。** 剩余清单（按阻断度排序）：

**必须裁决（3 项，需要判断，不是编辑）**
1. §4.7 保序落法改为 `arm().start()` 或 `start(opts)` 一次性传参；同时写明 INV-APP-001/020/021
   各自的承接测试与 021 逃生口的去留。
2. §8 裁决 2 改判为 `ui/directory-browser` + `ui/schema-form/fields/DirectoryField`（依据：SchemaForm 是唯一消费方）。
3. `core/tokens` vs `styles/tokens` 二选一并写明两侧文件。

**必须补齐（阶段 1 的实际产出物，现在无任何前置阻挡）**
4. §6 八项接口**每项标出产出文件路径**（第一轮已提，本轮仍缺）。
5. §7 生成所有权表：114 个规范 slice → owner agent → 文件 glob。归一化已完成，可以直接跑。
6. §4.5 给 `runtime_layer: none` 的 96 条一个目录位置，并在 §7 给它独立 owner。

**编辑级修正（10 分钟）**
7. §4.1⑤ 加"仅普通声明"限定；§4.2 检查 ① 加"key selector 必须含 `.cm-`"；补 CM 覆盖的祖先 hook 改 `data-*`。
8. 裁决 5 清单改 `INV-DUP-001..010`；病灶 5 包名清单加 `mdast-*`/`micromark*`/`unified`。
9. §4.5 补 `spec`；补 `core/types`；§3.3 删 `enforcement` 改引 `verification_owner`；
   §7 删过期阻断句；"5 份"改 4；同步 `SCHEMA.md`。
10. §4.8 写死 outline 的"一个内核 + 两个 id 策略"（INV-DUP-005），并注明 `ReportBlock` 需先进 `core/domain`。

**口径说明**：第 1-5 节与第 6 节的 1/2/3/4/6/7/8 是**读代码/读 oracle 得出的**（行号均已核对）；
第 6 节第 9/10 项与"半天到一天"的工作量估计是**凭经验推测**。
