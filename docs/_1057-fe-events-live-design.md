# fe 实时事件通电：WebSocket driver + invalidation 扩面

状态：设计稿 **v4**（已收敛三轮双通道 review，收敛记录见 §11）
上游：#997（前端重写）· `docs/_fe-architecture.md` §4.7 · `docs/_fe-rewrite-plan.md` 阶段 3 切片 B1-4
基线：**`origin/main` = `446baa6c`**（设计期为 `71288fbd`；`446baa6c` 只多一条 #1056「fe-mutation 按 4 片矩阵分片」，已核实不影响任何结论，仅 `run.mjs` 行号 73→75）

> ⚠️ **基线纪律**：主仓当前 checkout 的 `design/fe-rewrite-architecture`（`85091e6c`）与 `origin/main` **已分叉**，
> 其 `web/` 与 `docs/oracle/` 是旧副本。本文所有行号取自只读 worktree `.claude/worktrees/997-c1-today`（= `71288fbd`）。
> 第一轮 review 有一条 Major 就因读了主仓旧副本而误判（§11）。

---

## 1. 一句话

`fe/` 现在是一个**静态快照应用**——只有用户自己发起的 mutation 会刷新界面，agent 侧、终端侧、多标签页的任何写入都看不见。本 issue 把 `/api/events` 真正接上，并修掉 invalidation 在**已建查询面**上的三处实际缺陷。

## 2. 现状事实（已双通道核实）

纯核三件套**已经写好且测过**，缺的只是浏览器侧的实现体与接线：

| 层 | 文件 | 状态 |
|---|---|---|
| core | `fe/core/events/protocol.ts` | ✅ 帧解码、两种 control frame、legacy `*.job_requested` shim |
| core | `fe/core/events/reducer.ts` | ✅ cursor 单调推进/去重、version gate、日志回退检测、snapshot-required |
| core | `fe/core/events/invalidation-plan.ts` | ✅ 50 个 kind 类型级穷尽 |
| systems | `fe/web/src/systems/events/event-stream.ts` | ⚠️ 只有 typestate + `EventStreamDriver` port，**无任何实现** |
| app | `fe/web/src/app/events/event-bridge.tsx` | ⚠️ 逻辑完整，**从未被挂载** |
| app | `fe/web/src/app/events/query-invalidation-adapter.ts` | ⚠️ 只映射 4 种 key |

三条决定性事实：

1. **全仓无一处 `new WebSocket`**（排除 node_modules 与架构 fixture）。`systems/events/README.md:20` 明写"不实现真实 WebSocket、指数退避、unauthorized probe、browser cursor store 或 idle batching；这些是后续 platform adapter"。
2. **`main.tsx:37` 没给 `AppProviders` 传 `renderEventBridge`**，而 `app/providers/public.tsx:55` 是 `{query.data && renderEventBridge?.(...)}`——optional call，静默不渲染。
3. **`SYNC_CURSOR_KEY`（`core/keys/storage.ts:12`）除了 `providers/public.tsx:47` 的 cache-bust `removeItem` 外无人读写**，`SyncCursorPort` 无实现。

### 2.1 已核实的**负面结论**（写在这里，避免下一轮 review 重复挖）

- **fe 与 web 的 `noop` kind 集合完全一致**（27 条：`harness.*` 4、`plugin.*` 2、`workflow.registered`、`plan.updated`、`task.context_*` 2、`workspace.*` 2、`forge.*` 7、`worktree.*` 3、`review.round`、`ratify.*` 2、`proposal.*` 2）。**不存在"fe 写成 noop 而 web 有失效面"的 kind。**
- fe 现有查询面只有 `server-version` / `coves` / `waves` / `wave` / `overlays` / `settings`（`app/providers/queries.ts:53-60`），**确无** `wave-files` / `waves-range` / `wave-backlinks` / `wave-report`。⇒ adapter 丢弃这四类 key 是**正确行为**。
- `runtime.*` 三个 kind 直接用 `findWaveOwningCard` 而非 `derivedWaveId`，是因为其 payload（`core/api/schemas.ts:242-268`）**只有 `card_id` 没有 `wave_id`**，二者等价。**实现者不要"顺手统一"，那会引入回归。**
- **每次连接都以全量失效收尾**：`_replay_complete` ⇒ reducer 发 `invalidate keys:null`（`reducer.ts:46`）⇒ adapter 执行整表 `client.invalidateQueries()`（`query-invalidation-adapter.ts:56-58`）。这条是 §5.1 多标签页风险定级的依据。

## 3. 范围

**做**：WS driver 实现体、browser cursor store、composition factory 接线、invalidation 三处缺陷（§5.6）、`retryUnless401` 修复。

**不做**（各自独立 issue，§10 登记）：登录页 / session gate；`wave-files`/`waves-range`/`wave-backlinks`/`wave-report` **四个查询本身**；终端 PTY WS；dev trace ring buffer（GATE-APP-079 明令本层不带）；`useConnectionState` + 连接状态 UI。

## 4. 冻结面：接口冻结 **与** 路径冻结

### 4.1 接口冻结：不改 `event-stream.ts` 一行

新增 `systems/events/websocket-driver.ts` 实现既有 `EventStreamDriver` port，正是 `README.md:20` 预告的"后续 platform adapter"。driver 需要的额外依赖（cursor 读取、401 探测）**从构造器注入**，不进 `EventStreamConfiguration`。这条通路由 README:16 直接背书：

> platform driver 是 `eventSubscriptionFrame` 出站义务的接收者：**每次连接都必须用当前 topics 与 cursor 构造必带 `since` 的订阅帧**；systems port 只传入原始配置与 URL。

port 里没有 cursor，而契约要求 driver 拿得到"当前 cursor" ⇒ 构造器注入是冻结面预留的通路。

> **这只是"可行通路"，不是类型契约。** port 无法证明 bridge 与 driver 拿到的是**同一个** cursor store 实例；误注入两个实例时类型与全部单测都会通过，而重连会带陈旧 cursor。⇒ §5.1 的 composition factory + §7 的 T-A1 是这条不变量的**唯一**锁。

### 4.2 路径冻结：新增文件同样要 `OWNERSHIP-CHANGE` 尾注

fe 的冻结是**路径级**的。`fe/tools/ownership/validator.ts:82-90` 对每个 commit 的 `commit.paths` 逐条比对 readonly 条目（directory 条目做**前缀**匹配），**新增文件与修改文件一视同仁**：

```
- { path: fe/core/events,            type: directory, readonly: true  }   # :34
- { path: fe/web/src/systems/events, type: directory, readonly: true  }   # :66
- { path: fe/web/src/app/events,     type: directory, readonly: false }   # :50
- { path: fe/web/src/main.tsx,       type: file,      readonly: false }   # :24
- { path: fe/tools/mutation,         type: directory, readonly: false }   # :17
```

尾注格式（`validator.ts:81` 正则，注意是 **em-dash `—`**）：`OWNERSHIP-CHANGE: <path> — <理由> (#NNN)`

⇒ 需要的尾注见 §6「尾注」列。**README:20 的"后续 platform adapter"是设计意图背书，不豁免尾注要求。**

> **变异证据不需要尾注**：`test:mutation` 是**就地改工作树、不提交**，validator 只看 commit paths。T-B3 / T-G3 的 patch 落在 `fe/core/events/` 也不触发。

