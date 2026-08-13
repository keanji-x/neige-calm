# #1057 实现评审（Codex）

评审范围：`git diff origin/main..HEAD`（`c929d7d0`、`eb1ae762`）。基准：完整阅读 `docs/_1057-fe-events-live-design.md` 后逐项核对。以下“事实”均来自当前实现；“推断”单独标明。

## Blocking

1. **§5.3(c) 第一条没有实现：`onclose` 在首次 `sink.*` 返回后仍写 driver 自身状态。**
   - 事实：close handler 先清 `this.socket`（`fe/web/src/systems/events/websocket-driver.ts:116-119`），随后调用 `sink.connectionState`（`:119`），但返回后才调用 `probe()` 与 `scheduleRetry()`（`:120-123`）。二者分别写 `this.probeInFlight`（`:138-140`）及 `this.retryDelay` / `this.retryTimer`（`:127-135`）。
   - 事实：每次 sink 返回后的 epoch 重核已存在（`:120`、`:122`），所以 §5.3(c) 第二句已落实；但这不能替代第一句“自身写入须在首次 sink 前完成”。
   - 推断：同步 state handler 可在 `:119` 内重入 stop/start；虽然 `:120` 能阻止旧 epoch 后续写入，但实现形状仍不满足冻结设计，且当前变异只覆盖 message 路径上的 post-sink 写入，没有覆盖 close 路径（manifest `fe/tools/mutation/manifest.json:1-8`）。

2. **T-B4 仍是恒绿变异，未达到 §7.2 的可证伪要求。**
   - 事实：测试只执行 `start → oldSocket.close → stop → runAllTimers`，没有在 stop 后再次 start（`fe/web/src/systems/events/websocket-driver.contract.test.ts:187-195`）。
   - 事实：manifest 变异只取消 epoch 推进及 connect 的 epoch 条件，仍保留 `closed`，并保留 close callback 的 `if (this.closed || ...) return`（manifest `fe/tools/mutation/manifest.json:481-487`；生产守卫见 driver `:116-117`）。
   - 推断：变异后 stop 令 `closed=true`，旧 close continuation 在 `:117` 返回，socket 构造数不变；因此 `:195` 仍通过。设计要求的断言域是 StrictMode 型 `stop(); start()` 后，旧在途续体不得构造新 socket。

3. **T-D9 没有按变异表实现四种变异，401 的“不改退避且继续重试”和“跃迁标志跨 epoch”没有独立变异证据。**
   - 事实：测试同时断言重试构造数与跨 epoch 不重复通知（driver test `:133-148`），但 manifest 只有“每次 401 都 notify”这一种 patch（manifest `:427-433`）。
   - 事实：设计指定的另外三种 patch——401 停止重试、401 改退避上限、跃迁标志改成 per-epoch——均不在 manifest；全文件仅这一条 T-D9 对应项。
   - 推断：当前断言本身不是恒真，但无法证明“继续重试/退避不变/跨 epoch 标志”各自能杀死指定变异，故 §7.2 的验收条件未满足。

## Major

1. **生产 composition 没有把 401 通知接到 `UnauthorizedChannel`，实际通知是 no-op。**
   - 事实：driver 在首次 unauthorized 跃迁调用 `this.onUnauthorized()`（driver `:144-149`）；factory 未传参时默认 `() => undefined`（`fe/web/src/app/composition.ts:18,28`）；`main.tsx` 构造 composition 时没有传 `onUnauthorized`（`fe/web/src/main.tsx:17-20`）。
   - 事实：仓库已有 `createUnauthorizedChannel`（`fe/core/api/unauthorized.ts:15`），但本 diff/生产组装没有使用它。
   - 推断：driver 单测中的 spy 能证明内部跃迁逻辑，却不能证明设计所说的“notify 到 UnauthorizedChannel”；当前浏览器运行时所有 probe 401 通知都会被丢弃。即便本 issue 不实现订阅者，也应组装真实 channel，而不是把发布端消音。

