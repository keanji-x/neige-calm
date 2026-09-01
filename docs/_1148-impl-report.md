# #1148 S1+S2 实施报告 —— source-anchor 判据加强

分支 `fix/1148-anchor-strength`。全库 1088 条 oracle 条目。

## S1 — 三条与实现不符的 statement

`web/src/WaveList.tsx:234-240` 只处理 `Delete`，注释明写 Backspace 是编辑键、刻意不做破坏性快捷键；
`web/src/WaveList.test.tsx:638` 有一条测试断言 Backspace 不删除。实现是对的，oracle 文案是错的。

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
落点却是 `specChatItems.ts` 里的形参名 —— 正是这张表要拦的碰巧命中（加表后该条回到原有 baseline 状态）。

**单调性**：新 anchor 只让 `present` 变大，`anchored` 是 `some`，所以绿条目不会因此变红。
实际跑数验证（不是推理）：1088 条里 `ok → 非 ok` 的转移数 **0**。

### 三档数字

| 档 | 条数 |
| --- | --- |
| 重新获得判据（原「抽不出 identifier」静默通过 → 现在有真 anchor） | **35**（34 条直接绿，1 条只引用了 unsupported 扩展名，不产生错误） |
| 新暴露并已逐条处置的红条 | **13**（全部处置为绿，**0** 条进 baseline，**0** 条进 pending） |
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

### 逐条处置清单（13 条新暴露的红条）

| id | 新 anchor | 定性 | 动作 |
| --- | --- | --- | --- |
| `INV-A11Y-010` | `waiting on you` | 锚点漂移：`Sidebar.tsx:58-66` 指的是 `writeExpandedCoves`，与 statement 毫无关系 | source → `Sidebar.tsx:271-290`（`aria-label="Waiting on you"` 分区 + `title={cove ? …}` 那行） |
| `INV-APP-047` | `/api/auth/whoami` | 锚点漂移：只引了 close handler，漏掉真正发探测的 `probeUnauthorized` | source → `events.ts:313-359,558-566` |
| `E2E-INV-INFRA-017` | `/dev/reset` | 锚点漂移：`reset.ts:16-17` 全是注释（注释被扫描器剥离） | source → `reset.ts:31-33`（用 `REPLAY_PORT` 拼 `/dev/reset` 的那行） |
| `E2E-CAP-SHELL-006` | `/calm`、`/calm/` | statement 把 `basepath: '/calm'` 整体塞进反引号，抽不出可匹配片段；且真正定义 basepath 的是 router 不是 spec | statement 改写成 `` `basepath` set to `/calm` ``；source 加 `web/src/app/router.tsx:153-161` |
| `E2E-CAP-SHELL-010` | `Open user menu` | `/calm/settings` 在 spec 里写成转义正则 `/\/calm\/settings…/`，字面不存在；真正可锚的是 accessible name | statement 把 `` `Open user menu` `` 改成 `"Open user menu"`（展示文案），命中 `a11y-keyboard.spec.ts:855` |
| `E2E-INV-CWD-004` | `siblingPrefix`、`claim` | statement 与实现不符：`/work/repository` / `/work/repo` 是示意写法，代码用的是 `/work-${ts}/…` | statement 改成引用真实变量名 `siblingPrefix` / `claim`，示意路径去掉反引号 |
| `E2E-CAP-TERMINAL-006` | `signal_killed` | statement 与实现不符：`signal` 在被引 spec 里只出现在注释；承载 signal 语义的是 `XtermView` 的 `signal_killed` 字段 | statement `` `signal` `` → `` `signal_killed` ``；source → `XtermView.tsx:60-90 terminal-clean-exit.spec.ts:120-127` |
| `E2E-INV-ENV-002` | `echo`（`/etc/services` 天然锚不住） | 锚点漂移：`92-96` 全是注释，真正的 POSIX echo 循环在 99 行 | source → `wheel-wave-switch.spec.ts:92-100`。这条的否定半句（「没有 `/etc/services`」）原理上锚不住，但肯定半句锚住了，无需 pending |
| `CAP-REPORT-SHELL-014` | `Multiple report cards found. Showing the earliest.` | 锚点漂移：没引渲染横幅的 `DuplicateReportBanner` | source 增补 `WaveReportPage.tsx:197-202` |
| `INV-REPORT-BACKLINK-010` | `cites block` | 锚点漂移：没引渲染 `· cites block {id}` 的那段 JSX | source 增补 `WaveReportPage.tsx:704-708` |
| `CAP-SPECCONVO-021` | `No messages yet` / `ask the Spec Agent below.` | 源码把破折号写成 `&mdash;`，整串永远匹配不上 | 由「非 ASCII 段切分 + 每段各自成 anchor」规则解决，条目本身不改，命中 `SpecConversation.tsx:598` |
| `CAP-COVE-NEWWAVE-017` | `PR autoflow` | 同上：`issue → PR autoflow` 里的 `→` 不可匹配，且文案被拆成两个 span | 同上，命中 `Cove.tsx:545` |
| `INV-NEWTASK-ISSUEDEV-020` | `derivedWorkflowInput` | 原理上锚不住：`notes` 是「刻意不存在」的东西，只在注释里出现 | 采用简报的 (a) 路线的等价做法：statement 改成指名**存在**的载体 `` `derivedWorkflowInput` 刻意不带 `notes` 字段 ``，source 从 `369-373` 扩到 `369-374`（`derivedWorkflowInput` 的定义行）。这样同一条不变量由一个正向 anchor 支撑，不需要进 pending |