## 5. 设计决策

### 5.1 D1 — cursor：单一权威、单一写者、fail-closed 实例绑定

**决策四条**：

1. **cursor store 是 `SYNC_CURSOR_KEY` 的唯一写者。** 内存值即时更新（`read()` 同步返回最新值），落盘经 idle 批处理（INV-APP-050）。
2. **实例绑定用后置注入，不用构造参数**：store 提供 `adopt(dbInstanceId)`。**`adopt()` 之前 `read()` 一律返回 `null`，`write()` 只更新内存不落盘**（fail-closed）。调用点见下方「adopt 的唯一正确时机」。
3. **持久值形如 `{ dbInstanceId, cursor }`**，`read()` 只接受「已 adopt 且 stamp 相符 且 cursor 是非负安全整数」，其余一律 `null`。
4. **`clear()`**：取消 pending flush + 清内存 + `removeItem`。`ServerCompatGate` 的 cache-bust 分支改调它，**不再自己 `safeRemove(SYNC_CURSOR_KEY)`**。
   ⚠️ **`clear()` 不得改变 adopt 状态**（不许顺手 un-adopt）。否则 (2) 的「未 adopt 不落盘」会成为第三条防线，把 T-C3 的组合变异吸收成恒绿。

**为什么是 `adopt()` 而不是构造参数**（第二轮 Blocking）：`dbInstanceId` 来自 `fetchVersion()` 的**异步**结果（`providers/public.tsx:9,40`），而 store 在 `main.tsx` 模块顶层**同步**构造（`main.tsx:15-33`，无 await）。两条替代路径都破功：

- 构造时读 `localStorage[DB_INSTANCE_ID_KEY]` —— 那是**上一次**会话的值。首次冷启动该 key 不存在 ⇒ stamp 写成 null ⇒ 首个会话的 cursor 永远被丢弃；且同一会话内发生实例切换时 storage 里仍是旧 id，比对「旧 == 旧」**通过** ⇒ 实例戳对真正危险的那条路径完全无效。
- 等版本查询回来再构造 store —— 与 composition factory 在 render 之前跑冲突。

**`adopt()` 的唯一正确时机：EventBridge effect 的首行**（v3 写"由 `ServerCompatGate` 调用"，两条实现路径都破功，第三轮 Blocking）：

- 放 gate 的 **effect** 里 —— effect **子先于父**，EventBridge 的 effect 已经先执行了 `initialEventState(syncEventVersion, latest.current.cursor.read())`（`event-bridge.tsx:57`）。adopt 晚于 `read()` ⇒ fail-closed 让**首个会话的 cursor 恒为 null**，正好复现了上面刚否决掉的那条缺陷。
- 放 gate 的 **render** 里 —— 对 render 之外构造的对象做写入，是 render 期副作用。

```tsx
// event-bridge.tsx
useEffect(() => {
  latest.current.cursor.adopt(dbInstanceId);   // 严格早于任何 read()
  let state = initialEventState(syncEventVersion, latest.current.cursor.read());
  ...
}, [stream, syncEventVersion]);                // dbInstanceId 走 ref，**不进依赖数组**
```

配套：`renderEventBridge` 签名从 `(syncEventVersion: number) => ReactNode` 加宽为 `(server: ServerVersionInfo) => ReactNode`（`public.tsx:24,37,55`），`EventBridgeProps` 增 `dbInstanceId: string`。
`dbInstanceId` **不得**进依赖数组，否则 T-B1「重渲染换 prop 引用不重开」的语义被稀释。

**渲染门禁（与 `adopt()` 配套，缺一不可）**：React 的 effect 子先于父 —— `renderEventBridge?.()` 渲染在 `ServerCompatGate` 返回的 fragment 里（`public.tsx:55`），所以 EventBridge 的 `configured.start()`（`:71`）会先于 gate 检测实例切换的 effect（`public.tsx:42-51`）执行。仅靠 `clear()` 关不掉这一段：driver 已经带着旧 cursor 打开了 socket。

门禁的判据**必须在 render 期就存在**——`busted` 顶不了这个位（`setBusted(true)` 在 effect 里，`:48`，而切换后的第一次 render 已经发生过了，那次恰恰就是 bridge 会挂载的那次）。且不得在 render 里直接读 storage（读可变外部状态，破坏 render 快照）。**用 `useState` 惰性初始化**：

```tsx
const [previousInstanceId] = useState(() => safeRead(runtime, DB_INSTANCE_ID_KEY)); // 首次 render 只读一次，纯读、StrictMode 幂等
const id = query.data?.dbInstanceId;
const verdict = id === undefined ? 'pending'
  : previousInstanceId !== null && previousInstanceId !== id ? 'switched' : 'same';
return <>{verdict === 'same' && renderEventBridge?.(query.data!)}{children}</>;
```

写 `DB_INSTANCE_ID_KEY`、`client.clear()`、清 IDB、`reload()`、`setBusted` **全部留在既有 effect**（`:42-51`），行为不变。

- **不破坏 INV-APP-001**：bridge 仍由 `ServerCompatGate` 内部的 `renderEventBridge` 产生，只是比现契约晚一轮挂载。
- **不破坏既有 provider 契约测试**：`public.contract.test.tsx` 对 `renderEventBridge` 无任何类型断言；`public.test.tsx:38` 只传 `() => <i>bridge</i>`，忽略参数即可继续绿；`:57-63`（切换 ⇒ 清 cursor + 写新 id）与 `:69-74`（无 previous ⇒ `SYNC_CURSOR_KEY` 保持）语义均不变。
- ⇒ §6 里"`:51` 依赖数组相应更新"**不再需要**：`previousInstanceId` 是 render 期常量。

```
                       ┌── 实例切换 ──▶ effect: clear() + 清 cache/IDB + 写新 id + reload（bridge 全程不挂载）
version verdict ───────┤
                       └── 实例一致 ──▶ 渲染 EventBridge ──▶ effect 首行 adopt(id) ──▶ read() ──▶ start()

core/events/reducer ──persist-cursor──▶ bridge ──write()──▶ cursorStore（内存即时）
                                                                  │ scheduleIdle
                                                                  ▼
                                                    localStorage {dbInstanceId, cursor}
driver（每次 open）──read()──▶ cursorStore ──▶ eventSubscriptionFrame(topics, cursor)
```

**容错解析（必须）**：`web/` 用的是**同一个 key** `calm:sync:cursor`，写入**裸数字字符串**（`web/src/api/events.ts:152,520-533`），且两个应用同源。⇒ `read()` 的解析必须 fail-closed：`JSON.parse` 失败、结果非对象、缺 `cursor` 字段、cursor 非非负安全整数 —— **一律返回 `null` 且不抛**。

**多标签页：已知限制，不在本 issue 解决。** 墓碑/CAS 方案已评估后**放弃**：

