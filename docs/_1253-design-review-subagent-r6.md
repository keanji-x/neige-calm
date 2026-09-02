# r6 收敛确认轮 — subagent 通道

> 总判：**fix-then-ship（编辑级）。无 BLOCKER。** 设计本身已收敛：五轮里反复发作的两个病（陈述写宽过载体、不变量绿着骗人）在 r6 都换成了经代码验证成立的证明层；D5 的路径、dormant 恢复、幂等 key 三处裁决均在内核代码里核过，没有新的架构级问题。

## r5 八条的关闭情况：七条真关闭，一条关一半

**B1（首次触发无数据）—— 关闭，且时序担忧不成立。** `create_wave_conversation` 是先 `operation_runtime.wait(&op_id)` 拿到 `Succeeded`，**再**在同一 handler 里调 `send_spec_input`。所以 create 返回时 spec-harness-start 已成功、runtime 行已 active、harness 已注册；紧随的第二次 `/spec/input` 走 `ensure_live_spec_harness` 的 fast path（registry hit）。唯一能撞 `Starting` 503 的窗口是并发 reset / 第二次 create，属可重试类，D5 已归类。路径成立。

**B2（每次触发一行 enqueued）—— 关闭，可证。** `send_spec_input` 在 `harness.observe` 成功后**无条件**写一条 `Event::HarnessUserMessageEnqueued`，每次 POST 恰好一行。永久性是 fail-closed 证明过的：`events_prune.rs` 的 `first_message_dedup_kind_is_never_prunable` 断言该 kind 不在 `EVENTS_PRUNE_KINDS`，并插一条 400 天旧行验证它活过每轮 prune。反例可直接写成 SQL 计数（`tests/spec_card_reset.rs` 已有同型断言）。

**M1（INV-007 写宽）—— 表内已收窄，但 D5 正文还留着一处宽读法**（m6）。

**M2 → INV-011 —— 可证，但钉错了对象**（m1）。

**M3（dormant 恢复）—— 规则站得住，且没有被幂等吃掉。** 专门验了这条最容易变成空规则的：`reset_spec_card_shared` → `run_spec_card_operation` 用 `operation_key: new_id()` + `idempotency_key: None`，**不会**命中 `insert_operation` 的既有 key 短路；profile 按 `card_is_wave_assistant` 保留 `Assistant`、`create_card: None`，assistant 会话卡走 reset 合法。**但两个恢复动作不等价**（m3）。

**M4（409 forever）—— 机制已核实。** `insert_operation`：同 kind+key、hash 相同 → 返回既有 id；hash 不同 → `idempotency_payload_conflict`，而 `retryable_operation_key` 只在 `phase == Failed` 时给 `#N` 逃逸——所以「succeeded 但 hash 变了」确实永久 409。actor 固定有先例（`today.rs` 就是 `{"actor":"kernel",...}`，`Actor` 是裸 newtype，服务端合成路径可直接构造）。

**M5（判据语义）—— 关闭**（写明近似 + 反方向断言）。只剩命名没落地（m4）。

**m1–m4 四条 MINOR —— 全部吸收。**

## MAJOR

**§6 切片表与 §10 仍在教实现者去做两条已被证伪的事。** 切片表写着「有效性风险用 INV-010 的第二个反例证死（点三次 → 一条会话、三次 turn）」——正是 B2 杀掉的断言；同一行还写着「创建/重跑**两条路径**」，与 D5 的「不是两条并列分支」直接矛盾。§10 也还写着「点三次必须真的跑三轮」。

§5.2 是对的，但**切片表才是拆 brief 的载体**——按它派活会写出一条必红（或只能靠时序凑绿）的测试，把刚关掉的 BLOCKER 原样放回实现。

## MINOR

- **m1 INV-011 钉的是内核既有代码。** `derive_wave_conversation_keys` 的确定性已被 `conversation_keys.rs` 的 golden 钉死；本设计**新引入**的是 key builder。照原文写，删掉本设计全部新代码这条照样绿——又是「删掉也不红」的形状。
- **m2 INV-010 正例「三次 → 三行」在首次那次不成立。** 卡不存在时 create 内部先发 bootstrap（一行），紧接着摘要再一行 → 首次触发留下**两行**。
- **m3 「重新提交 spec-harness-start **或** 走 `/spec/reset`」两者不等价，差别用户可见。** `reset_spec_harness_card` 写死 `reset_harness_items: true`——走 reset 会**清空这条会话的 transcript**，而 D5 明说这条会话是「用户要的那个 conversation」。直接提交 start（`reset_harness_items: false`、`force_new_thread: true`、`create_card: None`、新 operation_key）能恢复而不擦历史。把「或」改成裁决。
- **m4 命名没落地。** 建议叫 `report_has_noninitial_content`，但六处仍是 `report_started`。
- **m5 INV-010 证的是「入队」不是「送达」。** enqueued 写在 `observe` 成功之后，而折叠溢出丢弃发生在更后面的 run_loop 里——观测被丢时它仍绿。与 INV-003 一样是可接受的近似，但要写明。
- **m6 D5 的「绕过按钮直接 POST 也拒」还是宽读法**，与收窄后的 INV-007 不一致。
- **m7 key 掺 workspace digest 的副作用没写**：re-point 会派生新会话卡，旧卡 `deletable: false` 会一直留在 Conversations 列表里。

> 后记（r8）：m7 的判断被另一通道推翻并改判——`derive_wave_conversation_keys` 用同一 digest 同时喂 card_id 与 operation_key，所以掺 digest 不是「一个要写明的代价」，而是**直接推翻「只有一条会话」**。r8 改为裸 key + 消掉 payload 变量。

## 结论

把切片表与 §10 那三行改掉（外加 m1–m7 的段落级润色），就是 ship。
