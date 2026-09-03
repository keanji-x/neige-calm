# #1148 S1+S2 实施报告 —— source-anchor 判据加强

分支 `fix/1148-anchor-strength`。全库 1088 条 oracle 条目。

## S1 — 三条与实现不符的 statement

`web/src/TrackList.tsx:234-240` 只处理 `Delete`，注释明写 Backspace 是编辑键、刻意不做破坏性快捷键；
`web/src/TrackList.test.tsx:638` 有一条测试断言 Backspace 不删除。实现是对的，oracle 文案是错的。

| id | 动作 |
| --- | --- |
| `CAP-A11Y-069` | statement 改成只认 `Delete`，并写明 `Backspace` 是刻意排除（编辑键）；why 补上排除理由；`source` 从 `30-32,215-235`（全是注释 + 漏掉 Delete 分支）重指到 `234-240,258`（Delete 分支 + `aria-keyshortcuts`） |
| `INV-CARD-142` | statement 的认领集合去掉 Backspace，并写明它刻意落到 hook |
| `CAP-CARD-144` | statement 去掉 Backspace；`source` 重指到 `234-240,259-270`；`authoritative_test` 从 `598,616`（指到 Alt+ArrowDown 的用例）改为 `620,638,730`（Delete / Backspace / × 三条真用例） |

`intentional_omission` 三条都保持 `false`：改后的 statement 已经把「Backspace 不绑删除」写进契约本身，
契约内部没有留缺口，不满足 `intentional_omission` 的语义。

## S2 — 抽取判据加强

### 最终规则定义（`fe/tools/oracle/validator.ts`）

判据强度标准：**anchor 缺席 = 真缺陷**。宁可留在「抽不出」桶里，也不用通用词制造碰巧命中。

新增两类 anchor，都只在原有 identifier 形状之外**追加**，不改动任何原有形状：

**1. 展示文案（display copy）** —— statement 里被 `"…"` 或 `“…”` 包住的片段。

- CJK 一票否决：这些 statement 里被引号包住的中文全是行文强调（`不得因为"看起来无关"而重排`），
  不是 UI 文案，永远不会出现在源码里。要求至少含一个拉丁字母。
- 引文按「不可匹配段」切开，**每一段幸存片段各自成为一个 anchor**（不是只取最长）：
  - 占位符 `<message>` / `{count}`：运行期才有值，源码里只有字面前缀。
  - 非可打印 ASCII 的连续段（`—`、`→`、弯引号）：源码常写成 HTML 实体（`&mdash;`）或转义，
    statement 里的字符不是文件里的字节。
  - 为什么不是「只取最长」：`"Issue dev / issue → PR autoflow"` 在 JSX 里是两个兄弟 span，
    较长的一半 `Issue dev / issue` 跨过了切点，而 `PR autoflow` 就在代码里。
- 长度下限 6 字符，滤掉 `"x"` / `"on"` 这类噪声。
- 匹配**不区分大小写**（statement 常用小写复述界面分区名：`"waiting on you"` vs 代码里 `Waiting on you`）。
  这一条只对展示文案类 anchor 生效，identifier 类仍严格区分大小写 —— `cardId` 和 `CardId` 在代码里
  是两样东西，混同就是制造命中。实现落在 `codeAnchorLines(path, contents, identifiers, caseInsensitive)`
  的第四个参数上，TS 与 CSS 两条路径都走同一个 `boundedOccurrenceOffsets`。

**2. 反引号代码片段** —— 作者用反引号显式标成代码的整片段（原有规则只收 camelCase / 连字符 / `aria-` 形状）。

- 词形：`/^[A-Za-z_$][A-Za-z0-9_$]*$/` 且长度 ≥ 4（`children`、`overflow`、`basepath`）。
- 路径/点分/下划线形：`/^[/@]?[A-Za-z0-9_$][A-Za-z0-9_$./-]*$/` 且含 `/`、`.` 或 `_`，长度 ≥ 4
  （`/api/auth/whoami`、`/dev/reset`、`web/src/ui`、`card.updated`）。
- 调用形 `foo(` 原本就被全局 pattern 覆盖，未重复实现。

**通用词停用表 `GENERIC_ANCHOR_WORDS`**（源码内，逐组带注释说明为什么不配当判据）：React/DOM 词汇、
CSS 内建值、本仓遍地都是的领域词。查表时统一小写。只作用于上面两类新候选，不影响原有形状。
`change` / `changes` 是实测加进去的：`INV-CHATITEM-008` 的 statement 引的是 UI 兜底文案 `"change"`，
落点却是 `plannerChatItems.ts` 里的形参名 —— 正是这张表要拦的碰巧命中（加表后该条回到原有 baseline 状态）。