- localStorage 无 CAS，两 tab 的写是 last-writer-wins。墓碑只能挡住**本 tab 自己的 pending flush**，而那条已被 `clear()` 的取消动作挡住了 —— 两个机制打同一条路径，跨 tab 那条无人覆盖。v2 声称"已被覆盖"是**虚假声称**，此处撤回。
- 真做仲裁需要持久 epoch + `BroadcastChannel`/`storage` 事件同步，是本 issue 之外的一套机制。
- **后果定级**：跨 tab 复活一个旧 cursor，导致的是重放跳段或一次多余的 `_snapshot_required` 往返。**不会造成 UI 数据缺口** —— 依据是 §2.1 末条：每次连接都以整表 `invalidateQueries()` 收尾，cursor 是重放续传点而非数据完整性保证。真正危险的**跨 DB 实例**场景已被 (2)(3) 覆盖。
- ⇒ §9 单列为已知正确性缺口 + §10 后续 issue。web 同样有此问题。

**类型落点（层级）**：`SyncCursorPort` 现声明在 `app/events/event-bridge.tsx:27`。driver 在 `systems` 层，**不得向上导入 app**（`fe/AGENTS.md:5`）。**这条有门禁**：`fe/.dependency-cruiser.cjs` 的 `systems-no-features-or-app` 规则，且 `tsPreCompilationDeps: true` ⇒ 连 `import type` 也会红。所以下移不是「更干净」，是**不下移就过不了 CI**。⇒ 接口下移到 `systems/events/cursor-port.ts`，bridge 改为向下导入。**实现**（`browser-cursor-store.ts`）**仍留在 `app/events/`**：它只被 factory 构造并作为参数注入 driver，driver 只依赖 systems 的类型，不产生向上依赖。

**被否决**：driver 自己数 `_id`（与 reducer 的 version gate / 回退清零语义分叉）；cursor 进 `EventStreamConfiguration`（一次性冻结 vs 每帧变，且要改冻结面）。

### 5.2 D2 — `connected` 由谁判定

driver 在投递 `replay-complete` 帧前先 `sink.connectionState('connected')`。**绝不在 `ws.onopen` 里发 `connected`**（INV-APP-044）。driver 本来就要 `decodeEventFrame` 才能投递，天然看得见 control frame。

| 时机 | 状态 |
|---|---|
| `start()` | `connecting` |
| `ws.onopen` | 保持 `connecting`（**不是** connected） |
| 收到 `_replay_complete` | `connected` |
| `ws.onclose` 且未被显式 stop | `connecting`（立刻，不等退避结束） |
| `stop()` | `disconnected` —— **由 `EventStream.stop()` 负责**（`event-stream.ts:128`），且它先置 `acceptingDelivery=false`（`:127`），driver 之后再发同名状态会被 `:112` 闸门丢弃。driver 侧为幂等冗余，**不对此写断言** |

> **潜伏项（显式登记）**：`event-stream.ts:126-129` 的 `stop()` **先**广播 `disconnected` **再**调 `driver.stop()`。任何 state 处理器在其中重入 `start()`，会在 driver 尚未 stop 时创建第二条 socket。本 issue 无 state 订阅者（`useConnectionState` 已推迟），故为潜伏风险；`useConnectionState` 落地时必须重新评估。

### 5.3 D3 — 退避归 driver、bounce 归 bridge，以及竞态防护

职责切分：driver 拥有 socket 与退避重试循环（500ms 起、翻倍、**上限 8000ms**、open 后重置为 500，INV-APP-046）；bridge 只做 reducer `reconnect` effect 触发的刻意 bounce。

**driver 必须满足的五条**（每条对应真实竞态）：

- **(a) 内部 epoch**：每次 `start()` / `stop()` 推进内部 epoch。所有会产生行为的回调与所有异步续体——open / message / close / timer，以及 probe 的 `.then`/`.catch`/`.finally` 与任何 `await` 之后的代码——进入时先核对捕获的 epoch；`connect(epoch)` 在创建 socket 前再核对一次。`error` 当前没有行为，因而不安装空回调。旧 epoch 只能关闭自己捕获的 socket，**不得**清空或覆盖新 epoch 的任何字段。
  > 为什么不够用只清 timer：`EventStream` 的 `generation`（`event-stream.ts:105,112`）只过滤**投递**方向，挡不住已排队的旧回调调用 driver 的 `connect()`。
- **(b) `stop()`**：置 `closed` + `clearTimeout` pending 重试 + 摘掉当前 socket 全部监听后再 `close()`；**幂等**，允许 start 前 / 重复调用（`event-stream.ts:20` 明写此义务）。
- **(c) 重入安全（v2 措辞自相矛盾，此处重写）**：reducer 的 `reconnect` effect 在 **`sink.*` 的同步调用栈内**执行（`event-bridge.tsx:61-70` 全同步 ← `event-stream.ts:104-114`）。且 `replay-complete` 路径上**必然有两次** `sink.*` 调用（先 `connectionState` 后 `frame`，§5.2），所以"sink 必须是最后一条语句"不可实现。正确的两条是：
  1. **进入任一 socket 事件处理函数后，所有对 driver 自身状态的写入必须在第一次 `sink.*` 调用之前完成**；
  2. **`sink.*` 返回后若仍有后续行为，必须重新核对 epoch；epoch 已变则立即 `return`，不得再触碰任何 `this.*`。**
     当前只有 replay-complete 的 `connectionState('connected')` 后还会投递 `frame`，所以需要重核；message 的末次
     `frame` 与 close 的末次 `connectionState('connecting')` 后结构上没有续体，不安装不可证伪的尾部空守卫。
- **(d) `start()` 不得抛出**：`new WebSocket(url)` 在 URL 非法 / 协议不匹配时会抛 `SyntaxError`/`SecurityError`。`EventStream.start()` 的抛出路径（`:116-123`）会 `started=false` + `driver.stop()` + rethrow，而**没有任何东西会再 start** ⇒ 静默永久断流。⇒ socket 构造失败必须被 driver 自己捕获，转成 `connecting` + 进入退避。
- **(e) StrictMode**：`main.tsx:36` 是 `<StrictMode>`，dev 下 React 会 mount→cleanup→mount，即 `start(); stop(); start();`。叠加退避 timer 后 dev 环境**天然**走一遍 (a)(b)。

### 5.4 D4 — 粘性重订阅

每次 `ws.onopen` 都必须发 `eventSubscriptionFrame(topics, cursor.read())`（INV-APP-049/055）。topics 来自 `configure()` 冻结的配置，driver 持有整个生命周期。

**这是最阴险的一条**：忘了它 = 连接建立、无任何报错、服务端永远不推送 —— 一条静默僵尸连接。测试必须断言**第二次** open 也发了订阅帧。

### 5.5 D5 — 401 探测：与 web 对齐，不改退避参数

INV-APP-047：close 之前**没有** open 过 ⇒ 升级被拒（axum 把未认证 upgrade 变成普通 HTTP 401，浏览器表现为 close 无 open）⇒ 探测 whoami。live socket 被服务端断开则**不**探测。

**决策（v2 的 60s 降频已撤回）**：探测到 401 后 **只 `notify()` 到 `UnauthorizedChannel`，不改退避参数、不停止重试**。

> ⚠️ **退避行为与 web 一致；通知频次是有意分歧**（v3 写"与 web 完全一致"，失实，第三轮 Major）。`web/src/api/events.ts:551-579` 的 `probeUnauthorized()` 在 401 时**无条件** `fireUnauthorized()`，只有 in-flight 闩锁、**没有**跃迁去重。fe 取"仅跃迁通知一次"是**更好的一条**，但必须写明是分歧，否则实现者会照 web 抄出每次都 notify。

