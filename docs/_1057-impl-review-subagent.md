# #1057 PR-A 实现评审（subagent 通道，只读）

范围：`git diff origin/main..HEAD`（`c929d7d0` A1 + `eb1ae762` A2，1013 行）。基准：`docs/_1057-fe-events-live-design.md` v4。
本地实跑（只读门禁，全部在 worktree `.claude/worktrees/1057-pra/fe` 内执行）：

- `npx tsc -b` → 退出 0
- `npx vitest run --project platform-independent --project web-dom` → 89 文件 / **1048 passed**, 1 skipped
- `npx depcruise core web/src` → no violations（192 模块）；`npx eslint web/src/systems/events web/src/app/composition.ts web/src/app/events` → 无输出
- `node tools/ownership/check-readonly-change-requests.mjs` → 0；`node tools/test-tier/check-test-tier.mjs` → 0
- **`test:mutation` 未能运行**（见 Blocking-1，manifest 校验直接抛错）

生产代码本身质量高，§5.2/§5.3/§5.4/§5.5 的 driver 语义**基本逐条落实**。所有 Blocking 集中在**变异证据层**：本 PR 新增的 24 条变异条目**一条都跑不起来**。

---

## Blocking

### B1 变异 manifest 无法通过 `validateManifest`，整个 `test:mutation` 在跑第一条变异前就抛错

**证据**（我在 scratchpad 里只读地 import `fe/tools/mutation/runner.ts` 并调用 `validateManifest` 复现）：

1. `fe/tools/mutation/manifest.json:6`（`events-bridge-reconnect-before-persist-and-post-sink-write`）的 `patch` 含**两个** `diff --git` 头（`core/events/reducer.ts` + `web/src/systems/events/websocket-driver.ts`）。
   `runner.ts:69-73` 的 `parsePatchTarget` 要求 `headers.length === 1`，否则 `throw new Error('patch must contain exactly one structured unified-diff target')`。
   实跑结果：`THROW: patch must contain exactly one structured unified-diff target`。
   顺带：该条 `target` 写的是 `web/src/app/events/event-bridge.tsx`，而 patch 根本不改这个文件 ⇒ 即使头数合法，`run.mjs:66` 的 `target_changed_after_apply` 也会是 false（`patch-noop`）。
2. 剔除该条后再跑，下一条立刻抛：`THROW: app-cursor-read-from-storage: unknown defended contract: design:§5.1(1)`。
   `runner.ts:143-148` 的命名空间白名单只有 `oracle` 与 `arch-rule`（`run.mjs:48` 传入），`Object.hasOwn(namespaces,'design')` 为 false。
   全 manifest 统计：`design:` **19 条**、`event-stream.ts:` **1 条**（`events-driver-stop-before-start-throws`），**全部是本 PR 新增**，全部非法。

⇒ `run.mjs:48` 在 `git status` 干净性检查之前就抛，**24 条新变异一条都没有被执行过**；`expected_red` 全是未经验证的声称。这正是记忆里「Review Cannot Replace Execution」那一条。

**建议修法**：① 把第一条拆成两条单文件变异（reducer 效果顺序 / driver 跨 sink 边界写字段），各自 `target` 与 patch 文件一致；② `defends` 只能用 `oracle:<ID>` 或 `arch-rule:<rule>`。设计 §5.5 已明说「oracle 中没有条目约束 notify 频次 ⇒ `defends` 写 §5.5」——但 runner 不接受该命名空间，**必须二选一**：要么把这些语义写进 `docs/oracle/app-dataflow.yaml` 拿到真 ID，要么给 runner 增加 `design:` 命名空间（属工具改动，需另行论证）。前者更符合 §8 的 oracle 准入条件。

### B2 24 条新 patch 里 17 条 `git apply --check` 失败，1 条本身损坏，2 条 apply 到完全错误的位置