**单调性**：新 anchor 只让 `present` 变大，`anchored` 是 `some`，所以绿条目不会因此变红。
实际跑数验证（不是推理）：1088 条里 `ok → 非 ok` 的转移数 **0**。

### 三档数字

| 档 | 条数 |
| --- | --- |
| 重新获得判据（原「抽不出 identifier」静默通过 → 现在有真 anchor） | **35**（34 条直接绿，1 条只引用了 unsupported 扩展名，不产生错误） |
| 新暴露并已逐条处置的红条 | **11 条新暴露 + 2 条被新规则救回的旧红**（全部处置为绿，**0** 条进 baseline，**0** 条进 pending）[^exposed] |
| 仍抽不出 identifier | **172**（原 207） |

附带效果（不是本 PR 的目标，但必须记账）：原本就红的条目里有 **48** 条因为拿到真 anchor 转绿，
另有 **10** 条 subtype 从 `not-in-file` 变成 `range-miss`。两个账本因此收缩：

- `anchor-baseline.json` 214 行 → **174** 行，`ANCHOR_BASELINE_MAXIMUM` 218 → **174**。
- `anchor-pending.json` 38 行 → **30** 行，`ANCHOR_PENDING_MAXIMUM` 38 → **30**，
  `ANCHOR_PENDING_IDS` 同步删掉离场的 8 个 id（`E2E-CAP-AXE-008`、`E2E-CAP-DELETE-003`、
  `E2E-CAP-DELETE-020`、`E2E-CAP-LIST-018`、`E2E-CAP-MODAL-012`、`E2E-CAP-RENAME-011`、
  `E2E-CAP-RENAME-016`、`E2E-INV-SPECCHAT-008`）。留在 pending 的
  `E2E-CAP-ADDPANEL-007` / `E2E-CAP-DELETE-001` subtype 变了，note 一并改写成准确描述。
- 实际失败 204 条 = baseline 174 + pending 30，无未记账项，无双重记账。

[^exposed]: 原文写作「13 条新暴露」，不准确，实测复核（在 `636ddc46` 的工作区上跑 main 版
    `extractStatementIdentifiers`）：`CAP-SPECCONVO-021` 与 `CAP-AREA-NEWTRACK-017` 在 main 上抽出的
    identifier 都是 `[]` —— 它们既不在 main 的 baseline 也不在 pending，是**无判据静默通过**，不是红条。
    新规则给了它们 anchor 且当场命中，所以这两条属于上面「重新获得判据」那 35 条，把它们再算进「新暴露」
    是重复记账。真正因判据加强而首次变红的是 **11** 条。

### 逐条处置清单（11 条新暴露 + 2 条救回的旧红）