> v1 写"停止重试"（会导致永久静默冻结，因本 issue 无登录页也无状态指示器）；v2 改"60s 降频"，但那**直接违反 INV-APP-046**（statement 明写上限 8000ms），且引出一串新问题：60s 何时回落、与"open 后重置"如何合并、T-D6 与 T-D9 在同一实现里互相矛盾。
> 与 web 对齐同时达成两个目标——**不构成敲击风暴**（8s 间隔，web 已在生产跑了很久）且**cookie 回来后自愈**——并且**不需要任何 oracle 改述**。这是两轮 review 后最干净的解。

**通知频次与两个闩锁的生命周期**（必须写死，否则实现怎么写都能绿）：

| 字段 | 生命周期 | 复位条件 |
|---|---|---|
| **in-flight 闩锁**（防探测风暴，INV-APP-048 改述） | **per-epoch** | 每次 `start()` 推进 epoch 时重建；旧 epoch 的 probe 续体不得触碰（T-D11） |
| **`ok → unauthorized` 跃迁标志**（防重复通知） | **per-driver-instance，跨 epoch 保留** | **只有探测返回 200 才回落到 `ok`** |

跨 epoch 保留是关键：若跟随 epoch 复位，则每次 bridge bounce（`_snapshot_required` 触发的 `stop(); start();`、StrictMode 双挂载）之后的第一次 401 都会重新通知，"只通知一次"在真实路径上失守，而单 epoch 内的测试测不出来。

> ⚠️ **`notify()` 必须由 driver 独家发起**：`core/api/client.ts:40` 是**第二条通路**——`if (error.kind === 'unauthorized') unauthorized?.notify();`，任何经 `performApiRequest` 且传了 channel 的请求都会通知。若探测函数按常规组装成 `performApiRequest(transport, whoamiOperation(), channel)`，则每次 401 都 notify，driver 侧的跃迁闩锁完全不可观测、T-D9③ 恒绿。⇒ **注入的 probe 不得向 `performApiRequest` 传 unauthorized channel。** 今天 `runOperation`（`queries.ts:41`）不传 channel 属侥幸，设计层面必须禁止。后续 session-gate issue 把 channel 接进 `runOperation` 时**必须重估本条**。

> **`defends` 说明**：oracle 中**没有**任何条目约束 notify 频次（`app-dataflow.yaml` 只有 INV-APP-048 讲 in-flight 闩锁）。⇒ T-D9③④ 的 `defends` 写 §5.5，不是 oracle id。

**判据统一**：探测结果一律以 `error.failure.kind === 'unauthorized'` 判定（`core/api/types.ts:25-31` → `queries.ts:32-38` 的 `ApiError`），与 §6 的 `retryUnless401` 修法**用同一句**。

**复用 core 既有件**，不新写 fetch：`core/api/auth.ts:19` 的 `whoamiOperation()` 已存在且零消费者；driver 在 systems 层不能 import app，故探测函数从 factory 注入。

> **oracle 改述申请（唯一一条，措辞已收窄）**：INV-APP-048 要求"**模块级** in-flight 闩锁"，与 `no-module-runtime-state` 门禁直接冲突。
> **改述为**："在当前唯一组装实例内去重；若未来允许多个 stream/driver 并存，须注入共享 probe coordinator。"
> **不声称与原契约能力等价**——原文防跨实例风暴，实例字段只防实例内；当前组装只有一个 driver，故可接受，但前提写进 oracle，并由 T-A1 锁住"factory 只构造一个 event driver"。
> 须写进 `docs/oracle/app-dataflow.yaml`，**由 orchestrator 裁决后 PR-A 方可开工**（§8）。

### 5.6 D6 — invalidation 扩面的真实边界

"39/50 事件 no-op"**不该被当作缺陷数**。按三层口径统计才有意义：pure plan 非空 / adapter 可映射 / **有活跃查询消费者**。§2.1 已核实绝大多数 no-op 正确（对应查询不存在）。注意 `['overlays','card']` 虽能映射（`query-invalidation-adapter.ts:36`），但**仓库内没有 card overlay 的 `useQuery`** ⇒ 也是 dormant；`runtime.*` 真正可观察的修复来自 G1。

| # | 缺陷 | 证据 | 修法 |
|---|---|---|---|
| **G1** | `findWaveOwningCard` **无生产实现**，`InvalidationContext` 未注入 | `event-bridge.contract.test.tsx:140` 只有测试里的 `() => null` | 照 `web/src/app/eventBridge.tsx:257-268`：扫 `client.getQueriesData({queryKey:['wave']})`，找含该 card 的 detail，返回 `key[1]`。⚠️ web 版返回 `string \| undefined`，fe 的 `InvalidationContext` 要求 `string \| null`（`invalidation-plan.ts:14`）。cache 存 `WaveDetailWire`（`queries.ts:104-108`，含 `cards`），`.cards.some(c => c.id === cardId)` 可用 |
| **G2** | write-through 被 adapter 丢弃 ⇒ **INV-APP-030/031（#288 回归锁）在 fe 失效** | `query-invalidation-adapter.ts:41-45` + README:99-104 | 用 `core/domain/cove.ts` 的 `toCove` 做显式 wire→domain 转换后复活，保留 phantom 防护。**需加宽 `QueryCachePort`**（现只有 `invalidateQueries`/`removeQueries`/`clear`，`:20-24`）为 `getQueryData` + `setQueryData`。⚠️ `CacheWrite.value: Cove`（`core/api/schemas.ts:1002`）与 `toCove(wire: CoveWire)`（`core/domain/cove.ts:18-28`）今天**结构等价属巧合**，须加编译期断言 |
| **G3** | wave 派生 helper 与 `wave.report_edited` **都不产 `wave-report` key**，共 **13 处** | web 的 `waveFilesDerivedEventKeys`（`invalidationPolicies.ts:84-99`）三条 return 全带 `wave-report`，被 **12 个 kind** 复用（`:164,168,172,207,215,219,223,227,231,235,242,304`）；**外加**独立的 `'wave.report_edited'`（`:189-197`，CAP-APP-037）。fe 侧 `invalidation-plan.ts:46-48` 的 `waveFiles()` 与 `:103` 的 report_edited 都不产 | 见下方落地形状，改 **13 处** |

**G3 落地形状**（"类型级穷尽"在当前结构下 **TypeScript 写不出来** —— `InvalidationPlan.invalidate` 是 `readonly unknown[][]`，plan 是函数，返回值内容不可类型化）：

```ts
// core/events/invalidation-plan.ts
type WaveFilesDerivedKind =
  | 'runtime.started' | 'runtime.status_changed' | 'runtime.superseded'
  | 'terminal.deleted' | 'codex.hook' | 'claude.hook'
  | 'codex.worker_requested' | 'terminal.worker_requested'
  | 'task.completed' | 'task.failed' | 'task.dispatched' | 'task.gate_result';
export const WAVE_FILES_DERIVED_KINDS = Object.freeze([/* 12 个字面量 */] as const);
```

**主锁 = 运行期集合等式元测试**（不是遍历名单）：

```ts
const actual = new Set(ALL_EVENT_KINDS.filter(k =>
  planFor(k).invalidate.some(key => key[0] === 'wave-report')));
expect(actual).toEqual(new Set([...WAVE_FILES_DERIVED_KINDS, 'wave.report_edited']));
```