我逐条只读跑了 `git apply --directory=fe --check`（repo root，cwd 与 `run.mjs:22` 一致）：

| 结果 | 条目 |
|---|---|
| **corrupt**（`补丁在第 15 行损坏`，hunk 行数与内容不符） | `app-providers-retry-reads-top-level-status` |
| **apply 失败**（17 条） | `events-driver-drop-subscription` / `-subscribe-first-open-only` / `-handmade-subscription` / `-drop-retry-epoch-guards` / `-drop-clear-retry` / `-remove-backoff-cap-reset` / `-probe-live-close` / `-drop-probe-latch` / `-notify-every-401` / `-let-constructor-throw` / `-old-probe-mutates-new-epoch` / `-drop-all-epoch-protection`、`app-cursor-drop-idle-batching`、`app-composition-split-cursor-store`、`app-bridge-reopen-on-context-change`、`app-adapter-drop-wave-invalidation`、`events-bridge-reconnect-...` |
| **apply 成功但位置完全错**（静默危险） | `events-driver-connect-on-open`：`块 #1 成功应用于 156（偏移 53 行）`——本意是在 `websocket-driver.ts:95-100` 的 `onopen` 里插 `sink.connectionState('connected')`，实际插到了第 156 行（文件只有 155 行，即 `probe()` 之后/类尾）；`events-driver-stop-before-start-throws`：`偏移 86 行`，`throw` 被塞到远离 `stop()` 的位置 |

根因：这些 patch 的 hunk **没有任何上下文行**（形如 `@@ -104,1 +104,0 @@` 后直接跟一行 `-`），而行号相对真实文件普遍偏移 +2 ~ +5（例：manifest 写 `socket.send(...)` 在 104 行，实际 `websocket-driver.ts:99`；`const epoch = ++this.epoch` 写 58，实际 `:56`；`if (this.retryTimer !== null) clearTimeout(...)` 写 73，实际 `:71`）。零上下文 hunk 让 git 无法用内容定位，只能按行号硬碰，要么失败、要么错位命中。

**建议修法**：所有 patch 用 `git diff` 真实产出（带 ≥3 行上下文），不要手写行号。修完后必须**实跑** `setsid npm run test:mutation`（记忆：mutation runner 就地改工作树，期间禁止并发读者）并把 `verdict` 全绿的报告贴进 PR。

### B3 既有条目 `app-providers-retry-401-as-400` 被本 PR 打废，未同步删除/更新

`manifest.json` 中该条（`defends: oracle:INV-APP-059`）的 patch 上下文是旧实现 `if (... 'status' in error && error.status === 401) return false;`，而 `public.tsx:19-25` 已改成读 `error.failure.kind`。实跑 `git apply --check` 结果：失败。新条目 `app-providers-retry-reads-top-level-status` 是它的替代品，但旧条目仍留在 manifest 里 ⇒ 即使 B1/B2 修好，这条也会让整轮变异判决红。

**建议修法**：删除 `app-providers-retry-401-as-400`（其防守面已由新条目覆盖，`defends` 改成 `oracle:INV-APP-059` 即可保留 oracle 追溯）。

### B4 T-B4 的变异被 `closed` 标志吸收，仍是恒绿——正是设计 §11 点名两次的那条

`websocket-driver.contract.test.ts:187-196` 的 T-B4：`start(); sockets[0].close(); stop(); runAllTimers();` —— **全程没有第二次 `start()`**，所以 `this.closed` 一直是 `true`。
对应变异 `events-driver-drop-all-epoch-protection` 只删了 ① `start()` 的 `++this.epoch`（`:56`）② `stop()` 的 `++this.epoch`（`:68`）③ `connect()` 入口的 epoch 分量（`:85` 改为 `if (this.closed) return;`），而**保留**了 `socket.onclose` 里的 `if (this.closed || epoch !== this.epoch) return;`（`:117`）。变异后 close 续体到达时 `this.closed === true` ⇒ 直接 return ⇒ 构造数不变 ⇒ **测试仍绿**。
（推断，未实跑——因 B1/B2 该变异当前根本无法执行；但控制流是确定的，`closed` 单独即可拦截。）

