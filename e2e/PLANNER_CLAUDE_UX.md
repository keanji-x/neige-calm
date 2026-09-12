# Real Planner operating Claude TUI

This opt-in usability round sends human goals through the normal Planner REST
input. The real Planner chooses and calls the production terminal tools. The
driver never calls `calm.terminal.*`, simulates Claude, or supplies tool results.
It is separate from automatic Tier 2 cases because real rewind evidence and
post-use feedback require review; collecting a round is not a passing UX test.

## Dedicated host only

Do not execute this round on the shared production host. The standard dev
compose stack mounts the developer home and has broad development capabilities;
it is not a sandbox for running this acceptance on production. Use a dedicated
test account/host with no business data, an isolated checkout, and approved
Codex and Claude authentication/network access. Use the standard stack as-is;
do not broaden the separate forge E2E runner's network allowlist for Claude.

Requirements: the existing Tier 2 prerequisites in [README.md](README.md), Python
3, and the actual authenticated Claude executable on the server container's
login-shell PATH. Provision those before running. Missing Claude, auth prompts,
unsupported provider transcripts, inert Planner, missing evidence and timeouts
are failures, never skips. No login, installation or credential copy is performed
by this driver. CLI versions are collected without reading credential files.

Commit the source under test. The wrapper exports the existing `NEIGE_BUILD_SHA`
build input; `/api/version` must identify that exact commit before a Planner is
created. If Make considers an older binary current, rebuild it with the matching
`NEIGE_BUILD_SHA` before retrying; the driver does not accept a missing or stale
fingerprint. Standard `make dev` builds may take time before the bounded model
turns begin. The wrapper tears down only its generated compose project.

```bash
bash e2e/planner-claude-ux.sh --dedicated-test-host
```

The flag acknowledges the environment requirement; it cannot establish whether
a machine is actually dedicated. The operator must verify that first.

## First round

One newly created production Planner performs these goals in one actual Claude
terminal/conversation, with ordinary text observation as the default:

The goals explicitly permit Claude's workspace-trust confirmation for this
one disposable workspace. They do not permit trusting another folder, changing
OS/account/filesystem permissions, or completing a login flow.

1. Start Claude, submit a short Chinese arithmetic/memory prompt, observe its
   answer rather than the input echo.
2. Type a Chinese prompt with punctuation, correct the unsubmitted number using
   the TUI editor, inspect the draft, then submit exactly once.
3. Use the real `/rewind` menu to preserve the first exchange and replace the
   second; inspect the restored state, edit/resubmit and verify the answer.
4. Interview the same Planner about actual friction, repeated observations,
   necessary screenshots, unclear states, workarounds and missing evidence.

An unsupported scenario still gets a post-use interview when the Planner is
responsive. API/Planner failure preserves partial evidence. In-flight steering,
multiline editing and detach/revisit remain later scenarios; this first round
does not claim to cover them.

## Results and review

Artifacts are private under ignored `e2e-artifacts/planner-claude-*/`: exact
source/server and CLI versions, per-scenario transcript/elapsed time/metrics,
terminal and session identity, interview and `review.json` or `incomplete.json`.
`planner_model_selection` records the card's selection, not a provider-attested
model identity: null follows the installation default. The run endpoint does
not expose the resolved per-turn model. Record that separately before comparing
rounds. Completion requires a current app-server `agentMessage` with
`phase: final_answer` plus a settled Planner phase; unsupported/missing message
phase times out explicitly instead of treating commentary as completion.
Cookie credentials travel over private stdin; credential fields, common token
patterns and URL query strings are redacted. Images retain byte counts/hashes,
not base64. This is not a general secret scrubber: keep the workspace synthetic,
never show credentials in the TUI, and inspect artifacts before sharing them.
No raw server logs are automatically exported by this round.

Exit **3** means **review required**, including incomplete scenario findings.
Exit **1** means infrastructure/protocol failure; exit **2** means invalid usage.
The driver never returns an acceptance pass. Numerical answers and `/rewind`
input are supporting evidence only. Review the actual returned terminal text,
draft correction, menu/restored history, same-session IDs, single submissions
and interview before recording a scenario as accepted. Check terminal launch
and all other tool calls for shell substitution or a simulated response. A tool
error, echoed answer, absent menu proof or replaced session must be resolved.
If text lacks selection evidence, collect and inspect the necessary screenshot
in a subsequent round; this collector's image hashes do not establish selection.