两条否决记录（第三轮 Blocking + Major，都已核实）：

- ❌ **编译期断言不能当变异证据**：`fe/tools/mutation/run.mjs:75` 唯一执行的是 `spawnSync('npx', ['vitest', 'run', …])`，`tsc -b` 根本不在变异流程内 ⇒「名单删一项」产生 **0 条红测试**，`expected_red` 无法满足。
- ❌ **遍历名单是自证式断言**：遍历 `WAVE_FILES_DERIVED_KINDS` 只能证明名单内的 kind 都对；删掉一项 ⇒ 循环少跑一轮 ⇒ **恒绿**。

集合等式对全部 50 个 kind 反推，因此 T-G3a（删名单一项）与 T-G3b（某调用点改回 `waveFiles(...)`）**双向都红**。

**附加防线**（非变异证据）：在 `core/events/invalidation-plan.contract.test.ts` 加
`expectTypeOf<typeof WAVE_FILES_DERIVED_KINDS[number]>().toEqualTypeOf<WaveFilesDerivedKind>();`
—— 用仓库既有习惯（该文件 `:1,39` 已在用 `expectTypeOf`），`tsc -b` 会真红。
⚠️ **不要**在生产模块里写 `type _Exhaustive = ...`：`tsconfig.core.json:12` 开了 `noUnusedLocals`，会触发 TS6196 直接编译失败。
`Object.freeze([...] as const)` 满足 `fe/AGENTS.md:21` 的运行时冻结要求（`invalidation-plan.ts:25` 已有先例）。

`wave.report_edited` **单独**一条断言（defends CAP-APP-037）。

**13 处名单已双通道逐一核实、不多不少**：`runtime.started/status_changed/superseded`（`:87-98`，走 `findWaveOwningCard` + `waveFiles`）+ `terminal.deleted`(:124)、`codex.hook`(:128)、`claude.hook`(:129)、`codex.worker_requested`(:130)、`terminal.worker_requested`(:131)、`task.completed`(:132)、`task.failed`(:133)、`task.dispatched`(:135)、`task.gate_result`(:155)（走 `derivedWaveId`）+ 独立的 `wave.report_edited`(:103)。
**`wave.updated` / `wave.lifecycle_changed` / `card.*` 直出字面 `['wave-files', id]`，web 侧同样不带 `wave-report`，不属于本次扩面——未误收。**

> **v2 的两处错误在此修正**：① v2 说"12 处"，漏了 `wave.report_edited` 自己 ⇒ **13 处**；② v2 的 T-G3 断言域是"走 derived 降级链的 kind"，变异却删 `wave.report_edited` 的 key —— 两者**不相交**，变异必然不红。
> **v1 的错误（v2 已纠正，此处保留提醒）**：INV-APP-035 的三级降级在 fe **已经存在**（`invalidation-plan.ts:50-58` 的 `derivedWaveId`），不要重复实现。
>
> **G3 当前 dormant**：`wave-report` 查询不存在，adapter 照样丢弃 ⇒ **不产生任何可观测行为**。仍要现在做：它是 pure 层与 web 的 parity 缺口，report issue 落地时只需在 `mapPlannedQueryKey` 加一行。

### 5.7 被显式推迟的 oracle 条目

| 条目 | 理由 |
|---|---|
| CAP-APP-058（`useConnectionState` + SSR 快照） | 无消费者。连接状态语义（INV-APP-044/045）由 driver 契约测试锁定 |
| INV-APP-043 | 是 web 已知限制的**陈述**，非待实现行为；fe reducer 天然同构。标 `migrated` 并注明"由 reducer 结构保证" |
| INV-APP-019 / 057 | 已 `skipped` / 被 `configure()` typestate 吸收 |

## 6. 文件清单

| 文件 | 层 | 动作 | 尾注 |
|---|---|---|---|
| `fe/web/src/systems/events/websocket-driver.ts` | systems | 新增 | **需** |
| `fe/web/src/systems/events/websocket-driver.contract.test.ts` | — | 新增 | **需** |
| `fe/web/src/systems/events/fake-socket.ts`（测试夹具，见 §7.1） | — | 新增 | **需** |
| `fe/web/src/systems/events/cursor-port.ts` | systems | 新增（`SyncCursorPort` 下移，含 `adopt`/`clear`） | **需** |
| `fe/web/src/systems/events/README.md` | — | 修改（删已实现项） | **需** |
| `fe/core/events/invalidation-plan.ts` | core | 修改（G3，13 处 + `WAVE_FILES_DERIVED_KINDS`） | **需** |
| `fe/web/src/app/composition.ts` | app | **新增**：`createEventComposition({storage, transport, …})` → `{ store, driver, stream }`。无顶层副作用，可被测试 import | 否 |
| `fe/web/src/app/events/browser-cursor-store.ts` (+ test) | app | 新增 | 否 |
| `fe/web/src/app/events/wave-lookup.ts` (+ test) | app | 新增（G1） | 否 |
| `fe/web/src/app/events/query-invalidation-adapter.ts` | app | 修改（G2 + 端口加宽） | 否 |
| `fe/web/src/app/events/query-invalidation-adapter.test.ts` | app | 修改（端口加宽波及全部 recording fake） | 否 |
| `fe/web/src/app/events/event-bridge.tsx` | app | 修改（cursor 类型改为向下导入；`EventBridgeProps` 增 `dbInstanceId`；effect 首行 `adopt()`） | 否 |
| `fe/web/src/app/events/event-bridge.contract.test.tsx` | app | 修改（T-B1 变异目标；cursor 夹具更新） | 否 |
| `fe/web/src/app/events/README.md` | app | 修改（映射表 + 故意不做） | 否 |
| `fe/web/src/app/providers/public.tsx` | app | 修改（`retryUnless401`；**渲染门禁**（`useState` 惰性初始化取 previous id）；cache-bust 改调 `store.clear()`；`renderEventBridge` 签名加宽为 `(server: ServerVersionInfo) => ReactNode`）。**`:51` 依赖数组无需改动** | 否 |
| `fe/web/src/main.tsx` | app | 修改（退化为「调 factory + 渲染」） | 否 |
| `fe/tools/mutation/manifest.json` | tooling | 修改（新增变异条目） | 否 |
| `docs/oracle/app-dataflow.yaml` | — | 修改，逐条见下 | 否 |

**oracle 具体改动**：INV-APP-048 改述（§5.5）· INV-APP-035 statement 修订（其 `source` 行范围 67-96 的实际代码本就含 report key，statement 只提 wave-files）· CAP-APP-037 `pending → migrated` + 补 fe 侧 `authoritative_test` · INV-APP-046/047/048 的 `authoritative_test` 现为 `NONE`，由 PR-A 的 T-D6/T-D7/T-D8 **首次填上** · 其余本 issue 迁移条目状态更新。

`wsUrl()` 放在 `websocket-driver.ts`（读 `location`，systems 层允许；core 禁浏览器 API）。`storage` 由构造器注入，符合 `no-direct-persistence`（`fe/tools/architecture/no-direct-persistence.mjs:47` 只豁免 `core/keys/storage.ts`；`main.tsx:33` 已有同款先例）。

## 7. 测试计划与变异证据

