# #1057 PR-A 双通道评审修复报告

范围：逐条处理 `_1057-impl-review-codex.md` 与 `_1057-impl-review-subagent.md`。

## Blocking

- Codex B1 / subagent B1：manifest 的 `defends` 已全部改为真实 `oracle:` / `arch-rule:`；新增
  INV-APP-106..112，迁移并回填 INV-APP-046/047/048。多文件变异拆成 reducer 与 driver 两条单文件变异。
- subagent B2：所有相关 patch 均由带上下文的真实 unified diff 重建；本地逐条
  `git apply --directory=fe --check` 结果为 `51/51` 成功、0 失败。
- subagent B3：删除已失效的 `app-providers-retry-401-as-400`；并同步重建另外五条受 provider 重构影响的旧 patch。
- Codex B2 / subagent B4：T-B4 改为 `start → close → stop → start → drain`，断言重启后旧 close 续体不得构造第三条 socket；
  `events-driver-drop-all-epoch-protection` 同时删除 close 的 `closed/epoch` guard。定向实跑该变异时 T-B4 确实红。
- Codex B3：T-D9 现有四条独立变异：401 停止重试、401 改 60s 退避、每次 notify、unauthorized 标志 per-epoch；
  四条均定向实跑且各自由 T-D9 杀死。

## Major

- Codex M1：`main.tsx` 构造真实 `UnauthorizedChannel`，把 driver 的 `onUnauthorized` 发布接入 channel；本 issue 仍不增加订阅者。
- Codex M2：T-D11 现在同时观察新 epoch probe 闩锁、notification 与原有 1000ms 退避；旧 epoch reject 不得污染三者。
- Codex M3 / subagent M1：composition 增加窄 driver factory seam；T-A1 断言 factory 恰好调用一次，且返回的就是 composition driver。
- subagent M2：oracle 已改述 INV-APP-048 为单-driver composition 前提下的实例级 per-epoch 闩锁；
  INV-APP-046/047/048 已回填权威测试并迁移。
- subagent M3：T-D5b 删除恒真的“构造数−关闭数”断言，改为精确构造数，并观察 pending timer 与 clearTimeout handle。

## Minor

- Codex minor 1：T-D3 仍锁协议行为（必须带 `since`），不升级为源码身份契约；oracle INV-APP-049 的语义也是出站帧行为。
- Codex minor 2：T-D5b 的名称与断言收窄为 pending retry；已出队 callback 仍由 T-D5a 独立负责。
- subagent minor 1：`write(null)` 同步 remove 是 durable reset 语义；未改成 idle，因为 null 不是高频 cursor 推进，且同步清除避免旧 cursor 存活窗口。
- subagent minor 2：保留 `writtenBeforeAdopt`，因为设计明确要求 pre-adopt write 更新内存；补入 INV-APP-107 的 fail-closed/adopt 契约。
- subagent minor 3：T-A2 删除“伪 socket”解释；有效断言仍是实例切换时 bridge 构造为 0，变异直接让该断言红。
- subagent minor 4：不在 start 重置 retryDelay；INV-APP-046 明确规定 open 后重置，stop/start 保留退避是既有设计语义。
- subagent minor 5：providers effect 依赖未回退；生产引用稳定，依赖完整性优先，连接 effect 本身仍严格不含 dbInstanceId。
- subagent minor 6：保留由 verdict 蕴含的 `query.data!`；空 dbInstanceId 不属于 server schema 的有效返回。
- subagent minor 7：保留 message 尾部与 onerror 的显式 epoch 重核；它们是 §5.3(a)(c) 的源码级边界，不作为死代码删除。
- subagent minor 8：T-C3 继续用“强制执行已取消 callback”锁第二道 null 防线；组合变异同时删除 cancel 与 null guard，确实会红。

## 生产缺口

- close handler 现先完成 socket 清理、probe latch 与 retry 状态写入，再首次调用 `sink.connectionState`；sink 返回后只做 epoch 重核。
- unauthorized 发布端已接到真实 channel，不再默认静音。

## 门禁实跑

- 关键定向证伪：T-B4 变异红；T-D9 四条变异逐条红。
- 完整 mutation：待本文件首轮提交后运行并回填实际 JSON 摘要。
- 常规门禁：待 mutation 完成后运行并回填。