Optional control/input readbacks contribute evidence only when their nested
`observation.status` is `available`; the same terminal/session/text checks apply
to `observation.state`. An unavailable readback contributes no view and remains
separate from the preserved written/unknown receipt. Neither receipt nor fresh
state certifies application completion; the review requirement remains.

For each pain point, cite the scenario and actual tool row, add the smallest
reproduction, make a focused improvement, review it, and repeat the affected
goals and interview. Keep baseline/new source commits in separate artifact
directories. Image/normalized-JSON byte counts and elapsed time are observations; no token or
latency savings are inferred without a comparable measured baseline. Identical
key actions are counted as repetitions, not automatically classified as errors.
Human intervention and token savings are explicitly unmeasured.

The collector requires `structuredContent` on every terminal tool result and
never parses the one-line text summary; servers from this change (#1618
rounds 07/08) onward emit it, and a result without it is an evidence error,
not a skipped row.

`readback_available` and `readback_unavailable` count the corresponding nested
results on completed control/input calls, separately from tool errors. Neither
means the application finished. `observation_refusals` counts input calls
refused by an observation fence: the exact production error strings, plus
completed input calls whose receipt `outcome` is `stale_observation` (#1618
rounds 07/08: nothing written, a fresh observation returned instead of an
error). A release readback whose state carries `text_omitted` instead of `text`
(screen unchanged since the previous observation) is accepted as evidence of
the same terminal/session but adds no observation entry. `requested_key_presses` sums key actions'
`repeat` (default 1); `additional_repeated_key_presses` sums the extra `repeat - 1`
presses. These describe requests, including failed or replayed requests, not
confirmed physical writes. Invalid repeat counts contribute to
`unmeasured_key_press_requests` instead of silently becoming 1. The existing
`repeated_identical_input_actions` metric keeps its original definition.

#1618 wait/drift counters (aggregated per scenario as `wait_summary` in
`review.json` for comparison with rounds 05/06) are each read from a completed
call's own arguments or result, never inferred. `change_wait_requests`: `observe`
calls and `observe: true` readbacks whose arguments say `wait_for: "change"`;
`change_wait_outcomes`: tally of those calls' returned `wait.outcome` (observe
result or readback `observation.state`; failed calls and unavailable readbacks
contribute none); `unsettled_change_waits`: outcome `changed` with `settled:
false`; `elapsed_wait_requests`: explicit `wait_for: "elapsed"` or `wait_ms > 0`
without `wait_for`; `unmeasured_wait_observations`: observations lacking a `wait`
block (older server); `drift_allowed_inputs`: inputs requesting
`allow_output_since_observation`; `drift_observed_inputs`: non-failed input
receipts with `output_since_observation: true`; `implicit_observation_inputs`:
inputs omitting `observation_id` (`observation_id_used` is informational). A
settled or `unchanged` wait outcome, like `application_result: "unverified"`,
is not application completion.

#1620 hook-signal counters (also in `wait_summary`), read the same way:
`signal_wait_requests`: `observe` calls and `observe: true` readbacks whose
arguments say `wait_for: "signal"`; `signal_wait_outcomes`: tally of those
calls' returned `wait.outcome` (`signal` is an ordinary observation, not
application completion); `submit_actions`: input calls whose action type is
`submit` (a `submit` of `/rewind` satisfies the rewind input check like `text`);
`open_with_claim`: `open` calls with `claim: true`; `hooks_seen_observations`:
observations whose `signals.hooks_seen` is true; `signals_observed`: total
`signals.since_previous_observation` entries; `unmeasured_signal_observations`:
observations lacking a `signals` block (older server). The goals tell the
Planner to start Claude with `--settings "$NEIGE_CLAUDE_SETTINGS"`.

## Model-free driver checks

These validate only collection/failure handling; they are not a fake passing
Planner or Claude run:

```bash
bash -n e2e/planner-claude-ux.sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s e2e -p 'test_planner_claude_ux.py' -v
```