### 7.1 fake socket 夹具契约（PR-A 第一批产物）

T-D5b / T-B4 / T-D10 的红**取决于夹具语义**，必须先定死：

- `close()` **异步**（下一个 macrotask）派发 `close` 事件 —— 否则"旧 socket 的 onclose 仍会被派发"这个前提不成立，去掉 epoch 也只有一条 socket，测试恒绿
- 可手动触发 `error`；构造计数与关闭计数可读；`send` 记录原始字符串
- 可注入"构造即抛"以驱动 T-D10

### 7.2 变异表

变异证据是**结构化 manifest**（`fe/tools/mutation/manifest.json`，现 21 条），每条需 `mutation_id` / `defends` / `target` / `patch`（真实 git diff）/ `expected_red`（精确到测试名）/ `selection_paths` / `why_more_than_one`。下表给 `defends` / `target` / 断言 / 变异四项，`mutation_id` 与 `expected_red` 实现期填。

> ⚠️ `test:mutation` **就地改工作树**，必须 `setsid` 脱离运行，期间禁止并发读者。

| ID | defends | target | 断言 | 必须变红的变异 |
|---|---|---|---|---|
| T-D1 | INV-APP-049 | driver | open 后发出订阅帧 | 删掉 `publishSub()` |
| T-D2 | INV-APP-055 | driver | **第二次** open 也发订阅帧 | 订阅改成只在首次 open 发 |
| T-D3 | INV-APP-049 | driver | 帧**由 `eventSubscriptionFrame()` 构造** | driver 自拼 `{sub}` 省略 `since`。**why_more_than_one**：会连带 T-D1/T-D2 变红 |
| T-D4 | INV-APP-044 | driver | onopen **不**产生 `connected`；`_replay_complete` 才产生 | onopen 里加 `connectionState('connected')` |
| T-D5a | §5.3(a)(b) | driver | 用 fake timers **保存回调引用**，`stop()` 后**直接调用**该引用（模拟已出队未执行）⇒ 不得重连 | **三处组合 patch**：timer 回调里的 `closed`、timer 回调里的 epoch、**`connect()` 入口的 epoch 二次核对**。前两条是 v3 的写法，仍被第三条吸收（§5.3(a) 明写 `connect(epoch)` 建 socket 前再核对） |
| T-D5b | §5.3(b) | driver | `stop(); start();` 后旧 pending timer 到期不得再开 socket；「构造数 − close 数 ≤ 1」；**并断言 cancel 被调用** | 删掉 `stop()` 里的 `clearTimeout` |
| T-D5c | `event-stream.ts:20` | driver | `stop()` 幂等、可在 `start()` 前调用 | 让 `stop()` 在无 socket 时抛错 |
| T-D6 | INV-APP-046 | driver | 退避 500/1000/2000/4000/8000/8000，open 后重置 500 | 上限改无限；删 open 时重置 |
| T-D7 | INV-APP-047 | driver | close **无** open ⇒ 探测；close **有** open ⇒ 不探测 | 去掉 `opened` 判别 |
| T-D8 | INV-APP-048(改述) | driver | 首个 probe pending 时制造第二次 close-before-open ⇒ 仍只探测一次 | 删掉实例闩锁字段 |
| T-D9 | §5.5 | driver | 401 后**退避参数不变**、继续重试；仅跃迁时 `notify()` 一次（epoch1 连续两次 unauthorized 只通知一次）；**④ `stop(); start();` 到 epoch2 后再来一次 401 仍不重复通知**（跃迁标志跨 epoch 保留） | ① 让 401 停止重试（v1 设计）② 让 401 改退避上限（v2 设计）③ 每次探测都 notify ④ **把跃迁标志改成 per-epoch**（③ 单独可能不红——见 §5.5 的 `client.ts:40` 第二通路警告，probe 必须不传 channel） |
| T-D10 | §5.3(d) | driver | `WebSocket` 构造抛错时 driver 不向上抛，按退避重试 | 去掉 driver 的 try/catch |
| T-D11 | §5.3(a) | driver | epoch 1 的 probe pending → stop/start 到 epoch 2 → resolve epoch 1 为 unauthorized ⇒ epoch 2 的 latch/channel/退避**均不变** | 让 probe 续体不核对 epoch |
| T-C1 | §5.1(1) | store | `write()` 后 `read()` **同步**返回新值 | 让 `read()` 走 storage |
| T-C2 | INV-APP-050 | store | 连续 N 次 `write()` 只落盘一次，落最后一个值 | 去掉 idle 批处理 |
| T-C3 | §5.1(4) | store | `write(7)` 排队 → `clear()` → **drain idle** ⇒ storage 为空 | **唯一 patch**：同时删 cancel **与** flush 的 null 判据。前提是 §5.1(4) 已写死「`clear()` 不 un-adopt」，否则被「未 adopt 不落盘」这条第三防线吸收。**不可与「断言 cancel handle 被调用」互换**——后者只证明取消义务，不证明 flush 的 null 防线 |
| T-C4 | §5.1(3) | store | 持久值 stamp 与已 adopt 的实例不符 ⇒ `read()` 返回 null | 忽略 stamp 直接返回数字 |
| T-C5 | — | store | storage 抛异常时：`runAllTimers()` 驱动 flush **不抛**，且内存值不受影响 | 去掉 try/catch |
| T-C6 | §5.1(2) | store | **未 `adopt()` 时 `read()` 返回 null 且 `write()` 不落盘** | 让未 adopt 的 `read()` 直接返回持久数字 |
| T-C7 | §5.1 容错 | store | 喂入 web 写的裸数字 `'123'` ⇒ `read()` 返回 null 且不抛 | 让解析假定对象形状 |
| T-A1 | §4.1 / §5.5 | composition | 投递 id=N 的 event、idle 未执行时触发重连 ⇒ 第二个 socket 发 `since:N`；且 factory 只构造**一个** driver | 给 bridge 与 driver 注入两个不同 store 实例 |
| T-A2 | §5.1 门禁 | providers | 持久值为旧实例 cursor、版本响应新实例 ⇒ **socket 构造数为 0**（不只是 `since:0`） | 把渲染门禁退回 `query.data &&` |
| T-B1 | INV-APP-020 | bridge | 挂载后 driver 恰好一次 `start()`；重渲染换 prop 引用不重开 | 依赖数组加 **`context`**（加 `client` 无效：`event-bridge.contract.test.tsx:131-141` 三次 rerender 的 `client` 引用未变） |
| T-B2 | — | bridge | 端到端：假 socket 推 `card.added` ⇒ `['wave', id]` 被 invalidate | 让 adapter 丢弃 `['wave', …]` |
| T-B3 | §5.1 / §5.3(c) | reducer + driver | 推 `_snapshot_required` ⇒ (a) 恰好一条 socket 存活 (b) 新 socket `since === 0` (c) `client.clear()` 一次；**(d) 在 `connectionState` 处理器里触发 stop/start 后，driver 不得再产生任何投递或字段写入** | ① reducer effect 顺序把 `reconnect` 提到 `persist-cursor` 前（**why_more_than_one**：会连带 reducer 既有契约测试变红）② 把 message handler 的共享字段写入挪到 `sink.frame()` **之后**（锁 §5.3(c)） |
| T-B4 | §5.3(e) | driver | **断言域已改**：让 mount#1 留下一个**在途续体**（fake socket 异步 `close` → 退避 timer → `connect()`），断言「`stop()` 之后不得有任何**新 socket 构造**」 | 删 `start/stop` 的 epoch 推进 + 全部 callback guard。⚠️ v3 的断言「仍恰好一条 socket」**恒绿**：StrictMode 是同步 `start();stop();start();`，即便删光 epoch，正确的 `stop()`（清 timer + 摘监听 + close）仍保证存活 socket 恰好一条 |
| T-G1 | INV-APP-034/035 | wave-lookup | cache 有含该 card 的 wave detail ⇒ `runtime.status_changed` 命中 `['wave', waveId]`；查不到 ⇒ 静默跳过 | `findWaveOwningCard` 恒返回 null（**即当前生产行为**） |
| T-G2a | INV-APP-030 | adapter | write-through 替换该行**且保留其他行** | `setQueryData(key, [updated])` |
| T-G2b | INV-APP-031 | adapter | cache 无该 cove 时 no-op，不造 phantom | 缺失时 push 新行 |
| T-G3ab | §5.6 | plan | **运行期集合等式**：对全部 50 个 kind 反推「plan 含 `wave-report` 的集合」== `WAVE_FILES_DERIVED_KINDS ∪ {wave.report_edited}` | **双向都红**：① 名单删一个 kind ② 某个 derived 调用点改回 `waveFiles(...)`。⚠️ 编译期断言（`expectTypeOf`）**不可作变异证据**：`tools/mutation/run.mjs:75` 只跑 vitest，不跑 `tsc`；「遍历名单」也不行——删一项只是少跑一轮循环，恒绿 |
| T-G3c | CAP-APP-037 | plan | `plan('wave.report_edited')` 含 `wave-report` | 从 `wave.report_edited` 删 `wave-report` |
| T-R1 | — | providers | `ApiError`(401) 不重试；`ApiError`(500) 重试一次 | 恢复读顶层 `error.status`（**当前行为，必须变红**） |

