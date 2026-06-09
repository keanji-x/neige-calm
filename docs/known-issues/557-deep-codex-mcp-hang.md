# 557: Codex Stop Hook Missing Deep Dive

## TL;DR

- `9efd3c95` 与 `cd9d814a` 最后都调用了 `calm_report_write`，且 rollout 都收到 MCP error output；本次证据不支持“最后一次 MCP call 仍 in-flight”。
- WORKING 的关键差异是 final 后有 `hook.codex.stop` 并把 runtime phase 推到 `turn_completed`；STUCK final/task_complete 后 DB 没有 Stop，phase 留在 `turn_running`。
- `calm_report_write` 的锁错误正常经 JSON-RPC error 返回；不像 handler deadlock 或 transport 丢响应。
- Codex turn loop 确实必须等 tool futures drain 后才会跑 Stop；但 0.137 MCP tool 默认 timeout 是 120s，不是无限等待。
- 近端 bug 更像 final 后 Stop hook/ingest/phase-complete 链路丢事件，而不是这次的 MCP write 卡死。

## Q1: STUCK vs WORKING 对比

| 项 | WORKING `9efd3c95` | STUCK `cd9d814a` |
|---|---|---|
| runtime | `handle_state_json.phase=turn_completed` | `handle_state_json.phase=turn_running`, runtime `status=turn_pending` |
| hook 计数 | 15 hooks: 8 Pre, 4 Post, 1 Stop | 46 hooks: 24 Pre, 20 Post, 0 Stop |
| 最后 function_call | `rollout-working.jsonl:55` `calm_report_write`, `call_OF4VKSfO9BRhlZcjKwWvO87g` | `rollout-stuck.jsonl:101` `calm_report_write`, `call_2TuGGMdCNBV0Fz5ja8zoDR0P` |
| MCP 结束 | `rollout-working.jsonl:56`, Err, 12.37ms, DB locked | `rollout-stuck.jsonl:102`, Err, 7.63ms, DB locked |
| tool output | `rollout-working.jsonl:57` has `function_call_output` | `rollout-stuck.jsonl:103` has `function_call_output` |
| final/task | `rollout-working.jsonl:61`, `:63` | `rollout-stuck.jsonl:107`, `:109` |
| DB tail | event 117 Pre `calm_report_write`; event 124 Stop; event 127 `turn_completed` | event 221 Pre `calm_report_write`; events 222-226 items only; no Stop/phase change |

`call_2TuGGMdCNBV0Fz5ja8zoDR0P` is not in-flight in the rollout: it has both `mcp_tool_call_end` and `function_call_output`.
The daemon window has no `cancelled`/`aborted`/`completed` marker for that call, and no exact hit for the STUCK thread id `019eac47-2535-7262-b085-5dd9f44095e1`.

## Q2: `calm_report_write` 是否会返回

- Handler path is straight-line async: role check, arg parse, resolve report, then persist: `crates/calm-server/src/mcp_server/tools/wave_report.rs:146`-`177`.
- The write funnels through `persist_report(...)` and maps all non-forbidden errors to JSON-RPC internal errors: `crates/calm-server/src/mcp_server/tools/wave_report.rs:409`-`427`.
- `persist_report` performs one `write_with_events_typed` transaction, CRDT load/update, card update, and two events: `crates/calm-server/src/wave_report.rs:218`-`311`.
- No obvious in-handler mutex wait or card role cache lock appears on this path; the plausible wait is SQLite/event write contention inside the transaction.
- In both observed rollouts it returned quickly with `database is locked`, so this capture does not show a report-write deadlock.
- `calm_update_wave_state` has the same error-shaping pattern: `write_with_events_typed(...).await.map_err(map_emit_error)` at `crates/calm-server/src/mcp_server/tools/wave_state.rs:268`-`303`; mapper at `:583`-`589`.
- Transport awaits the handler and always writes either ok or error frame for the same request id: `crates/calm-server/src/mcp_server/transport.rs:327`-`349`.
- There is no pending response map in our transport: one per-connection loop serially handles a request, then flushes response; tool errors are not silently swallowed (`transport.rs:443`-`456`).

## Q3: daemon 日志窗口里的 MCP 异常

- Grep on `/tmp/repro-557-evidence/daemon-stderr-window.log` for `mcp.*err|tool_call.*err|aborted|cancelled|panic|timeout` returned 0 lines.
- Exact grep for STUCK identifiers also returned 0 lines: `019eac47-2535`, `cd9d814a`, `call_2TuGGMdCNBV0Fz5ja8zoDR0P`, `calm.report.write`, `calm_report_write`.
- The only MCP-related log noise in the window is generic session startup/auth spans around `2026-06-09T12:06:34Z`, not tied to the STUCK thread.
- Therefore the daemon log provides negative evidence only: no visible MCP panic, timeout, cancellation, or call-level completion marker for the STUCK turn.

## Q4: Codex turn loop 为什么会等 tool