| id | 新 anchor | 定性 | 动作 |
| --- | --- | --- | --- |
| `INV-A11Y-010` | `waiting on you` | 锚点漂移：`Sidebar.tsx:58-66` 指的是 `writeExpandedAreas`，与 statement 毫无关系 | source → `Sidebar.tsx:271-290`（`aria-label="Waiting on you"` 分区 + `title={area ? …}` 那行） |
| `INV-APP-047` | `/api/auth/whoami` | 锚点漂移：只引了 close handler，漏掉真正发探测的 `probeUnauthorized` | source → `events.ts:313-359,558-566` |
| `E2E-INV-INFRA-017` | `/dev/reset` | 锚点漂移：`reset.ts:16-17` 全是注释（注释被扫描器剥离） | source → `reset.ts:31-33`（用 `REPLAY_PORT` 拼 `/dev/reset` 的那行） |
| `E2E-CAP-SHELL-006` | `/calm`、`/calm/` | statement 把 `basepath: '/calm'` 整体塞进反引号，抽不出可匹配片段；且真正定义 basepath 的是 router 不是 test | statement 改写成 `` `basepath` set to `/calm` ``；source 加 `web/src/app/router.tsx:153-161` |
| `E2E-CAP-SHELL-010` | `Open user menu` | `/calm/settings` 在 test 里写成转义正则 `/\/calm\/settings…/`，字面不存在；真正可锚的是 accessible name | statement 把 `` `Open user menu` `` 改成 `"Open user menu"`（展示文案），命中 `a11y-keyboard.spec.ts:855` |
| `E2E-INV-CWD-004` | `siblingPrefix`、`claim` | statement 与实现不符：`/work/repository` / `/work/repo` 是示意写法，代码用的是 `/work-${ts}/…` | statement 改成引用真实变量名 `siblingPrefix` / `claim`，示意路径去掉反引号 |
| `E2E-CAP-TERMINAL-006` | `signal_killed` | statement 与实现不符：`signal` 在被引 test 里只出现在注释；承载 signal 语义的是 `XtermView` 的 `signal_killed` 字段 | statement `` `signal` `` → `` `signal_killed` ``；source → `XtermView.tsx:60-90 terminal-clean-exit.spec.ts:120-127` |
| `E2E-INV-ENV-002` | `echo`（`/etc/services` 天然锚不住） | 锚点漂移：`92-96` 全是注释，真正的 POSIX echo 循环在 99 行 | source → `wheel-track-switch.spec.ts:92-100`。这条的否定半句（「没有 `/etc/services`」）原理上锚不住，但肯定半句锚住了，无需 pending |
| `CAP-REPORT-SHELL-014` | `Multiple report cards found. Showing the earliest.` | 锚点漂移：没引渲染横幅的 `DuplicateReportBanner` | source 增补 `TrackReportPage.tsx:197-202` |
| `INV-REPORT-BACKLINK-010` | `cites block` | 锚点漂移：没引渲染 `· cites block {id}` 的那段 JSX | source 增补 `TrackReportPage.tsx:704-708` |
| `CAP-SPECCONVO-021` | `No messages yet` / `ask the Planner Agent below.` | 源码把破折号写成 `&mdash;`，整串永远匹配不上 | 由「非 ASCII 段切分 + 每段各自成 anchor」规则解决，条目本身不改，命中 `PlannerConversation.tsx:598` |
| `CAP-AREA-NEWTRACK-017` | `PR autoflow` | 同上：`issue → PR autoflow` 里的 `→` 不可匹配，且文案被拆成两个 span | 同上，命中 `Area.tsx:545` |
| `INV-NEWTASK-ISSUEDEV-020` | `derivedWorkflowInput` | 原理上锚不住：`notes` 是「刻意不存在」的东西，只在注释里出现 | 采用简报的 (a) 路线的等价做法：statement 改成指名**存在**的载体 `` `derivedWorkflowInput` 刻意不带 `notes` 字段 ``，source 从 `369-373` 扩到 `369-374`（`derivedWorkflowInput` 的定义行）。这样同一条不变量由一个正向 anchor 支撑，不需要进 pending |

**没有任何一条进 `anchor-baseline.json` 或 `anchor-pending.json`。** 两个账本本次只减不增。

## 测试与变异验证

新增 fixture 目录 `fe/tools/oracle/fixtures/source-anchor/anchor-classes/`，共 11 个，
**每个目录只有一条 entry**，配一个共享的被引文件 `fixtures/anchor-display-copy.ts`（7 行，每行一种形状，
第 4 行是不含任何 anchor 的诱饵行）。

红向 fixture 之所以能钉住分支：删掉某条抽取分支后，该 statement 一个 identifier 都抽不出，
`identifiers.length === 0` 的捷径会让它静默通过 —— 于是**只有**这条 fixture 变红。

变异验证（先 commit，再 `git apply` patch，读具体是哪条测试红）：

| 变异（改掉的实现分支） | 变红的测试（除 `accepts all real oracle data without exceptions` 外，它对任何变异都红） |
| --- | --- |
| 反引号整片段整个 `if` 短路 | `anchor class backtick-path-range-miss …`、`anchor class backtick-word-range-miss …`、`extracts display copy and backtick fragments…` |
| 只短路 `wordShaped` | `anchor class backtick-word-range-miss …`、`extracts display copy and backtick fragments…` |
| 只短路 `pathShaped` | `anchor class backtick-path-range-miss …`、`extracts display copy and backtick fragments…` |
| 展示文案抽取循环改成空迭代 | `anchor class display-copy-range-miss …`、`anchor class display-copy-with-identifier produces no violation`、`extracts display copy and backtick fragments…` |
| `caseInsensitive` 传参改成空集 | `anchor class display-copy-case-insensitive produces no violation` |
| `UNMATCHABLE_RUN_PATTERN` 去掉占位符段 | `anchor class display-copy-placeholder produces no violation`、`extracts display copy and backtick fragments…` |
| `UNMATCHABLE_RUN_PATTERN` 去掉非 ASCII 段 | `anchor class display-copy-typography-split produces no violation`、`extracts display copy and backtick fragments…` |
| 去掉 `isGeneric`（反引号侧） | `anchor class generic-word-not-anchored produces no violation`、`extracts display copy and backtick fragments…` |
| 去掉 `isGeneric`（展示文案侧） | `anchor class generic-display-copy-not-anchored produces no violation` |

