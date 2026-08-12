# #1057 PR-A 第二轮终审修复报告

## Blocking

- B1：恢复 `app-theme-swap-light-dark-bg`。manifest 现为 52 条；相对 main 的 21 条，新增 32 条，
  仅删除 `app-providers-retry-401-as-400`（已由更准确的 `app-providers-retry-reads-top-level-status`
  替代），没有再删除任何既有防守。52/52 patch 均通过 `git apply --check`。
- 恢复项已实跑：`app-theme-swap-light-dark-bg` 只红
  `E2E-CAP-AXE-005 a direct structured dataset write repaints token consumers`（1 failed / 4 passed）。
- B2：遵守 `docs/oracle/FOLLOWUPS.md:67`“基线只能下降，不能新增或换子类”及
  `fe/tools/oracle/README.md:7`“shrinking baseline, not an exemption”。删除本 PR 新增的
  `INV-APP-112 not-in-file`、`INV-APP-108 range-miss` 两项，并修正 source anchor。
  baseline 现为 218 项，与 main 的 218 项相同（未上涨）；oracle 真实数据测试 59/59 通过。

## Major

- M1：`INV-APP-107` 精确补入 cursor storage best-effort/异常不逃逸契约；reducer 效果顺序拆为
  独立 `INV-APP-113`，`events-reducer-reconnect-before-persist` 改为防守该条目。
- M2：新增 `createBrowserEventComposition`，在 app composition 内构造真实 UnauthorizedChannel，
  `main.tsx` 只调用该 factory。`INV-APP-112` 迁移为有权威测试的真实契约；新增
  `app-browser-composition-silence-unauthorized`，定向实跑只红 T-A3（1 failed / 3 passed）。
- M3：确认 message 的末次 frame sink 与 close 的末次 state sink 后均无续体，删除两处死 epoch 重核；
  同时在设计 §5.3(a) 写明 replay-complete 的 connected sink 后仍有 frame，只有该路径需要 post-sink
  重核，message/close 尾部结构上不可达。重建受影响的变异 patch。

## Minor

1. 删除无行为的 `onerror` 空守卫，并在设计中明确无行为时不安装空 callback。
2. 删除 T-D5b 不精确的 `clearTimeout(expect.anything())`；保留有效的 pending timer 与构造数断言。
3. 保留 `driverFactory` 测试缝：它同时验证工厂只调用一次、返回 driver 身份与共享 cursor 的端到端重连，
   未把单 driver 契约降为单一 mock 调用断言。
4. 保留 10 条 mutation 对 oracle anchor 测试的诚实 expected-red：anchor 是独立门禁，删除这些红会使
   runner 错报实际红集合；本轮已修正新增欠债，未再用 baseline 绕过。后续行号漂移仍须同步 manifest。
5. 保留 `write(null)` 同步 durable reset：null 是低频清除且同步 remove 避免旧 cursor 存活窗口；
   保留 `writtenBeforeAdopt`，因为设计要求 pre-adopt write 更新内存并在随后 adopt 时保持该值。
6. 保留 stop/start 之间的 `retryDelay`：INV-APP-046 明确只在 open 后复位；bounce 延续既有退避，
   避免 snapshot-required 重启绕过 backoff。两项均是明确契约裁决，不静默改语义。

## 验证摘要

- 定向：composition + driver 19/19；oracle + mutation runner 90/90；52/52 patch apply-check。
- 恢复主题变异：E2E-CAP-AXE-005 精确红；新增 401 composition 变异：T-A3 精确红。
- 完整门禁：`OWNERSHIP_BASE_SHA=origin/main npm run lint && npm run build && npm test`（见提交前实跑）。
- 本轮未改 `core/events`；`systems/events` 的生产与契约测试变更均在提交中附精确路径 em-dash 尾注。