**必须保持绿**（过严守卫的反例）：

- `['wave-files']` / `['waves-range']` / `['wave-backlinks']` / `['wave-report']` 经 `mapPlannedQueryKey` 返回 `null` ⇒ **不产生任何 cache 调用**。不得改成"广失效"
- 未知 / 未来 kind 的帧不抛错（INV-APP-026）
- §2.1 那 27 条 `noop(reason)` 保持 noop

### 7.3 真实栈冒烟（不入 CI，输出贴进 PR）

```
CALM_DEV_AUTOLOGIN=true make dev-fresh      # 注意 CALM_CODEX_HOST_BIN，见 prod runbook
cd fe && FE_API_PROXY_TARGET=http://127.0.0.1:<port> npm run dev
```

1. 在 `/cove/$id` 停留，用另一客户端改某个 wave 标题 ⇒ **界面无需刷新即更新**
2. 手工失效 cookie（清 `calm-session`）⇒ 观察持续按 8s 上限重试且只通知一次；重新登录后**自愈**

两条都必须实跑，不接受"我检查过代码"。

## 8. 切片

**准入条件**：§5.5 的 INV-APP-048 oracle 改述须先由 orchestrator 裁决。

| PR | 内容 | 估算 |
|---|---|---|
| **PR-A** | cursor-port 下移 + cursor store（含 `adopt`/`clear`）+ fake-socket 夹具 + WS driver + composition factory + 渲染门禁 + `retryUnless401` + T-D*/T-C*/T-A*/T-B*/T-R1 | ~900 行（含测试） |
| **PR-B** | G1 + G2（含端口加宽）+ G3（13 处）+ **G4**（见下）+ README/oracle 更新 + T-G* | ~500 行 |

> **G4（从 #1059 移交）**：`harness.item.added` / `harness.phase.changed` / `harness.transcript.cleared` 三条当前是 `noop`，理由是「没有查询消费」。#1059 落地后 fe **已有** `harness-items` 与 `spec-run` 两个查询，该理由不再成立 ⇒ 改为真 plan（`[['harness-items', card_id]]` / `[['spec-run', card_id]]`），并在 `mapPlannedQueryKey` 加两条映射。
> 这是**与 web 的有意分歧**（web 那三条仍是 noop，因为它走 card-topic 直接消费而非 query 失效），PR 描述必须点名。需 T-G4 + 变异证据，且 `core/events` 是 readonly ⇒ 需 `OWNERSHIP-CHANGE` 尾注。
> 移交原因：#1059 实现时顺手做了这一步，但它与 PR-B 同改 `invalidation-plan.ts`，两分支并行会必然冲突；且今天无事件流，#1059 不因剥离而损失功能。

按项目惯例（单 PR 目标 ~1k 行、不鼓励碎片化），PR-A **不预先切分**。若实现期实际超过 ~1100 行，唯一干净的缝是：**A1** = cursor-port + store + `public.tsx`（门禁 + `clear()` + `retryUnless401`）+ T-C*/T-R1（约 300 行，**不接线、零行为变化**）；**A2** = 夹具 + driver + factory + 接线 + T-D*/T-A*/T-B*。**不要**按"driver / 测试"切，那会让变异证据与被测对象分家。

> Codex 通道主张拆成 A1/A2/A3 三个 PR；未采纳，理由如上（碎片化成本高于收益），异议记录在此。

## 9. 风险

| 风险 | 缓解 |
|---|---|
| **双 socket / 状态串味**（§5.3 a-c） | T-D5a/b、T-D11、T-B3(d)、T-B4；实现必须做全续体 epoch + sink 边界重核 |
| **静默僵尸连接**（§5.4） | T-D2 锁第二次 open |
| **实例切换时带旧 cursor 建连**（§5.1） | `adopt()` fail-closed + **渲染门禁**；T-A2 断言 socket 构造数为 0 |
| **cursor 跨标签页复活** | ⚠️ **已知正确性缺口，本 issue 不解决**。后果限于重放跳段 / 多余的 snapshot 往返，不造成 UI 缺口（依据 §2.1 末条）。§10 后续 issue；web 同样存在 |
| `SYNC_CURSOR_KEY` 与 web 同键同源、值形状不同 | fail-closed 容错解析；T-C7 |
| driver 构造抛错致永久断流 | T-D10 |
| 401 后行为 | 与 web 对齐（8s 上限持续重试 + 跃迁通知），自愈；T-D9 |
| `EventStream.stop()` 先广播后 stop 的重入窗口 | 当前无 state 订阅者，§5.2 已登记为**潜伏项**；`useConnectionState` 落地时必须重估 |
| `_snapshot_required` 循环 | reducer 已清 cursor ⇒ 下次 `since:0`。若实测仍循环属**服务端问题**，另开 issue，不在前端加计数器绕过 |
| oracle 改述被当成"降低标准" | §5.5 已明确**不声称能力等价**并写死前提 |

**回滚**：渲染门禁不通过 / factory 不注入 `renderEventBridge` 即回到当前行为。driver 与 cursor store 变成无消费者代码，不影响任何现有路径。

## 10. 后续 issue（本 issue 明确不做）