- Stop hook is gated after sampling returns with no follow-up: `if !needs_follow_up` then `run_turn_stop_hooks(...)` at `external/codex/codex-rs/core/src/session/turn.rs:293`-`301`.
- `sample_response` pushes tool futures when output items finish: `turn.rs:1929`-`1935`.
- Before returning from sampling, Codex drains all in-flight tool futures: `drain_in_flight(...).await?` at `turn.rs:2203`.
- `drain_in_flight` waits on `in_flight.next().await`, so a still-running tool prevents sampling from returning and Stop cannot run: `turn.rs:1716`-`1735`.
- MCP tool calls go through `connection_manager.call_tool(... client.tool_timeout ...)`: `external/codex/codex-rs/codex-mcp/src/connection_manager.rs:699`-`703`.
- Codex 0.137 default MCP tool timeout is 120s: `external/codex/codex-rs/codex-mcp/src/rmcp_client.rs:76`-`77`, applied when config lacks `tool_timeout_sec` at `:195`-`198`.
- The rmcp client wraps timed operations with `active_time_timeout` when timeout is `Some(...)`: `external/codex/codex-rs/rmcp-client/src/rmcp_client.rs:910`-`920`.
- So the structural rule is “Stop waits for MCP response/timeout”; with default config that should be bounded to 120s, not indefinite.
- This capture shows the final `calm_report_write` response arrived, so the missing Stop is after tool drain, not at that await point.

## 可执行下一步

- Add one log immediately before and after `run_turn_stop_hooks` in Codex `run_turn`: if STUCK prints “before Stop” but no hook row, hook runtime/bridge ingestion is guilty; if it never prints, sampling did not return despite rollout final/task_complete.
- Add calm supervisor ingest logs for `EventMsg::AgentMessage`, `TaskComplete`, and `codex.hook Stop` with turn id: if rollout has final/task_complete but no ingest after push watermark 48, the shared-daemon stream reader stalled or detached.
- Dump the generated per-card Codex `config.toml` for STUCK and assert `[mcp_servers.calm].tool_timeout_sec`: if it is absent, Codex should use 120s; if it is explicitly disabled/overridden, rerun with a small timeout to see whether missing Stop becomes a timeout error.

## 后置补充(claude 在 codex 出报告之后实测)

把 dispatcher 的 trace 日志 + DB 直接 grep 对照,出现了一个更窄的矛盾点,需要后续刨:

```
docker logs neige-calm-569-server-1 | grep "hook.codex.stop"
2026-06-09T12:07:58.292500Z TRACE dispatcher: dispatcher push: ignoring hook event hook_kind=hook.codex.stop card_id=9efd3c95...
2026-06-09T12:09:02.212500Z TRACE dispatcher: dispatcher push: ignoring hook event hook_kind=hook.codex.stop card_id=cd9d814a...
```

```
sqlite> SELECT id,scope_card FROM events WHERE payload LIKE '%hook.codex.stop%';
124|9efd3c95f85b47ef8b9178f4d1632349    <- WORKING (id=124)
                                         <- STUCK 没有,events MAX(id)=226
```

观察:
- WORKING `9efd3c95` 的 Stop:dispatcher 命中 + DB 行同时存在,自洽。
- STUCK `cd9d814a` 的 Stop:dispatcher 命中 broadcast,**但 events 表里完全没有这条 row**。

按 `crates/calm-server/src/db/sqlite.rs:3917-3956` `log_pure_event` 的 commit-then-emit 不变式,broadcast 必须发生在 DB commit 之后。所以这个不一致只能由下面三类 bug 之一产生:
- B1. 有另一条 `bus.emit_envelope`(或 `bus.emit`) 路径绕过了持久化,**仅 STUCK 这条路径走它**。
- B2. row 在 broadcast 后被某处清掉/回收(events 表没有删除路径,但 sqlite WAL 没 checkpoint 时极端罕见情况下读不到 — 不太可能,因为前后 row id 是连续的 220→221→222→…→226,没空隙)。
- B3. role_gate `enforce_role` 对 STUCK 这次 Stop 触发了不对称的 `tx.rollback() + bus.emit` —— 当前代码看不像,需要重新审 sqlite.rs:3934 那一段在 violation 路径下有没有 leak。

WORKING 同样是 `role=spec` 的卡,Stop 能写进 DB,所以 role_gate **不会**因卡角色一刀切拒绝 spec 自发 Stop。不对称必然在 STUCK 走到 ingest 之前(payload 字段、session_id 校验、idempotency key 撞)或之中。

下一步实证(不写代码改动,只加临时日志):
- 在 `crates/calm-server/src/routes/codex.rs:163-217` `ingest_provider_hook` 顶部和 `log_pure_event` 调用前后各加一条 `tracing::info!`,带 `hook_event_name + card_id + hook_idempotency_key`,重启 dev 容器后**复现 STUCK 一次**,查这条 Stop 走没走完整路径。
- 在 `crates/calm-server/src/db/sqlite.rs:3934-3940` `enforce_role` 失败分支加 `tracing::warn!(?violation, ?actor, ?event)`,看 STUCK Stop 是不是被 role_gate 静悄悄 forbid 掉了(用 `let _ = tx.rollback()`)。