注意「展示文案抽取循环」那一行只红了 3 条：`display-copy-placeholder` / `-case-insensitive` /
`-typography-split` 是绿向 fixture，抽不出 anchor 时会走 `identifiers.length === 0` 的静默通过捷径，
所以钉住它们各自分支的是上面下面几行的定向变异，不是这一行 —— 这正是每类判据都要有自己变异的原因。

另有一条集合相等的元测试 `anchor classes: every fixture directory is exercised, in both directions`，
保证新加 fixture 目录不会漏跑。

## 门禁（S1+S2 轮）

```
cd fe && npx vitest run --project platform-independent
  → Test Files 39 passed | 1 skipped (40)；Tests 908 passed | 1 skipped (909)
    其中 tools/oracle/oracle.test.ts 86 passed（改动前 73），含
    `accepts all real oracle data without exceptions`（全库 1088 条零违规）

cd fe && npm run lint:js       → exit 0（eslint --max-warnings=0 + 5 个架构/所有权检查全过）
cd fe && npm run typecheck     → exit 0（tsc -b）
```

---

# 评审修复轮（双通道裁决：codex 通道的 6 条阻断项全部成立）

## A0 停用表补本仓领域词 —— A1 假绿的机制性根因

subagent 的逐词消融证明停用表就是这套判据的实际护栏，而本仓遍地都是的领域词不在表里。补入
`theme`/`themes`、`terminal`/`terminals`、`codex`、`area`/`areas`、`report`/`reports`，每个在源码里
写清它覆盖多少源文件、为什么命中它只证明「引对了区域」而非「引对了行」。另把 `view`/`views` 单独成组，
披露它正在替 `INV-UI-DIALOG-003`（statement 是「focus effect 的依赖**不得**包含 `view`」，证明不存在类）
做静默豁免 —— 词表理由本身站得住，但这条关系必须像 `change` 那样写在源码里，不能留成隐式。

**补词后的全库翻转清单（1088 条，只有 3 条动，逐条实跑得出）**：

| id | 翻转 | 处置 |
| --- | --- | --- |
| `E2E-INV-INFRA-019` | 绿 → 红（`range-miss`）；唯一 anchor 是 `theme` | A1，见下 |
| `E2E-CAP-ADDPANEL-007` | 红 `range-miss` → 红 `not-in-file`；`terminal`/`codex` 出表后只剩 `aria-hidden`，而它在该文件里只出现在注释 (`a11y-keyboard.spec.ts:465`) | pending 行 subtype 与 note 同步改写（仍在 #1170 名下，账本不增行） |
| `E2E-CAP-ADDPANEL-010` | 绿 → **无判据静默通过**（唯一 anchor 是 `terminal`） | statement 改写成指名轮询的两个真载体 `` `/api/tracks` ``（:406）与 `` `containsTerminalCard` ``（:410），重新拿回判据 |

其余 1085 条无变化。「抽不出 identifier」的条数经 ADDPANEL-010 处置后仍是 172（与 S2 轮相同）。

## A1~A6 逐条判定与处置