同一问题波及 T-D5a（`:64-76`）与 T-D5b（`:78-88`）：两者也都是 `stop()` 后不再 `start()`，因此**只证明了 `closed` 这条防线，对 epoch 推进不可证伪**。真正覆盖 epoch 的只剩 T-B3（`:173-185`，message 处理器跨 sink 边界）与 T-D11（`:160-171`，probe 续体）。

**建议修法**：把 T-B4 改成设计 §7.2 描述的形状——`start()` → 让 socket 异步 close 留下在途续体 → `stop(); start();`（新 epoch，`closed` 回到 false）→ `runAllTimers()`，断言「第二次 start 之后**不得出现第三条 socket 构造**」。此时 `closed` 防线失效，只有 epoch 能拦住，变异才可证伪。T-D5a/T-D5b 同理补一次 restart。

---

## Major

### M1 T-A1「factory 只构造一个 driver」的断言是空的，而它是 INV-APP-048 oracle 改述的唯一前提

`composition.test.tsx:36` 写的是 `expect(composition.driver).toBeDefined();` —— 对「只构造一个 driver」零证明力，删掉/改成两个 driver 都不会红。
设计 §5.5 明写：「当前组装只有一个 driver，故（实例级闩锁）可接受，但前提写进 oracle，**并由 T-A1 锁住『factory 只构造一个 event driver』**」。

**建议修法**：把 `createEventComposition` 的 driver 构造计数暴露成可观测量（例如注入 `socketFactory` 之外再断言 `stream` 与 `driver` 一一对应），或直接断言「同一 composition 内 `WebSocketDriver` 构造次数 === 1」（用 `vi.spyOn` 不行，class 是直接 import；可改为对 `createEventComposition` 返回值做结构等式 + 一条元测试断言模块内只有一处 `new WebSocketDriver`）。最低限度：删掉这行伪断言，别让它冒充证据。

### M2 §8 准入条件未落地：`docs/oracle/app-dataflow.yaml` 一行未改

diff 中没有任何 `docs/` 改动。设计 §6 明确要求 PR-A 期间：INV-APP-048 改述（§5.5）、`INV-APP-046/047/048` 的 `authoritative_test` 由 T-D6/T-D7/T-D8 **首次填上**；§8「准入条件：§5.5 的 INV-APP-048 oracle 改述须先由 orchestrator 裁决」。
当前 driver 用的是**实例字段** `probeInFlight`（`websocket-driver.ts:37`），而未改述的 INV-APP-048 要求「模块级 in-flight 闩锁」⇒ 实现与在册 oracle 文本不一致，且没有任何 `authoritative_test` 指向新写的契约测试。

**建议修法**：本 PR 内补上 oracle 改述与三条 `authoritative_test`；若 orchestrator 决定放到 PR-B，需在 PR 描述里显式登记该缺口。

### M3 T-D5b 的第二条断言恒真

`websocket-driver.contract.test.ts:87`：`expect(fake.constructionCount - fake.closeCount).toBeLessThanOrEqual(1);`
该测试里构造 2 次、close 3 次（测试手动 close 1 次 + `stop()` close 1 次 + 第二条 socket 未关），差值恒 ≤ 1；把 `stop()` 的 `clearTimeout` 删掉也不会让它翻。真正承担证伪的只有 `expect(clear).toHaveBeenCalled()`（`:86`），而它只证明「调过 clearTimeout」，不证明清的是**那个** handle。

**建议修法**：断言 `clearTimeout` 被以 `stop()` 前记录的 timer id 调用（`clear.mock.calls`），并把「旧 timer 到期不得再开 socket」写成 restart 后的精确构造数断言（见 B4）。

---

