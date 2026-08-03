# P8b-1 误加宽 source 逐条判定

范围是 `dbbbf77d^..dbbbf77d` 改写的全部 73 个 source，不使用排除公式。路径 A 逐行核对加宽前区间与 statement；路径 B 不看追加行，从 `authoritative_test` 反查（有测试时）或用 statement 所述行为/调用点反向搜索（`NONE` 时）。两路均命中加宽前区间，追加行不增加契约证据；73/73 均判定回退，HEAD 与加宽前值逐字一致。

| # | id | 被追加位置 | 路径 B 实际来源 | 判定 |
|---:|---|---|---|---|
| 1 | `INV-A11Y-005` | `Cove.tsx:426` | statement 行为反向搜索 | 回退 |
| 2 | `CAP-A11Y-006` | `Sidebar.tsx:224` | statement 行为反向搜索 | 回退 |
| 3 | `GATE-A11Y-028` | `wave-create-browse-cwd.spec.ts:90` | authoritative test `:57` | 回退 |
| 4 | `INV-A11Y-039` | `terminal.tsx:71` | authoritative test `new-terminal-card.spec.ts:17` | 回退 |
| 5 | `INV-A11Y-043` | `codex.tsx:364` | statement 行为反向搜索 | 回退 |
| 6 | `CAP-A11Y-045` | `codex.tsx:269` | statement 行为反向搜索 | 回退 |
| 7 | `GATE-A11Y-046` | `plugin-iframe.tsx:378` | statement 行为反向搜索 | 回退 |
| 8 | `INV-A11Y-047` | `plugin-iframe.tsx:362` | statement 行为反向搜索 | 回退 |
| 9 | `INV-A11Y-048` | `plugin-iframe.tsx:399` | statement 行为反向搜索 | 回退 |
| 10 | `INV-A11Y-060` | `Wave.tsx:117` | statement 行为反向搜索 | 回退 |
| 11 | `INV-A11Y-065` | `Cove.tsx:254` | statement 行为反向搜索 | 回退 |
| 12 | `INV-A11Y-079` | `Dialog.tsx:248` | authoritative test `a11y-keyboard.spec.ts:531` | 回退 |
| 13 | `INV-A11Y-081` | `Dialog.tsx:360` | authoritative test `a11y-axe.spec.ts:382` | 回退 |
| 14 | `INV-A11Y-083` | `Cove.tsx:58` | authoritative test `a11y-keyboard.spec.ts:632` | 回退 |
| 15 | `CAP-A11Y-085` | `Wave.tsx:346` | authoritative test `a11y-keyboard.spec.ts:632` | 回退 |
| 16 | `INV-A11Y-090` | `calm.css:299` | statement 行为反向搜索 | 回退 |
| 17 | `INV-A11Y-091` | `calm.css:308` | authoritative test `color-system-anchor.spec.ts:26` | 回退 |
| 18 | `GATE-A11Y-094` | `calm.css:3521` | statement 行为反向搜索 | 回退 |
| 19 | `INV-A11Y-096` | `Wave.tsx:291` | statement 行为反向搜索 | 回退 |
| 20 | `GATE-A11Y-099` | `a11y-keyboard.spec.ts:236` | authoritative test `:215` | 回退 |
| 21 | `INV-A11Y-108` | `Dialog.tsx:124` | statement 行为反向搜索 | 回退 |
| 22 | `E2E-CAP-INFRA-001` | `playwright.config.ts:63` | authoritative test `:106` | 回退 |
| 23 | `E2E-INV-INFRA-009` | `replay-server.setup.ts:95` | authoritative test `:118` | 回退 |
| 24 | `E2E-INV-INFRA-019` | `reset.ts:118` | authoritative test `:105` | 回退 |
| 25 | `E2E-CAP-WAVECREATE-002` | `wave-create.spec.ts:66` | authoritative test `:82` | 回退 |
| 26 | `E2E-INV-WAVECREATE-005` | `wave-create-auto-match.spec.ts:88` | authoritative test `:142` | 回退 |
| 27 | `E2E-CAP-WAVECREATE-008` | `wave-create-new-cove.spec.ts:21` | authoritative test `:49` | 回退 |
| 28 | `E2E-CAP-WAVECREATE-012` | `wave-create-conflict.spec.ts:1` | authoritative test `:131` | 回退 |
| 29 | `E2E-CAP-WAVECREATE-015` | `wave-create-browse-cwd.spec.ts:110` | authoritative test `:119` | 回退 |
| 30 | `E2E-CAP-CWD-009` | `a11y-cwd-resolve.spec.ts:57` | authoritative test `:70` | 回退 |
| 31 | `E2E-INV-CWD-010` | `a11y-cwd-resolve.spec.ts:328` | statement 行为反向搜索 | 回退 |
| 32 | `E2E-INV-MODAL-014` | `a11y-keyboard.spec.ts:626` | authoritative test `:629` | 回退 |
| 33 | `E2E-CAP-REPORT-005` | `a11y-axe.spec.ts:284` | authoritative test `:287` | 回退 |
| 34 | `E2E-CAP-TERMTHEME-002` | `tui-theme-protocol.spec.ts:260` | authoritative test `:184` | 回退 |
| 35 | `E2E-INV-TERMTHEME-006` | `new-terminal-osc-echo.spec.ts:25` | authoritative test `:238` | 回退 |
| 36 | `E2E-INV-WHEEL-005` | `wheel-wave-switch-routing.spec.ts:146` | authoritative test `:91` | 回退 |
| 37 | `E2E-INV-WHEEL-006` | `wheel-wave-switch-routing.spec.ts:142` | authoritative test `:149` | 回退 |
| 38 | `INV-CARD-001` | `XtermView.tsx:1045` | authoritative test `XtermView.test.tsx:437` | 回退 |
| 39 | `CAP-CARD-058` | `XtermView.tsx:776` | authoritative test `XtermView.test.tsx:1154` | 回退 |
| 40 | `INV-CARD-088` | `registry.ts:437` | authoritative test `registry.test.tsx:558` | 回退 |
| 41 | `GATE-CARD-155` | `wheelRouter.ts:2` | authoritative test `wheelRouter.test.ts:421` | 回退 |
| 42 | `INV-CARD-211` | `UnknownCard.tsx:21` | authoritative test `UnknownCard.test.tsx:33` | 回退 |
| 43 | `INV-CARD-221` | `useWaveFsViewer.ts:4` | statement 行为反向搜索 | 回退 |
| 44 | `INV-CARD-228` | `wave-file-tree.tsx:6` | authoritative test `wave-file-tree.test.tsx:119,195` | 回退 |
| 45 | `GATE-TOKENS-007` | `calm-tokens.test.ts:463` | authoritative test `:462-471` | 回退 |
| 46 | `GATE-TOKENS-031` | `calm-tokens.test.ts:908` | authoritative test `:908-928` | 回退 |
| 47 | `GATE-STATE-006` | `no-react-state-hook-members.cjs:31` | statement 行为反向搜索 | 回退 |
| 48 | `GATE-STATE-009` | `no-persistent-in-usestate.cjs:1` | statement 行为反向搜索 | 回退 |
| 49 | `GATE-WIRE-002` | `schemas.test.ts:220` | authoritative test `:220-224` | 回退 |
| 50 | `GATE-WIRE-005` | `schemas.ts:926` | authoritative test `schemas.test.ts:91-102` | 回退 |
| 51 | `INV-SPECCONVO-012` | `SpecConversation.tsx:488` | authoritative test `SpecConversation.test.tsx:1225,1238` | 回退 |
| 52 | `INV-SPECRUN-005` | `useSpecCurrentRun.ts:150` | authoritative test `useSpecCurrentRun.test.tsx:196,210,227,246` | 回退 |
| 53 | `CAP-SIDEBAR-016` | `Sidebar.tsx:323` | statement 行为反向搜索 | 回退 |
| 54 | `INV-DIRPICK-001` | `DirectoryPicker.tsx:148` | authoritative test `DirectoryPicker.test.tsx:68,97` | 回退 |
| 55 | `CAP-DIRPICK-014` | `NewTaskForm.tsx:523` | authoritative test `DirectoryPicker.test.tsx:686` | 回退 |
| 56 | `INV-SCHEMAFORM-001` | `SchemaForm.tsx:14` | statement 行为反向搜索 | 回退 |
| 57 | `INV-DUP-010` | `Sidebar.tsx:151` | authoritative tests `Cove.test.tsx:286` / `Sidebar.test.tsx:85` | 回退 |
| 58 | `INV-UI-DIALOG-034` | `Dialog.tsx:55` | statement 行为反向搜索 | 回退 |
| 59 | `INV-UI-MENU-038` | `Menu.tsx:142` | authoritative test `Menu.test.tsx:117` | 回退 |
| 60 | `INV-UI-CONFIRM-064` | `ConfirmDialog.tsx:1` | authoritative test `ConfirmDialog.contract.test.tsx:54` | 回退 |
| 61 | `INV-UI-CONFIRM-065` | `ConfirmDialog.tsx:58` | authoritative test `ConfirmDialog.contract.test.tsx:220` | 回退 |
| 62 | `INV-UI-CONFIRM-070` | `ConfirmDialog.tsx:40` | statement 行为反向搜索 | 回退 |
| 63 | `INV-A11Y-014` | `Cove.tsx:76,58,91` | statement 反查排序实现 `:99,168-189` | 回退 |
| 64 | `INV-A11Y-019` | `Wave.tsx:117` | statement 反查默认视图分支 `:252-266` | 回退 |
| 65 | `INV-A11Y-035` | `Cove.tsx:169` | locator 契约反查两个 select 区间 | 回退 |
| 66 | `INV-A11Y-057` | `Wave.tsx:117,252-294` | statement 反查 Tab 顺序 `:295-433` | 回退 |
| 67 | `INV-A11Y-059` | `WaveRow.tsx:20`、`Cove.tsx:111-115` | statement 正典实现反查 `WaveRow.tsx:44-65` | 回退 |
| 68 | `CAP-A11Y-063` | hook `:196`、Menu `:37` | authoritative test `a11y-keyboard.spec.ts:316` | 回退 |
| 69 | `CAP-A11Y-069` | `WaveList.tsx:1` | statement 反查 Delete/Backspace 分支 `:205-225` | 回退 |
| 70 | `CAP-A11Y-070` | `WaveList.tsx:91` | statement 反查 roving Tab 分支 | 回退 |
| 71 | `INV-APP-012` | `providers.tsx:178` | authoritative test `cache-bust-db-instance-id.test.tsx:267` | 回退 |
| 72 | `GATE-OVERLAY-001` | `useOverlayState.ts:92` | type test `useOverlayState.test-d.ts:28-47` | 回退 |
| 73 | `CAP-A11Y-067` | `WaveList.tsx:91` | statement 反查 Home/End 分支 `:200-230` | 回退 |

回退后未通过机器锚点的条目不再扩 source；统一登记到 `ANCHOR-NONE.md`，保留上述人工双路径结论。