| id | 判定 | 处置 |
| --- | --- | --- |
| `E2E-INV-INFRA-019` | **确认假绿**。`reset.ts:105-114` 是一个**合法**请求体（110 行带着 `theme`），区间里没有任何东西证明「缺字段被拒」 | 走简报的优先路线：statement 改成这段 helper 真正保证的东西（每个建 track 的 fixture 走同一个 body 形状），source 收到 `106-112` 的对象字面量，anchor 落在 `area_id`(107)/`attach_folder`(110)。**全库没有任何 oracle 条目覆盖服务端「缺 theme → 422」契约**；已在 `why` 里点名它由 `crates/calm-server/tests/cases/theme_required.rs` 证明、本条不再声称，等你另开 issue |
| `INV-A11Y-010` | **文案错，不是代码缺陷**。行按钮（`Sidebar.tsx:653-660`）不带 aria-label／aria-labelledby，name 由内容计算，必然包含 `side-track-area` 的 area 名；且 `docs/` 下已无 §2.2 那份 a11y 契约原文可援引。area 名是同一按钮里可见的 span，藏起来反而让屏幕阅读器听不到跨 area 的消歧信息 | statement 改成「name = track 标题 + area 名，`title` 属性纯属信息性」；source 从只框住分区标题的 `271-290` 重指到 `278-283,653-660` |
| `CAP-REPORT-SHELL-014` | 三段陈旧区间证实：`84-87` 是类型成员、`163-169` 是 localStorage helper、`955-957` 是 Files rail | source 改为 `118-120`（`selectReportCards` 按 sort 升序）、`197-202`（横幅文案）、`779-781`（取第一张）、`1035-1037`（`>1` 才渲染）；statement 点名两个载体。查过全库 30 处 `TrackReportPage.tsx:` 引用，**没有第二条**携带这三段区间 |
| `E2E-CAP-TERMINAL-006` | **真缺口**。全仓搜索确认：断言 exit badge 的测试只有 `terminal-clean-exit.spec.ts:121,127`（`exit 0` + success 配色），`signal_killed` 只在 `XtermView.test.tsx`/`codex.test.tsx` 里当**输入**出现、无人断言 badge 的 signal/非零分支 | 不找区间凑绿：source 重指真正的 badge 逻辑 `CardExitBadge.tsx:22-25,46-72`（anchor 是 `signal_killed`，那里是它的真实现），`authoritative_test` 改 `NONE` 并在 `why` 里写明「拿 exit 0 的用例当权威测试是循环论证」。缺的是**测试**，不是锚点 —— 等你另开 issue |
| `E2E-INV-INFRA-017` | `reset.ts:31-33` 只是**调用**端点，见证不了「谁挂载」 | source 重指 `crates/calm-server/src/bin/replay.rs:226-245`（`/dev/*` 子路由在此构建、只并入 `--serve`），登记进 `anchor-unsupported.yaml`；`authoritative_test` 改 `NONE`（无人断言这个否定命题，它是「路由在哪构建」的架构事实） |
| `INV-NEWTASK-ISSUEDEV-020` | 原区间 `369-373` 全是 JSDoc、`626-632` 全是 JSX 注释，两段都被 trivia 剥离；statement 与 `authoritative_test:77` 说的确实不是一件事 | statement 合并两层（控件层「表单不提供 notes 输入控件」+ payload 层「对象字面量只有四个键」），与 `:77`/`:277` 两个用例一一对应；source 扩到 `374-382` 的对象字面量与 `386-387` 的 raw JSON 逃生口 |

## B1 四个护栏 fixture 与变异验证

subagent 实跑证明：放宽大小写、`DISPLAY_COPY_MINIMUM` 6→3、`BACKTICK_WORD_MINIMUM` 4→2、去掉 CJK
一票否决 —— 11 个 anchor-class fixture 全绿。也就是说原有 fixture 只钉住了「分支存在」，**一个护栏值都没钉住**。

新增 4 个单违规 fixture（共享被引文件 `anchor-display-copy.ts` 新增第 8 行 `export type CardId = string;`）。
变异验证：先 commit，再改实现、跑测试、读**具体哪条**变红，然后从备份拷回（不用 `git checkout -- <path>`）：

| 变异 | 变红的测试（每次都另有 `accepts all real oracle data without exceptions`，它对任何变异都红） |
| --- | --- |
| `DISPLAY_COPY_MINIMUM` 6 → 3 | `anchor class display-copy-below-minimum produces no violation`（`"Retry"` 变成 anchor，被引文件里没有） |
| `BACKTICK_WORD_MINIMUM` 4 → 2 | `anchor class backtick-word-below-minimum produces no violation`（`` `osc` `` 变成 anchor） |
| 去掉 `CJK_PATTERN.test(quoted) \|\|` 一票否决 | `anchor class display-copy-cjk-mixed produces no violation`（`"保存 Draft copy 草稿"` 切出 `Draft copy`） |
| `typescriptAnchorLines` 里 `caseInsensitive.has(identifier)` → `true` | `anchor class identifier-case-sensitive-miss is the only violation, as not-in-file`（statement 写 `cardId`、被引行只有 `CardId`，本该红；变异后它绿了，红的是这条断言本身） |

每次变异都只红了它对应的那一条 anchor-class 测试 —— 四个护栏各自独立成立。

## B2 记账更正与已知最弱的两个锚

- 「13 条新暴露」已更正为「11 条新暴露 + 2 条原本无判据、被新规则救回的条目」，见上文脚注（实跑复核，不是转述）。
- 当前最弱的两个锚，本轮**不改**，只记账：
  - `E2E-INV-SPECCHAT-008`：anchor `working` 命中的是 `a11y-planner-chat-interrupt.spec.ts:61` 的可访问名
    字符串 `'Planner Agent is working'`，而 statement 指的是 FSM 门控变量；点明这层关系的注释（:58）反而被
    trivia 剥离。锚是真的，但它锚住的不是 statement 说的那个东西。
  - `E2E-CAP-TRACKCREATE-015`：statement 里的 `role="option"` 让 `option` 走**展示文案**通道成为
    anchor（6 字符恰好过下限、不在停用表），于是任何用 `getByRole('option')` 的区间都会绿。
    候选处置是把 `option` 收进停用表或要求它带 `role=` 前缀，属于下一轮判据强度问题。

