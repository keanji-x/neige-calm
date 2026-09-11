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

For each pain point, cite the scenario and actual tool row, add the smallest
reproduction, make a focused improvement, review it, and repeat the affected
goals and interview. Keep baseline/new source commits in separate artifact
directories. Image/normalized-JSON byte counts and elapsed time are observations; no token or
latency savings are inferred without a comparable measured baseline. Identical
key actions are counted as repetitions, not automatically classified as errors.
Human intervention and token savings are explicitly unmeasured.

## Model-free driver checks

These validate only collection/failure handling; they are not a fake passing
Planner or Claude run:

```bash
bash -n e2e/planner-claude-ux.sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s e2e -p 'test_planner_claude_ux.py' -v
```