2. **T-D11 的断言域窄于设计：只检查 channel/notify，未检查 epoch-2 latch 与退避。**
   - 事实：T-D11 最终唯一断言是 `notify` 未调用（driver test `:160-170`）。
   - 事实：设计要求旧 probe resolve 后 epoch-2 的 latch/channel/退避均不变；manifest patch也只删 catch 分支 guard（manifest `:445-451`），没有分别使 finally latch 或退避可观测。
   - 推断：当前生产 `.then/.catch/.finally` 都有 epoch guard（driver `:141-153`），实现本身正确；缺陷在验收证据，未来删 `.finally` guard 或污染新 epoch latch 时该断言可能仍绿。

3. **T-A1 没有实现“factory 只构造一个 driver”的断言。**
   - 事实：composition 确实只 `new WebSocketDriver` 一次并把同一 store 注入 driver，随后返回 `{store, driver, stream}`（composition `:24-32`）。
   - 事实：测试的相关断言仅为 `expect(composition.driver).toBeDefined()`（composition test `:35-37`），它不计数构造次数；其有效部分是通过重连 `since:23` 证明 store 同实例（`:26-35`）。
   - 推断：split-store manifest 变异会变红（manifest `:453-460`），但“只构造一个 driver”这一半设计断言未被锁定。

## Minor

1. **T-D3 没有直接证明函数调用，只通过输出形状间接证明。**
   - 事实：T-D1/T-D3 共用断言，比较 `{sub:['*'], since:17}`（driver test `:34-39`）；handmade mutation 改成缺 `since` 的 `{sub: topics}`（manifest `:355-361`），确实会红。
   - 推断：该变异有效、不是恒真；但若手写实现仍产出完全相同形状，测试无法区分是否调用 `eventSubscriptionFrame()`。若“由该函数构造”是源码级契约，需要 spy/source-shape 锁；若只要求协议行为，现状可接受。

2. **T-D5b 的“旧 close work”表述强于测试实际覆盖。**
   - 事实：测试在异步 close 已派发并建立 retry timer 后才 stop/start（driver test `:78-87`），主要锁定 clearTimeout 与存活 socket数；已出队 timer callback 由 T-D5a 单独覆盖（`:64-76`）。
   - 推断：组合覆盖总体合理，但 T-D5b 自身并未保存/直接调用旧 close continuation；报告/manifest 不应把这部分归给单条测试。

## 设计条目落实核对

- **§5.3(a) 所有异步续体 epoch：已落实（实现），测试部分落实。** start/stop 推进 epoch（driver `:55-68`）；connect 入口与构造后复核（`:84-92`）；open/message/error/close 均入口守卫（`:95-117`）；timer 守卫及 connect 二次复核（`:127-135`）；probe `.then/.catch/.finally` 均守卫（`:141-153`）。T-D11 证据范围不足，见 Major 2。
- **§5.3(b) stop：已落实。** closed、清 timer、摘四类监听、再 close，且空 socket 幂等（driver `:67-81`）；T-D5b/T-D5c 有直接断言（test `:78-93`）。
- **§5.3(c) 重入安全：部分落实。** 每次 sink 后复核已实现（driver `:109-113,119-123`）；首次 sink 前完成全部自身写入未实现，见 Blocking 1。
- **§5.3(d) 构造异常：已落实。** socket factory try/catch 后按退避（driver `:89-92`）；T-D10 覆盖（test `:150-158`）。
- **§5.3(e) StrictMode：实现具备 epoch 防护，但验收未落实。** T-B4 变异恒绿，见 Blocking 2。