## 门禁（评审修复轮）

```
cd fe && npx vitest run --project platform-independent
  → Test Files 39 passed | 1 skipped (40)；Tests 912 passed | 1 skipped (913)
    其中 tools/oracle/oracle.test.ts 90 passed（本轮 +4），含
    `accepts all real oracle data without exceptions`（全库 1088 条零违规）
cd fe && npm run lint:js       → exit 0
cd fe && npm run typecheck     → exit 0
```

账本对账（实跑读数，不是估算）：`anchor-baseline.json` **174** 行 = `ANCHOR_BASELINE_MAXIMUM` **174**；
`anchor-pending.json` **30** 行 = `ANCHOR_PENDING_MAXIMUM` **30** = `ANCHOR_PENDING_IDS` 冻结集 **30** 个 id
（无多余、无缺失）；实际失败 **204** = 174 + 30。本轮两个账本一行未增。

---

# 第三轮（第二轮双通道评审阻断项 C1~C5）

本轮**全部是 oracle YAML 的文档级改动**，validator 逻辑一行未动（唯一的 `validator.ts` 改动是把
停用表里 `theme` 那条注释里对 E2E-INV-INFRA-019 旧文案的引用改成过去时，因为该 statement 本轮已收窄）。
核心教训：上一轮为了让 statement 可锚，把几条 statement 写宽到了载体之外 —— anchor 是真的，
但量词/条件超出了被引区间能见证的范围。本轮每条改完都逐分句核对了行号。

| # | 条目 | 改法 |
| --- | --- | --- |
| C1 | `E2E-INV-INFRA-019` | statement/why 从「整个 e2e 套件」收窄到 `createTrackInArea` 这个 shared seed helper；source 从 `106-112` 扩到 `88-93,101-112`（补上函数声明与 theme sentinel 注释） |
| C2 | `INV-A11Y-010` | 把 `side-track-area` 写成条件渲染（上游查不到 area 时传 null），name 是「标题」或「标题+area 名」二选一；source `278-283` → `271-283` 并回分区标题（`TrackRow` 是所有分区共用的） |
| C3 | `CAP-REPORT-SHELL-014` | `authoritative_test` 从 `:864`（unsupported-block 用例，无关）改指 `:1040,1051,1054-1055`（duplicate banner + Earliest/Later 断言） |
| C4 | `E2E-INV-INFRA-017` | source 由单段构造扩为四段：`replay.rs:130-159`（`--assert` 提前 return 的门控）、`226-245`（`/dev/*` 构造）、`277-284`（`.merge(dev_routes)` 唯一一处）、`main.rs:185-192`（生产 router 的全部 merge 面）；`anchor-unsupported.yaml` 同步登记四段 |
| C5 | `INV-NEWTASK-ISSUEDEV-020` | source 补 `456-470`（提交分支：`workflow_input` 只有 `JSON.parse(rawJson)` 与 `derivedWorkflowInput` 两个来源）与 `757-781`（raw JSON textarea）；「表单不提供 notes 控件」是证明不存在类，仍由 `authoritative_test:77` 钉住 |

## C3 顺带扫描：本 PR 改动过的 16 条 `authoritative_test`

`authoritative_test` 只被 `authoritative-test-location` 检查存在性与区间合法性，不检查语义。
逐条实读结果：13 条指得对（`E2E-CAP-SHELL-006:908`、`E2E-CAP-SHELL-010:851`、`E2E-INV-CWD-004:234`、
`E2E-CAP-ADDPANEL-010:360`、`E2E-INV-ENV-002:98`、`CAP-CARD-144:620,638,730`、
`INV-NEWTASK-ISSUEDEV-020:77,277`，另 3 条为 `NONE`、1 条为 helper 自指见下）。发现并修：

- `CAP-REPORT-SHELL-014`（C1 本身）:864 → :1040,1051,1054-1055。
- `INV-REPORT-BACKLINK-010`：`:2338` 落在「report.md loading fallback」用例里，与「`cites block <id>`
  只在有结构化 blocks 时渲染」无关；真正的两个用例是 `:2559`（有 blocks，提示在）与 `:2596/:2618`
  （flat v1 报告，提示不在，正是 why 里引的那句测试名）。已改指 `:2559,2596,2618`。