## Minor

1. **`write(null)` 绕过 idle 批处理**：`browser-cursor-store.ts:64-69` 在 `value === null` 时**同步** `removeItem`，而 §5.1(1) 说落盘统一走 idle 批处理。行为上安全（store 仍是唯一写者），但这条分支没有任何变异覆盖（`a null write durably resets an adopted cursor`，`browser-cursor-store.test.ts:86-90` 只是正向用例）。
2. **`writtenBeforeAdopt` 是设计外的第三状态**：`browser-cursor-store.ts:45,60-62,75-78` —— 若 adopt 之前发生过 `write()`，`adopt()` 会**跳过**读取持久值。生产路径上 bridge 的 effect 首行就是 adopt（`event-bridge.tsx:48`），该分支不可达；但它是无变异覆盖的额外语义，建议要么删，要么补一条变异。
3. **T-A2 的「socket 构造数」是假的**：`public.test.tsx:72-76` 里 `socketConstructionCount` 只在 `renderEventBridge` 回调里自增，测试中根本没有 socket。真正承担证伪的是 `expect(renderEventBridge).not.toHaveBeenCalled()`（该断言有效，对应变异也确实会红）。建议删掉误导性变量名，或改成驱动真实 `createEventComposition` + fake socket factory，与设计 §7.2「socket 构造数为 0（不只是 since:0）」的字面要求对齐。
4. **`retryDelay` 在 `start()` 时不复位**：`websocket-driver.ts:36,98` 只在 `onopen` 复位。bridge 因 `_snapshot_required` 做 `stop(); start();` 之后，退避会从上次的值继续（最坏 8000ms 起）。INV-APP-046 只规定「open 后重置」，不算违约，但属可疑行为，建议显式在 `start()` 里 `this.retryDelay = 500`。
5. **`public.tsx:41` 依赖数组被加宽**：设计 §5.1 明说「`:51` 依赖数组**不再需要**改动」，实现加了 `cursorStore, previousInstanceId`。`previousInstanceId` 是 render 期常量无害；`cursorStore` 在多处测试里是内联字面量对象（如 `public.test.tsx:10` 的 `noopCursorStore` 是稳定的，但 `providers.browser.test.tsx:16`、`event-bridge.contract.test.tsx:94` 是**每次 render 新建**）⇒ 那些用例里该 effect 每次 render 都重跑。生产侧 `main.tsx:19` 传的是模块级 `events.store`，稳定，无实际影响。
6. **`query.data!` 非空断言**：`public.tsx:63`。`verdict === 'same'` 蕴含 `id !== undefined` 故安全，但可用 `query.data && verdict === 'same' && ...` 消除断言。另：`dbInstanceId === ''` 时旧代码走 `if (!id) return`、新 verdict 会判成 `'switched'/'same'`，属边界语义漂移（服务端不会返回空串）。
7. **死代码**：`websocket-driver.ts:113` 的 `if (this.closed || epoch !== this.epoch) return;` 是 `onmessage` 的最后一条语句，`return` 无副作用；`:115` 的 `onerror` 整体是空守卫。满足 §5.3(c)(2) 的字面要求，但读起来像遗漏了后续逻辑，建议加注释说明「刻意留空以满足 sink 边界重核契约」。
8. **T-C3 的「删 cancel」那一半不可观测**：`browser-cursor-store.test.ts:51` 用的是 `drainIncludingCancelled()`，无论 `clear()` 有没有调 `idle.cancel` 都会执行回调 ⇒ 组合 patch 里真正起作用的只有 flush 的 `cursor === null` 判据。变异整体仍会红，符合设计意图，但「组合两处」的说辞名不副实。

---

## 设计条目落实核对