1. 登录页 / session gate / `UnauthorizedChannel` 订阅者
2. `wave-files` 查询 + Files 树 ⇒ 解锁 ~9 个事件的失效
3. report blocks + `wave-report` 查询 ⇒ 激活 G3
4. `wave-backlinks` 查询、`waves-range` 日历查询、card overlay 查询
5. 终端 PTY WS（`/api/terminals/{id}`，v4 协议）+ xterm
6. `useConnectionState` + 连接状态 UI（落地时须重估 §5.2 潜伏项）
7. **cursor 跨标签页仲裁**（持久 epoch + `BroadcastChannel`/`storage` 事件）
8. **wire 类型生成链**：`fe/core/api/generated/wire.ts` 是 `web/` ts-rs 产物的手工副本，`test:wire` 只是 fe↔web diff，对 `crates/calm-types` 的漂移无感知（且让 `web/` 删不掉）
9. ~~层间方向门禁缺失~~ —— **该条已撤回，系误判**。层序门禁一直存在于 `fe/.dependency-cruiser.cjs`（12 条 forbidden 规则，`lint:depcruise`，CI `ci.yml:527`），且 `tsPreCompilationDeps: true` 覆盖 type-only import。误判原因：只在 `eslint.config.js` 与 `tools/architecture/*` 里找，没跟进 `lint` 脚本的 depcruise 分支。真实缺口是该配置的三个洞（fail-open / styles 出边 / mock import），另立 issue
10. **oracle `app-dataflow.yaml:1194` 与 §5.1(1) 冲突**：该条要求 unauthorized 清理时直接 `removeItem('calm:sync:cursor')`，与"cursor store 是唯一写者"抵触。本 issue 不触发该路径（无 session gate）；session-gate issue 落地时须改走 `store.clear()`
11. **变异 harness 不覆盖 `tsc -b`**：`tools/mutation/run.mjs:75` 只跑 vitest ⇒ §5.6 那条 `expectTypeOf` 编译期断言不能当变异证据（该结论不变）。但**不建议**为此加 typecheck 通道：`tools/architecture/architecture.test.ts:135-141` 的 `core-platform-types` 已经在 vitest 里用 `ts.createProgram` + `getPreEmitDiagnostics` 跑 TS 并断言诊断码 —— 类型级契约**已可被现有 harness 证伪**，缺的只是写成 vitest 用例。加第二通道要改判决核心并跑 21 次全量 tsc，成本高一个量级

## 11. 双通道 review 收敛记录

报告：`_1057-fe-events-live-design-review-{codex,subagent}[-r2|-r3].md`。

| 轮次 | Blocking | Major | Minor | 性质 |
|---|---|---|---|---|
| 第一轮 | 6 | 12 | 12 | 结构性（ownership、层次、重入、G3 范围） |
| 第二轮 | 7 | 13 | 7 | **三处设计走向反转** |
| 第三轮（定向） | 5 | 12 | — | **零反转**，全是"形状不对，换这个写法" |

⇒ 第三轮已无设计层分歧，只剩落地形状，判定**收敛**。

**两轮全部采纳**，除下列三条：

1. **驳回**（第一轮 codex Major #4，"G3 是误判，web 与 oracle 只要求两个 key"）：该结论读的是**主仓旧副本**。权威版本（`71288fbd`）`web/src/app/invalidationPolicies.ts:189-194` 明确发三个 key 含 `queryKeys.waveReport`（PR #1029 `fb9445e6` 引入），oracle CAP-APP-037 statement 同含 `wave-report`（`source: :189-197`）。第二通道基线正确，独立得出相反且更强的结论（12 处，后修正为 13 处）。**根因是 v1 brief 只把 `fe/` 指向 worktree，让 `web/` 与 `docs/oracle/` 落回主仓**；第二轮已加基线纪律。
2. **降级**（第二轮两通道 Blocking，"墓碑挡不住跨标签页复活"）：**问题成立**，v2 的"已被覆盖"是虚假声称，已撤回。但**不采纳** BroadcastChannel/CAS 修法：后果限于重放跳段，不造成 UI 数据缺口（§2.1 末条：每次连接以整表失效收尾），真正危险的跨 DB 实例场景已被实例戳覆盖。降级为已知限制 + 后续 issue。墓碑机制一并删除（不做 CAS 时它只重复 `clear()` 已覆盖的路径）。
3. **不采纳**（第二轮 codex Major 5，PR-A 拆三）：见 §8。

**第二轮改变设计走向的三条**：

- **401 从"60s 降频"改回"与 web 对齐"**：60s 直接违反 INV-APP-046（上限 8000ms），且引出"何时回落""与 open 后重置如何合并""T-D6 与 T-D9 互相矛盾"一串问题。对齐 web 同样自愈、同样无风暴，且**省掉一条 oracle 改述**。
- **`dbInstanceId` 改后置 `adopt()` + 渲染门禁**：v2 的实例戳没定义 `dbInstanceId` 来源，两条候选路径都破功；且 React effect **子先于父**，`clear()` 必然晚于 bridge 的 `start()`。
- **G3 从 12 处改 13 处，"类型级穷尽"改为"名单==联合类型 + 运行期遍历"**：TS 无法对函数返回值内容做类型级证明；v2 的 T-G3 变异落在断言域外必然不红。

**第三轮修正的四类落地形状**（无设计反转）：

- **A `adopt()` 的调用点**：v3 写"由 `ServerCompatGate` 调用"，两条路径都破功（gate effect 晚于 bridge 的 `cursor.read()`；gate render 是副作用）⇒ 下沉到 **EventBridge effect 首行**，`renderEventBridge` 签名加宽。门禁判据改用 `useState` 惰性初始化（`busted` 在 effect 里，来不及）。
- **B 401 的两个闩锁**：v3 称"与 web 完全一致"**失实**（web 每次 401 都 notify），改标为有意分歧；补写 in-flight 闩锁 per-epoch、跃迁标志跨 epoch；并禁止 probe 走 `client.ts:40` 的第二条 notify 通路（否则 T-D9③ 恒绿）。
- **C G3 的锁**：`Expect`/`Equal` 在 fe **不存在**，`noUnusedLocals` 会让 `type _Exhaustive` 直接编译失败；更硬的是**变异 runner 只跑 vitest**（`run.mjs:75`）⇒ 编译期锁**不可能变红**，不能当变异证据。"遍历名单"也是自证式（删一项只是少跑一轮）⇒ 主锁改为**运行期集合等式元测试**。
- **D 三条组合变异仍被吸收**：T-D5a 被 `connect()` 入口的第三次 epoch 核对吸收（需三处组合）；T-B4 被正确的 `stop()` 吸收、**断言恒绿**（需改断言域为"stop 后不得有新 socket 构造"）；T-C3 需先写死 `clear()` 不 un-adopt。

**反复出现的同一类缺陷 —— 升格为流程规则**：**变异被冗余防线对称吸收**。三轮共命中 **11 条**（T-D5 三个方向、T-C3 两次、T-B4 两次、T-G3 两次、T-D3、T-B1、T-C5）。规则：

> 当一个场景有 N 条独立防线时，删任意 N−1 条的变异都会被剩下那条吸收。**写变异证据前必须先枚举该场景的全部防线**，再决定用 N 处组合 patch，还是改为断言各防线各自的**可观测义务**。
> 推论：**"我加了一条变异"不等于"该断言可证伪"** —— 三轮 review 里每一轮都有新写的变异当场被判恒绿。