只记账、本轮不改的两条：

- `INV-APP-047` → `websocket-driver.contract.test.ts:1`。真正的载体是 `:114` 的
  `T-D7 probes only a connection that closes before open`（两个分句都在里面）。但 `:1` 是
  `app-dataflow.yaml` 里 **5 条**条目共用的「文件级指针」写法，只改这一条会造成不一致 ——
  这是全库范围的约定问题，留给 #1170 类的后续。
- `E2E-INV-INFRA-019` → `reset.ts:105`，指向 helper 自己的 `request.post`，与 source 同文件同段，
  是自指而非错指；这条本来就没有独立测试（真正的断言是 helper 自身 `:115-119` 的 throw）。

## 非阻断项量化：`isGeneric()` 的通路覆盖

`isGeneric()` 现在只挡两条通路（`:332` 展示文案、`:352` 反引号整片段），形状扫描（`:341-345`）与
全局散文扫描（`:354-356`）从不查停用表。按简报要求先量化：把 `isGeneric()` 加到全部四条通路后重跑全库，

```
VIOLATIONS 5
<baseline>|<count>|source-anchor|baseline count must equal actual count: declared 174, distinct valid 174, actual 178
<baseline>|CAP-A11Y-044|source-anchor|unbaselined range-miss
<baseline>|GATE-CARDHEAD-004|source-anchor|unbaselined not-in-file
<baseline>|INV-A11Y-056|source-anchor|unbaselined range-miss
<baseline>|INV-APP-020|source-anchor|unbaselined range-miss
```

即 **4 条翻转**（`CAP-A11Y-044`、`INV-A11Y-056` 与 `GATE-CARDHEAD-004` 现在靠 `className`/`classname`
维持判据，`INV-APP-020` 靠 `start()`），且账本要从 174 涨到 178 —— 那还要同时抬 `ANCHOR_BASELINE_MAXIMUM`
这个「只减不增」的上限。**> 3 条，按简报的判据本轮不做**，量化结果留档，另开 issue。
验证用的 `validator.ts` 补丁已还原（`git diff` 对该文件除上述注释外为空）。

## 门禁（第三轮）

```
cd fe && npx vitest run --project platform-independent
  → Test Files 39 passed | 1 skipped (40)；Tests 912 passed | 1 skipped (913)
cd fe && npm run lint:js       → exit 0
cd fe && npm run typecheck     → exit 0
```

另实跑 `validateOracle(defaultOracleOptions(repoRoot))`：**VIOLATIONS 0**（全库）。
账本对账不变：`anchor-baseline.json` **174** 行 = `ANCHOR_BASELINE_MAXIMUM` **174**；
`anchor-pending.json` **30** 行 = `ANCHOR_PENDING_MAXIMUM` **30** = `ANCHOR_PENDING_IDS` **30**
（集合相等，无多余无缺失）。本轮两个账本一行未增。

---

# 第四轮（收口轮，本 PR 最后一轮修复）

第三轮双通道结论分歧：subagent 判 1 条阻断，codex 判 5 条。裁决（见 `docs/_1148-fix-brief-r3.md`）：
codex 这轮把标准提到「statement 的每个分句、连同传递性数据流都必须有被引行」，该标准全库无一条条目满足
（包括本来就绿的 638 条），按它执行等于重写整个语料；因此本轮只收口 codex 结论里**事实性**的部分，
标准升级另开 issue。本轮同样是纯 oracle YAML 文档改动，validator 逻辑一行未动。