**没有任何一条进 `anchor-baseline.json` 或 `anchor-pending.json`。** 两个账本本次只减不增。

## 测试与变异验证

新增 fixture 目录 `fe/tools/oracle/fixtures/source-anchor/anchor-classes/`，共 11 个，
**每个目录只有一条 entry**，配一个共享的被引文件 `fixtures/anchor-display-copy.ts`（7 行，每行一种形状，
第 4 行是不含任何 anchor 的诱饵行）。

红向 fixture 之所以能钉住分支：删掉某条抽取分支后，该 statement 一个 identifier 都抽不出，
`identifiers.length === 0` 的捷径会让它静默通过 —— 于是**只有**这条 fixture 变红。

变异验证（先 commit，再 `git apply` patch，读具体是哪条测试红）：

| 变异（注释掉的实现分支） | 变红的测试 |
| --- | --- |
| `wordShaped` / `pathShaped` 整个 `if`（反引号整片段） | `anchor class backtick-path-range-miss is the only violation, as range-miss`、`anchor class backtick-word-range-miss is the only violation, as range-miss`、`extracts display copy and backtick fragments…` |
| 展示文案抽取循环 | `anchor class display-copy-range-miss is the only violation, as range-miss`、`anchor class display-copy-with-identifier produces no violation`、`anchor class display-copy-placeholder produces no violation`、`anchor class display-copy-case-insensitive produces no violation`、`anchor class display-copy-typography-split produces no violation`、`extracts display copy and backtick fragments…` |
| `caseInsensitive` 传参（改为空集） | `anchor class display-copy-case-insensitive produces no violation` |
| `UNMATCHABLE_RUN_PATTERN` 去掉占位符段 | `anchor class display-copy-placeholder produces no violation`、`extracts display copy and backtick fragments…` |
| `UNMATCHABLE_RUN_PATTERN` 去掉非 ASCII 段 | `anchor class display-copy-typography-split produces no violation`、`extracts display copy and backtick fragments…` |
| `isGeneric` 停用表（反引号侧） | `anchor class generic-word-not-anchored produces no violation`、`extracts display copy and backtick fragments…` |
| `isGeneric` 停用表（展示文案侧） | `anchor class generic-display-copy-not-anchored produces no violation` |

另有一条集合相等的元测试 `anchor classes: every fixture directory is exercised, in both directions`，
保证新加 fixture 目录不会漏跑。

## 门禁

```
cd fe && npx vitest run --project platform-independent
  → Test Files 39 passed | 1 skipped (40)；Tests 908 passed | 1 skipped (909)
    其中 tools/oracle/oracle.test.ts 86 passed（改动前 73），含
    `accepts all real oracle data without exceptions`（全库 1088 条零违规）

cd fe && npm run lint:js       → exit 0（eslint --max-warnings=0 + 5 个架构/所有权检查全过）
cd fe && npm run typecheck     → exit 0（tsc -b）
```