- **§7.2 driver/composition/bridge：部分落实。** T-D1/2/3/4/5a/5b/5c/6/7/8/10、T-A2、T-B1/2/3 的断言与对应 patch 具有实际交集；T-C3 的组合 patch同时删除 cancel 与 null guard（manifest `:273-280`），且 clear 保持 adopt 状态（store `:83-89`），会红；T-D3、T-C3、T-B1 均非恒真。T-B4 恒绿；T-D9 缺三类 patch；T-D11、T-A1 断言不完整。
- **T-D5a 三处 guard 组合：已落实。** manifest 同时删除 timer 的 closed+epoch compound guard及 connect 入口 guard（manifest `:373-379`）；测试保存已出队 callback并在 stop 后直接调用（driver test `:64-76`）。
- **T-B4 stop 后不得新构造：未落实。** 断言文字正确，场景缺少 stop 后 start，变异被 closed 吸收。
- **T-C3：已落实。** clear 不 un-adopt（store `:83-89`），测试强行 drain 已取消 callback（store test `:48-53`），组合变异会写回 `{cursor:null}` 并变红。
- **T-D3：已落实到协议输出，未落实到源码调用身份。** 当前生产确实调用 helper（driver `:99`），指定缺-since 变异会红。
- **T-B1：已落实。** effect 依赖仅 `[stream, syncEventVersion]`（bridge `:47-68`）；测试用新 context/dbInstanceId 重渲染仍只 start 一次（bridge test `:126-151`）；加 context 的 manifest 变异会红（manifest `:463-469`）。

- **§5.2 状态机：已落实。** onopen 只重置退避并发订阅（driver `:95-100`），不发 connected；只有解码到 replay-complete 才先发 connected（`:106-112`）。driver 没发 disconnected；它由 EventStream.stop 负责（`fe/web/src/systems/events/event-stream.ts:125-129`），新增测试也没有误断言 driver disconnected。

- **§5.5 401：实现已落实，生产通知组装未落实，变异证据部分落实。** 退避在 close 时照常排定（driver `:116-135`），probe 401 只改跃迁状态/通知，不改 delay（`:138-153`）；`unauthorized` 是实例字段且 start/stop 不复位（`:38,55-81`），跨 epoch保留；`probeInFlight` 在每次 start/stop 复位并由旧续体 guard 隔离（`:37,58,70,138-153`）。probe 走 `runOperation(transport, whoamiOperation())`（composition `:27`），而 `runOperation` 调 `performApiRequest` 时没有传 unauthorized channel（`fe/web/src/app/providers/queries.ts:43-48`），故不会走 `core/api/client.ts:40` 的第二通知通路。生产 channel 未接及 T-D9 证据缺口见上。

- **§5.1 cursor store：已落实。** adopt 前 read=null/write 仅内存（store `:43-46,57-63`）；adopt 按实例戳加载（`:72-81`）；解析拒绝 JSON 异常、非对象、错 stamp、非非负安全整数（`:25-36`）；clear 取消 flush、清内存/持久值且不改变 adopted id（`:83-89`）。composition 创建一个 store并同时交给 driver和调用者（composition `:24-32`）；main 将该同一实例交给 bridge（main `:43-50`）。
- **§5.1 渲染门禁/adopt 时机：已落实。** previous id 用 lazy state 固定（providers `:44-46`），切实例时 bridge 不渲染（`:59-64`），bridge effect 首行 adopt、随后 read（bridge `:47-49`）。

- **§4.1 probe 通路：已落实。** systems driver只依赖下移的 cursor port（driver `:1-3`）；probe由 app factory注入且未传 unauthorized channel（composition `:24-29`）。
- **§5.4 粘性订阅：已落实。** 每次 socket onopen 都从当前 store 读取 cursor并用 helper 发帧（driver `:95-100`）；第二次 open 测试覆盖（driver test `:41-52`）。
- **retryUnless401：已落实。** 按 `error.failure.kind` 判 unauthorized（providers `:19-24`），相应契约测试通过。
- **范围/契约：未发现超出 A1/A2 设计切片的功能改动，也未发现修改冻结的 `event-stream.ts`。** inventory 与 README 更新是新增文件登记/能力说明（diff 文件清单所示）；未实施设计 §5.6 的 G1-G4，符合本次两个 commit 明确为 A1/A2。除生产 unauthorized 通知被默认 no-op 外，未见悄悄放宽既有公共接口契约。

验证：针对本改动的 6 个测试文件共 44 tests 全绿；`git diff --check origin/main..HEAD` 通过。绿测不消除上述 mutation 证据缺口。