| # | 条目 | 改法 |
| --- | --- | --- |
| D1 | `INV-REPORT-BACKLINK-010` | source 第三段 `846`（`pending: taskActionMutation.isPending`，与本条无关）→ `949-952`（`hasRenderedBlocks={reportCard?.blocks != null}`，「只在当前报告确有结构化 blocks 时」的真载体）。最终 `TrackReportPage.tsx:637-642,704-708,949-952` |
| D2 | `E2E-INV-INFRA-019` | (a) `authoritative_test: reset.ts:105` → `NONE` —— 该行是 helper 自己的 `request.post`，无 `it`、无断言，按 `SCHEMA.md:15`「无测试写 NONE」。(b) statement 枚举补 `title`（`reset.ts:108` 实际有传、服务端必填），并把「no caller … omits a field the kernel requires」改成「every caller of this helper sends exactly that key set, and no caller can vary it」——不再声称 kernel 的必填集；why 里「#250 PR2 made cwd required」同步改为「put cwd into this body（自 #1131 起服务端取 `Option<String>`，见 `crates/calm-server/src/routes/tracks.rs:195-214`）」。source 不变（`88-93,101-112` 已含 `108`） |
| D3 | `INV-A11Y-010` | 被引的 `Sidebar.tsx:271-283` 里的 `displayTitle` 是父作用域 map 中的同名变量；喂给 `:658` 那个 span 的是 `TrackRow` 自己在 `:633` 算出的同名变量。source `271-283,653-660` → `271-283,633-660` |
| D4 | `INV-NEWTASK-ISSUEDEV-020` | `authoritative_test` 并入 `NewTaskForm.issueDev.test.tsx:174-203`（用 `toEqual` 钉死整个 create body 含 `workflow_input` 恰四键、无 notes），原 `:277` 只证明 raw JSON 能带 notes。最终 `:77,174-203,277` |
| D5 | `E2E-INV-INFRA-017` | source 补 `replay.rs:112-115`（`--serve`/`--assert` 二选一的前置门控，缺任一 exit 2），最终四段 → 五段：`replay.rs:112-115,130-159,226-245,277-284` + `main.rs:185-192`；`anchor-unsupported.yaml` 的 `locations` 同步补同一段（该表要求登记集与实际集**严格相等**，改完实跑校验通过） |

自我更正：第三轮报告里把 `E2E-INV-INFRA-019` 的 `reset.ts:105` 记为「自指而非错指、只记账不改」，
判断错了 —— `authoritative_test` 的语义是「锁定它的测试」，helper 自身的 `request.post` 不是测试，
应写 `NONE`。本轮已改。

## 已识别、明确不在本 PR 范围（另开 issue）

1. **`authoritative_test` 只被检查存在性与区间合法性，不检查语义** —— 与 `source-anchor` 修之前同类的债。
   本 PR 过程中已实际抓到 **3 条错指**：`CAP-REPORT-SHELL-014`（指 unsupported-block 用例）、
   `INV-REPORT-BACKLINK-010`（指 report.md loading fallback 用例）、`E2E-INV-INFRA-019`（指向非测试的
   helper 自身代码）。另 `app-dataflow.yaml` 有 **5 条**共用 `:1` 的「文件级指针」约定，同属这一类。
2. **`isGeneric()` 只覆盖 4 条候选通路里的 2 条**（`:332` 展示文案、`:352` 反引号整片段；形状扫描
   `:341-345` 与全局散文扫描 `:354-356` 从不查停用表）。量化结果见上一节：补全会翻转 4 条并要把
   `ANCHOR_BASELINE_MAXIMUM` 从 174 抬到 178，超出本 PR 判据。
3. **「statement 的每个分句都要有被引行」这个更强标准的可行性与代价** —— 本轮 codex 通道按此标准对
   6 条中的 5 条提出引用缺口，说明全库大面积不满足；是否采纳、怎么分批迁移，需要单独评估。

另记一条本轮观察、不在范围内：`E2E-INV-INFRA-017` 的「ONLY / never production」是全称否定，原理上
不可能由任何引用区间锁住 —— 当前靠 grep 枚举成立，没有 fail-closed 门禁会在有人往 `main.rs` 加
`/dev/x` 时变红。该条已 `intentional_omission: true` + `authoritative_test: NONE` 如实声明，到此为止。
`web/e2e/helpers/reset.ts:95` 的注释「#250 PR 2: cwd is required」也已随 #1131 过期，属源码注释、本轮不动。

## 门禁（第四轮）

```
cd fe && npx vitest run --project platform-independent
  → Test Files 39 passed | 1 skipped (40)；Tests 912 passed | 1 skipped (913)
cd fe && npx vitest run tools/oracle
  → Test Files 1 passed (1)；Tests 90 passed (90)
cd fe && npm run lint:js       → exit 0
cd fe && npm run typecheck     → exit 0
```

另实跑 `validateOracle(defaultOracleOptions(repoRoot))`：**VIOLATIONS 0**（全库）。
账本对账（实跑读数）：`anchor-baseline.json` **174** 条 = `ANCHOR_BASELINE_MAXIMUM` **174**；
`anchor-pending.json` **30** 条 = `ANCHOR_PENDING_MAXIMUM` **30** = `ANCHOR_PENDING_IDS` 冻结集 **30** 个 id
（集合相等，无多余无缺失）。本轮两个账本一行未动，被改的 5 条均不在两个账本里（即本来就绿、改后仍绿）。