| 设计条目 | 判定 | 证据 |
|---|---|---|
| §5.1(1) cursor store 单一写者、内存即时、idle 落盘 | **部分落实** | `browser-cursor-store.ts:48-54,70`；`write(null)` 同步 removeItem 绕过批处理（Minor 1） |
| §5.1(2) `adopt()` 前 `read()` 恒 null、`write()` 不落盘 | 已落实 | `:57,60-63`；T-C6 `browser-cursor-store.test.ts:68-73` |
| §5.1(3) 持久值 `{dbInstanceId,cursor}` + 三重校验 | 已落实 | `:25-37`（对象/stamp/非负安全整数）；T-C4 `:55-59` |
| §5.1(4) `clear()` 取消 pending + 清内存 + removeItem，且**不 un-adopt** | 已落实 | `:83-89` 未触碰 `adoptedInstanceId`；`browser-cursor-store.test.ts:80-84` 正面锁住 |
| §5.1 容错解析（web 裸数字 `'123'`）不抛且返 null | 已落实 | `:28-36`；T-C7 `:75-78` |
| §5.1 `adopt()` 在 EventBridge effect 首行 | 已落实 | `event-bridge.tsx:48` |
| §5.1 `dbInstanceId` 不进连接 effect 依赖数组 | 已落实 | `event-bridge.tsx:42-45`（走 ref）、`:68` 依赖仍是 `[stream, syncEventVersion]` |
| §5.1 渲染门禁用 `useState` 惰性初始化取 previous id | 已落实 | `public.tsx:35,61-63` |
| §5.1 cache-bust 改调 `store.clear()`，不再自己 `safeRemove` | 已落实 | `public.tsx:47`；`safeRemove` 已删除 |
| §5.1 `SyncCursorPort` 下移到 systems | 已落实 | `systems/events/cursor-port.ts:1-6`；depcruise 实跑无违规 |
| §5.2 `connected` 仅由 `_replay_complete` 驱动 | 已落实 | `websocket-driver.ts:108-109`；T-D4 断言 open 后仅 `['connecting']` |
| §5.2 `onopen` 保持 `connecting` | 已落实 | `:95-100` 无 `connectionState` 调用 |
| §5.2 `disconnected` 由 `EventStream.stop()` 负责，driver 侧不断言 | 已落实 | driver 全文无 `'disconnected'`；测试无相关断言 |
| §5.3(a) epoch 覆盖 open/message/close/error/timer + probe `.then/.catch/.finally` + `connect()` 入口 | 已落实（代码层） | `:85,92,96,102,110,113,115,117,120,122,132,142,145,151`；但见 B4/M3：**证伪能力不足** |
| §5.3(a) 旧 epoch 不得覆盖新 epoch 字段 | 已落实 | `:92` 旧 epoch 只 `socket.close()`；`:118` 用 `this.socket === socket` 守卫 |
| §5.3(b) `stop()` 清 timer + 摘监听 + close + 幂等 | 已落实 | `:67-82`；T-D5c `:90-93` |
| §5.3(c)① 首次 `sink.*` 前完成自身状态写入 | 已落实 | `start()` `:57-62`；`onclose` `:118` 先于 `:119` |
| §5.3(c)② 每次 `sink.*` 返回后重核 epoch | 已落实 | `:63,110,113,120`；T-B3 `websocket-driver.contract.test.ts:173-185` 真会红 |
| §5.3(d) `start()` 不抛（socket 构造失败转退避） | 已落实 | `:90-91`；T-D10 `:150-158` |
| §5.3(e) StrictMode 双挂载 | **部分落实** | T-B4 断言域仍恒绿（B4） |
| §5.4 每次 open 都发订阅帧（含第二次） | 已落实 | `:99`；T-D2 `:41-52` 断言两次 open 各一帧 |
| §5.4 订阅帧由 `eventSubscriptionFrame()` 构造（必带 `since`） | 已落实 | `:99`；T-D1/T-D3 `:34-39` 断言 `{sub,since:17}` |
| §5.5 401 不改退避参数、继续重试 | 已落实 | `probe()` 与 `scheduleRetry()` 完全解耦（`:121,123`）；T-D9 断言构造数 3 |
| §5.5 in-flight 闩锁 per-epoch | 已落实 | `:37`，`start()`/`stop()` 各重置（`:58,70`），旧续体受 `:151` 守卫 |
| §5.5 跃迁标志跨 epoch 保留，仅 200 回落 | 已落实 | `:38` 不在 start/stop 复位；`:143` 仅 `.then` 置 false；T-D9④ `:145-147` |
| §5.5 probe 不得传 unauthorized channel | 已落实 | `composition.ts:27` → `queries.ts:44-51` `runOperation` 调 `performApiRequest(transport, operation)`（无第三参）⇒ 不触发 `core/api/client.ts:40` |
| §5.5 复用 `whoamiOperation()`、从 factory 注入 | 已落实 | `composition.ts:1,27` |
| §5.5 判据统一 `error.failure.kind === 'unauthorized'` | 已落实 | `websocket-driver.ts:17-21` 与 `public.tsx:19-23` 同一句 |
| §4.1 composition factory 保证 bridge/driver 同一 store 实例 | 已落实（代码），**证据未跑** | `composition.ts:24-30` 单一 `store`；`main.tsx:19,27` 同一 `events.store`；T-A1 `composition.test.tsx:26-37` 有效，但对应变异 apply 失败（B2） |
| §4.1 factory 只构造一个 driver 的锁 | **未落实** | `composition.test.tsx:36` 伪断言（M1） |
| §4.2 新增文件的 `OWNERSHIP-CHANGE` 尾注 | 已落实 | 两 commit message 覆盖 `cursor-port.ts` / `websocket-driver.ts` / `.contract.test.ts` / `fake-socket.ts` / `README.md` / `module-file-inventory.yaml`；ownership 检查实跑退出 0 |
| §6 `retryUnless401` 改读 `failure.kind` | 已落实 | `public.tsx:19-25`；`public.contract.test.tsx:12-16` 用真 `ApiError` |
| §6 `renderEventBridge` 签名加宽为 `(server: ServerVersionInfo)` | 已落实 | `public.tsx:26,44,63`、`main.tsx:22-31` |
| §6 `main.tsx` 退化为「调 factory + 渲染」 | 已落实 | `main.tsx:19,40-51` |
| §6 `wsUrl()` 放在 driver、storage 注入 | 已落实 | `websocket-driver.ts:23-25`、`composition.ts:16,24` |
| §7.1 fake socket：close 异步派发 / 计数可读 / 可注入构造抛错 | 已落实 | `fake-socket.ts:15-19,22-40` |
| §7.2 T-C1..C7 / T-D1..D11 / T-A1/A2 / T-B1..B4 / T-R1 断言存在 | 已落实（断言层） | 三个测试文件；1048 测试全绿 |
| §7.2 变异证据可执行且真会红 | **未落实** | B1/B2/B3/B4 |
| §7.2 必须保持绿的反例（四类 key 丢弃、未知 kind 不抛、27 条 noop） | 已落实 | 既有契约测试未被改动且全绿 |
| §5.6 G1/G2/G3 | 不适用（PR-B） | 无相关改动，无越界 |
| §8 oracle 改述 + `authoritative_test` 回填 | **未落实** | `docs/` 零改动（M2） |
| §7.3 真实栈冒烟两条 | 未见证据 | PR 描述外，本 diff 无法判定；需按 §7.3 实跑并贴输出 |
| 越界/悄悄放宽契约 | 无 | 未发现超出 §3/§8 PR-A 范围的改动；`event-stream.ts` 一行未改（接口冻结成立） |

**结论**：生产实现可以接受（Blocking 全部落在证据层）。修完 B1–B4 + M1–M3 后需**实跑** `setsid npm run test:mutation` 并贴报告，否则本 PR 的 24 条变异等于零证据。
