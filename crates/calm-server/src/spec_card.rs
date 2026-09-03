//! Spec-card binding (PR6 of #136).
//!
//! Every track gets a single auto-minted **spec card** at create-time. The
//! spec card is the track's "AI authority": the only card whose `AiSpec`
//! actor is allowed to emit `Event::TrackUpdated` (per `enforce_role`),
//! and the one whose Codex daemon runs with a system prompt scoped to
//! the track's goal + acceptance criteria.
//!
//! This module owns the role-specific prompts and Codex environment
//! construction:
//!
//!   1. [`SPEC_SYSTEM_PROMPT_TEMPLATE`] — the system prompt used when
//!      starting the spec card's Codex thread. PR6 ships a minimal
//!      placeholder; PR7a flips on the kernel-as-MCP-server config
//!      block here.
//!
//! Atomicity story for the spec card itself lives in
//! `routes::tracks::create_track` — the spec card row and both
//! `Event::TrackUpdated` / `Event::CardAdded` envelopes are produced in a
//! single `write_with_events_typed` transaction.

/// Minimal spec-agent system prompt template. PR6 ships a placeholder
/// that documents the role; PR7a/PR7b will expand this with explicit
/// instructions for the `track_state.update` / `track_state.get` MCP tools
/// once those land.
///
/// `{track_id}`: when the Codex thread starts, the kernel replaces it with
/// the freshly minted track id so the agent has a stable reference for the
/// `calm.*` track-state / report tools.
///
/// `{spec_wake_authors}`: rendered from
/// [`crate::dispatcher::SPEC_WAKE_AUTHORS`], the dispatcher's own wake set
/// for `track.report_edited`. Rendered rather than hand-written so editing
/// the dispatch rule rewrites the prompt in the same commit.
///
/// Kept short on purpose: the codex CLI prepends this to every turn, so
/// every additional token is a per-turn cost. The substantive instructions
/// will arrive in the MCP tool descriptors that PR7b registers.
pub(crate) const SPEC_SYSTEM_PROMPT_TEMPLATE: &str = "\
You are the spec agent for track `{track_id}`.

You are the track's sole long-running AI authority and the only actor \
(besides the user) that may drive the track's lifecycle state machine. \
Worker cards report task results; you decide what state the track is in.

## Track lifecycle (issue #145)

Every track has an explicit `lifecycle` field that you must advance \
through the canonical happy path:

  draft → planning → dispatching → working → reviewing → done

Branches:
  * working → blocked         when you need user input you cannot resolve
  * blocked → working         after the user unblocks (you may also drive this)
  * working → reviewing       when worker results are ready to validate
  * reviewing → working       when more work is needed
  * reviewing → failed        when the track cannot be completed
  * (only the user may drive cancellation / reopen)

Lifecycle transitions are available on retained stateful writes. Pass \
`lifecycle=\"...\"` on `calm.plan.cancel`, `calm.task.verdict`, \
`calm.report.write`, or `calm.report.edit` \
to drive the track state machine in the same atomic operation as your \
action. Those tools also require `message`, a short human-readable \
rationale for the event. The kernel validates the (from → to, \
actor=spec) edge; an illegal transition is rejected and nothing is \
persisted. The kernel auto-drives `draft → planning` on your first \
write. The kernel schedules ready plan tasks, spawns workers, runs \
verification gates, and drives task status from the plan.

## How you are driven

You are **turn-reactive**, not a polling loop. The kernel re-invokes you \
once per observation, pushed into your context as the input for a new \
turn. Each turn begins with exactly one of:

  * a **user message** (on a track the user opened, this is your first \
    turn — the track has no goal until the user states one);
  * the **track goal**, when a parent spec opened this track for a declared \
    task (your first turn on a child track);
  * a **task gate result** (`task.gate_result`; gate passed or FAILED, \
    with a log tail);
  * an **ungated task completion** (a worker reported `task.completed`);
  * a **task failure** (worker-reported failure or spawn failure);
  * a **report edit made by somebody else** (a `track.report_edited` whose \
    `author` is one of {spec_wake_authors}).

On each turn:

Read track state with the `neige` shell CLI (`neige state`, `neige ls`, \
`neige cat`); mutate the track with the `calm.*` MCP tools. Reads observe; \
writes are transactional.

1. Run `neige state` to read the track's current shape (lifecycle, \
   track/card metadata; results are in `runs/*` views, not in `neige state`). \
   This is your ground truth — do NOT keep \
   a private model of track state across turns. \
   Before you write anything to the report in a session, call \
   `calm.report.read` once: the report carries its own structure and its own \
   maintenance contract, and you may not write to a document you have not \
   read. `report_startup_read_required` tells you whether it already holds \
   content beyond the default skeleton. If the read returns `task` blocks, \
   treat them as the authoritative pre-set plan. Activate it by replacing \
   those blocks and setting `ready: true` — use the read's block ids and \
   revision as replace anchors. Do not mint duplicate tasks. Prose blocks are \
   NOT a plan to activate: maintain them per the document's own contract.
2. Decide what to do next and act:
   * **Name the track.** The title is a label for the work, not the user's \
     instruction. If `neige state` shows this track's title is still empty, \
     then as soon as you have worked out from the conversation what this \
     track is actually about, call `calm.track.rename(title, message?)` once. \
     If it already carries a title, someone has already named it — the user, \
     or the parent spec that opened this track — so leave it as it is and do \
     not call the tool. Write a \
     short noun phrase a human would recognise in a list, not a restatement \
     of the user's first sentence. Naming is name-once: if the track already \
     has a title the call returns \
     `{\"ok\": false, \"refused\": \"already_named\"}` and changes nothing — \
     that is not an error, leave the name alone and move on. Template tracks \
     and the per-area chat track refuse the same way. \
     Do not stall the work waiting to name it, and do not name it from a \
     guess: if you do not yet know what the user wants, ask.
   * Maintain task declarations as report `task` blocks. Read the report with \
     `calm.report.read`; for create, pass its `docRev` as `if_doc_rev`, while \
     replace passes the target block's `rev` as `if_rev`. Use \
     `calm.report.blocks.upsert` for both operations. A live task payload needs a per-track-unique \
     `key`, `kind` (`codex`, `claude`, or `terminal`), `goal`, `ready: true`, \
     and `declared_by: \"spec\"`; it may also carry `acceptance`, `depends_on` \
     sibling keys, `priority`, and usually `gate`. Use `calm.plan.cancel` to \
     cancel a pending projected task. Use `calm.plan.list` to inspect status.
   * Every codex or claude task should declare a verification `gate` with \
     re-runnable commands (fmt/linters/tests as appropriate). On tracks with \
     `require_task_gates`, an ungated codex/claude block write still succeeds, \
     but the read surface reports a `gate_required` diagnostic and the task is \
     not projected or scheduled unless it provides `no_gate_reason`; terminal \
     tasks are exempt. Gate cwd defaults task cwd → track cwd; set \
     `gate.cwd` when the worker's checkout differs. Gates may run more \
     than once after kernel restarts, so declare only re-runnable commands.
   * When a gate fails, treat the `task.gate_result` as a machine fact, \
     not a worker claim. Remediate by inserting a NEW `task` block with a \
     new key; retry policy is yours.
   * Record verdicts via `calm.task.verdict(status=...)` when worker \
     output is ready to validate. Required args include `message`; \
     optional `lifecycle` advances the track in the same write.
   * Discover report structure across the area with `calm.area.outline`, \
     and inspect incoming links to a report with \
     `calm.report.links.backlinks`.
   * Cross-reference as `[label](neige://wave/<track_id>#<block_id>)`; omit \
     `#<block_id>` for the whole report. Get block ids from `calm.area.outline`, \
     the single source for the whole area, including your own track. Links resolve \
     only within the area; missing anchors fall back to the whole report.
   * Keep the track report current — see the Track Report section below \
     for which write tool to use. Only `calm.report.write` and \
     `calm.report.edit` take `message` (required) and optional \
     `lifecycle`; the block-addressed writes do not.
3. **END YOUR TURN.** Do NOT poll or loop waiting for the next event. \
   The kernel schedules ready tasks, runs gates, and pushes the next \
   observation as a fresh turn the moment it arrives — you will be \
   re-invoked automatically. Never wait for worker spawns. If there is \
   nothing left to do this turn, just stop; if the track is \
   `done`/`failed`/`blocked` and you're waiting on the user, stop and \
   wait to be re-invoked.

## Track Report (issue #229)

Track 有一份面向用户的 Markdown 报告，由你维护。它显示在 Track 页面顶部，\
是用户了解这个 Track 状态的主要入口。

**报告自带的结构就是规则。** 内核不规定这份报告该有哪些章节、每个章节该写什么——\
那些规矩由文档自己携带，通常写在正文顶部的一段 HTML 注释里：它在渲染时被丢弃，\
用户在页面上看不到，但它在 body 源码里，你每次 `calm.report.read` 都读得到。\
你的职责是**维护**这个结构，不是重新设计它：

  * 不要新增文档契约清单以外的章节，不要重命名章节，不要调整章节顺序。\
    契约清单里列到的章节，缺哪个就按契约补哪个。
  * **不要因为格式看起来陌生或「旧」就整体重写本文档。** 一份自带结构的报告\
    就是它该有的样子；把它铲平成你熟悉的格式是破坏，不是整理。
  * 文档里的维护契约优先于你的习惯。契约没规定的，按契约的精神补。
  * 找不到任何契约时才用你的判断，并保持现有章节不变。

**块边界**：文档在**行首的 `# ` 或 `## `** 处切成块（更深的标题不切）。\
切出来的块就是 `calm.report.blocks.upsert` 用 `id` 寻址、深链 / 反链指向的\
那个单位。所以增删一个 H1/H2 就是增删一个块。

**内核保留的唯一硬约束**：无论文档自己的契约怎么说，**散文正文**（所有 prose \
块的文字合计；非 prose 块在 body 里的 fence 投影不计入）硬上限 **2000 字**。\
逼近上限就 consolidate。

**用中文写** — body / summary / 各种 MCP 工具调用里的 `message` 字段都用中文。\
读者听众是同一个人，不要混语言。

READ 当前报告及整文档锚用 `calm.report.read`：响应里的 `body` 是当前正文，
`docRev` 是下一次整文档写必须携带的锚。`neige cat report.md` 只返回 body，
不提供 `docRev`，因此不能用它为整文档写取锚。WRITE 按下面的优先级选：

  * **首选 · 局部修改** — `calm.report.blocks.upsert`：替换已有块传 `id` + \
    该块的 `if_rev`，新建块传 `if_doc_rev`（可选 `position`）。只动一个块，\
    块 id 保持不变，深链 / 反链不会失效。
  * **确实需要整文档重写** — 先 `calm.report.read({ with_markers: true })` \
    拿到每个块前面带 `<!-- neige:b_xxxx -->` 标记行的正文，在这份文本上改，\
    改完用 `calm.report.write_markdown(body, if_doc_rev, summary?)` 写回。\
    标记行把每个块钉回原来的 id（服务端剥掉，永不入库），这是整文档重写里 \
    **唯一** 能保住块 id 的通道。
  * **兼容 / 局部精修** — `calm.report.write(body, if_doc_rev, summary?, message, \
    lifecycle?)` 整体替换、`calm.report.edit(old_string, new_string, if_doc_rev, \
    replace_all?, message, lifecycle?)` 字符串替换。⚠️ 这两个没有标记通道：\
    整体替换会 best-effort 重新推导块 id，可能把已有块打散（深链 / 反链失效）；\
    而且新正文里每个非 prose 块的 ```neige-block <kind>``` fence 必须 \
    逐字节原样带回，碰坏一个整次写就被守卫拒绝。所以只在小范围精修、或需要带上 \
    `message` / `lifecycle` 时才用它们，不要拿它们做大改写。
  * `message` / `lifecycle` 只有 `calm.report.write` / `calm.report.edit` 接受；\
    `calm.report.blocks.*` 与 `calm.report.write_markdown` 不接受这两个参数，\
    需要推进 lifecycle 时改用 `calm.task.verdict` / `calm.plan.cancel` 上的 \
    `lifecycle`。

整文档写必须把最近一次 `calm.report.read` 返回的 `docRev` 原样作为
`if_doc_rev` 传入；写响应会返回新的 `docRev`，后续写使用这个新锚。它不是
`calm.report.blocks.*` 使用的块级 `if_rev`，两者不可混用。

`summary` 是侧栏的 1-行预览，~80 字符以内。

**内核已经知道 / 已经渲染的，不要在报告里复述：**

  * 不要复述 lifecycle 状态（用户在卡头已经看到 badge 了）。
  * 不要复述任务状态和进度（TASKS 面板已经渲染了任务的真实运行态）。
  * 不要把 `neige state` / `track_state` 的读取结果、工具调用记录等内核自己\
    就持有的机械事实写进报告。

### Reacting to report edits by others

报告不只有你在写：用户可以直接编辑，插件可以在 accept 事务里成批写入，\
track assistant 会话也可以写。内核会用 `track.report_edited` observation \
唤醒你；会唤醒你的 `author` 只有这几个：{spec_wake_authors}。该 turn 开始时：

1. 调 `calm.report.read` 拿最新 body 和 `docRev`。
2. 把这次修改当作 ground truth — 不要覆盖。assistant 的编辑和用户的编辑\
   同一条规则：它来自另一个会话，不是你的草稿的旧版本。
3. 然后继续你的任务。**不要** 盲目 `report.write` 你之前的草稿。

你不会被自己（`author = \"spec\"`）的编辑唤醒。

## Reading worker outputs (issue #339)

`neige state` deliberately returns metadata only — track row plus a cards \
list with id/kind/role/sort/created_at/updated_at, **no card payloads, \
no event payloads, no worker results**, plus the sibling boolean \
`report_startup_read_required`. To read what a worker actually \
produced, use the read-only track views from your shell via the `neige` \
CLI, which composes with tools like `grep`, `jq`, and `head`:

  * `neige ls [path]` — directory listing, e.g. `neige ls runs/` or \
    `neige ls /`.
  * `neige cat <path>` — read one view, e.g. `neige cat runs/K.md`, \
    `neige cat plan/<key>/gate.log`, \
    `neige cat runs/index.json`, \
    `neige cat cards/<card_id>/.payload.json`, or \
    `neige cat cards/<card_id>/runtime.json`.

Available `<path>` values for `neige cat` / `neige ls`:

  * `runs/<task_id>.md` — human-readable summary of one run \
    (status, worker output, gate result, verdict if recorded).
  * `runs/<task_id>.json` — structured projection. \
    `events.completed.payload.result` is the worker's actual output; \
    `events.failed` carries failures; `verdict` holds any \
    `task.verdict` accept/reject you recorded; `worker_card.payload` \
    has the plan task context.
  * `runs/index.json` — array of all runs in the track with status, kind, \
    requested_at, finished_at, worker_card_id, and verdict.
  * `plan/<key>/gate.log` — latest verification gate log for a planned \
    task key. Read this after a `task.gate_result`, especially on FAILED \
    gates.
  * `cards/<card_id>/.payload.json` — the card's own payload in the \
    track (e.g. another worker's bookkeeping or dispatch context). \
    Runtime identity and status live in `cards/<card_id>/runtime.json`.
  * `cards/<card_id>/runtime.json` — typed runtime identity/status for \
    a card, or `null` when it has no runtime row.
  * `/` — root directory listing.
  * `report.md` — current track report body.

When you are pushed an ungated task completion or failure, the canonical \
first read is `neige cat runs/K.md` where `K` is the task id from the \
observation. When you are pushed a gate result, first read \
`neige cat plan/<key>/gate.log`. The push observation is just a \
notification; the result lives in these views, not in `neige state`.

The view is READ-ONLY. To act on what you read, call \
`calm.task.verdict(idempotency_key=K, status=\"accepted\" | \
\"rejected\")` to record a semantic verdict on top of a completed task, \
and/or create a new `task` block with `calm.report.blocks.upsert` for \
follow-up work. Lifecycle-capable writes require `message` and can include \
`lifecycle=...`.

Track is implicit — derived from your card identity. Do NOT pass a \
`track_id` (these tools have no such parameter; cross-track reads are \
forbidden by design).

Do not mint new spec cards from within this session.
";

/// Head of the **claude** (CLI-completion) worker prompt — everything
/// before the shared `## Reading track state` tail. Step 3 reports through
/// the `neige` shell CLI. A literal-yielding macro so it can be
/// `concat!`'d with the shared tail at compile time (keeps DRY without a
/// runtime allocation or a stale duplicated tail).
macro_rules! worker_prompt_head_cli {
    () => {
        "\
You are a worker agent under spec card on track `{track_id}`.

You were spawned to execute one job. Your contract:

1. Read the goal, context, and acceptance criteria handed to you. \
   Run `neige state` if you need to inspect the track's shape before \
   starting — but don't poll it; the track snapshot you receive once is \
   enough.
2. Execute the task. Make tool calls, write files, run commands \
   — whatever the goal requires.
3. When the task is done, report exactly once via the `neige` shell CLI:
   * On success: `neige task-completed --idempotency-key K --result <json-or-text>` \
     where `K` echoes the idempotency key the kernel handed you. \
     Append `--artifact <path>` (may repeat) for any file/blob references \
     you produced.
   * On failure: `neige task-failed --idempotency-key K --reason '<text>'` \
     with a free-form failure description.
4. Exit. You are short-lived by design — run your single job and stop. \
   Your completion report is a claim; a kernel gate may verify it before \
   the task counts as done. The kernel delivers ungated reports, failures, \
   or gate results to the spec card as pushed turn inputs, and the spec \
   continues the track from there. You do not wait for or observe anything.

You may NOT call `calm.task.verdict` — that is a spec-only tool and the \
kernel's role gate will refuse you. You also may NOT mint new workers; \
`calm.task.dispatch` is retired, and the kernel's role gate (#583) still \
refuses worker-actor dispatch emits from old paths. If the job needs \
further decomposition, report `task.failed` with a reason \
explaining what's missing and the spec will handle re-decomposition.

"
    };
}

/// Head of the **codex** (MCP-completion) worker prompt — everything
/// before the shared `## Reading track state` tail. Step 3 reports through
/// the native `calm.task.complete` / `calm.task.fail` MCP tools.
macro_rules! worker_prompt_head_mcp {
    () => {
        "\
You are a worker agent under spec card on track `{track_id}`.

You were spawned to execute one job. Your contract:

1. Read the goal, context, and acceptance criteria handed to you. \
   Run `neige state` if you need to inspect the track's shape before \
   starting — but don't poll it; the track snapshot you receive once is \
   enough.
2. Execute the task. Make tool calls, write files, run commands \
   — whatever the goal requires.
3. When the task is done, report exactly once via the MCP tool:
   * On success: call `calm.task.complete` with `idempotency_key` = K \
     (the kernel task id you were handed). Optionally include `result` \
     (json-or-text) and `artifacts` (an array of path/blob refs you produced).
   * On failure: call `calm.task.fail` with `idempotency_key` = K and a \
     free-form `reason` (required).
4. Exit. You are short-lived by design — run your single job and stop. \
   Your completion report is a claim; a kernel gate may verify it before \
   the task counts as done. The kernel delivers ungated reports, failures, \
   or gate results to the spec card as pushed turn inputs, and the spec \
   continues the track from there. You do not wait for or observe anything.

You may NOT call `calm.task.verdict` — that is a spec-only tool and the \
kernel's role gate will refuse you. You also may NOT mint new workers; \
`calm.task.dispatch` is retired, and the kernel's role gate (#583) still \
refuses worker-actor dispatch emits from old paths. If the job needs \
further decomposition, report `task.failed` with a reason \
explaining what's missing and the spec will handle re-decomposition.

"
    };
}

/// Shared `## Reading track state` tail — concatenated into BOTH worker
/// prompts. Reads stay on the `neige` shell CLI for both providers
/// (#339/#377 read-via-CLI principle); only the completion *report* moves
/// to MCP for codex.
macro_rules! worker_prompt_tail {
    () => {
        "\
## Reading track state

You may read your track's state READ-ONLY from the shell with the `neige` \
CLI: `neige state` reads the track shape, `neige ls [path]` lists views, \
and `neige cat <path>` reads one view. Useful paths include `/`, \
`runs/index.json`, \
`runs/<idempotency_key>.md`, `runs/<idempotency_key>.json`, \
`cards/<card_id>/.payload.json`, and `cards/<card_id>/runtime.json`. \
`.payload.json` is the card's own payload; runtime identity/status lives \
in `runtime.json`. These views are own-track-only; cross-track reads are forbidden.
"
    };
}

/// Worker-agent system prompt. PR8 (#136) replaces the PR6 stub with
/// the production prompt: workers are short-lived, fire-and-forget,
/// driven by the kernel scheduler from the spec-maintained plan. They
/// run one job and exit.
///
/// The name retains the `_PLACEHOLDER` suffix only to avoid churn in
/// downstream call sites; the content is now production. A followup
/// can rename this to `WORKER_SYSTEM_PROMPT_TEMPLATE` for symmetry
/// with [`SPEC_SYSTEM_PROMPT_TEMPLATE`] when there's no other PR
/// touching this file.
///
/// This is the **claude** (CLI-completion) body; codex uses
/// [`WORKER_CODEX_SYSTEM_PROMPT`] (#838 Move 2).
pub(crate) const WORKER_SYSTEM_PROMPT_PLACEHOLDER: &str =
    concat!(worker_prompt_head_cli!(), worker_prompt_tail!());

/// codex worker variant (#838 Move 2). Identical to
/// [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] except step 3: completion is
/// reported through the native `calm.task.complete` / `calm.task.fail`
/// MCP tools (channel 2 — DaemonTrust + codex-injected `_meta.threadId`)
/// instead of the `neige` shell CLI. This decouples the kernel-critical
/// completion path from the per-thread `shell_environment_policy` env
/// (channel 3) that keeps getting silently dropped (#738/#747/#836).
///
/// claude keeps [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] (it has no codex
/// thread to authenticate against — the native-MCP resolver is
/// `AgentProvider::Codex`-only — and its contract test asserts the CLI
/// surface). The shared `## Reading track state` block (`worker_prompt_tail!`)
/// is concatenated into both, keeping reads on the CLI for both providers.
pub(crate) const WORKER_CODEX_SYSTEM_PROMPT: &str =
    concat!(worker_prompt_head_mcp!(), worker_prompt_tail!());

/// #1189 — the track assistant's system prompt.
///
/// Deliberately not a trimmed copy of [`SPEC_SYSTEM_PROMPT_TEMPLATE`]: most of
/// that prompt instructs the agent to drive the lifecycle state machine and the
/// plan, and every one of those tools rejects `CardRole::Assistant` at the
/// handler. Describing them here would teach the agent to spend turns on calls
/// that can only come back `-32602`.
///
/// Two things in here are load-bearing rather than stylistic:
///
/// * **"read with markers before you rewrite"** — a `calm.report.write` style
///   full-document rewrite is unavailable to this role, and a block write that
///   re-mints ids reads as "delete every task block and create new ones", which
///   the task-block guard rejects as a whole transaction (design §3.2a-bis.4).
///   The marker read is what keeps existing block ids stable.
/// * **"you do not own the plan"** — the guard exists, but an agent that keeps
///   trying to write task blocks produces a stream of rejected turns instead of
///   answering the user.
pub(crate) const ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = "\
You are an assistant conversation on track `{track_id}`.

You are talking with the user. Answer them. You are NOT the track's spec agent: \
you do not own the track's lifecycle, its plan, or its workers, and the kernel \
will reject you if you try to drive any of them.

## What you can do

* **Read the track.** Use the `neige` shell CLI (`neige state`, `neige ls`, \
  `neige cat`) for track and card state, and `calm.report.read` for the track \
  report.
* **Run shell commands** in the track's workspace, subject to the usual sandbox.
* **Write prose into the track report** through the block tools: \
  `calm.report.blocks.upsert`, `.move`, `.delete` \
  (`calm.report.blocks.kinds` lists the block vocabulary), or \
  `calm.report.write_markdown` for a whole-document rewrite.

## What you cannot do

Lifecycle transitions, plan writes, task verdicts, review, admin, and the \
whole-document `calm.report.write` are not yours. Neither are `task` blocks: \
the track's plan belongs to the spec agent, and a `task` block written from here \
is rejected — the whole write, not just that block. If the user asks for work \
to be scheduled, say so plainly and let them take it to the spec agent.

## Writing to the report, concretely

1. Call `calm.report.read` with `with_markers: true` FIRST. It gives you the \
   document's `docRev` and every block's `{id, kind, rev}`.
2. To add a block, pass that `docRev` as `if_doc_rev`. To replace one, pass \
   the block's own `rev` as `if_rev` together with its `id`.
3. `calm.report.write_markdown` needs the SAME marker read first, and you must \
   send the markers back. Without them your rewrite mints new ids for existing \
   content, which reads as deleting every block and creating replacements — \
   and if any of them were task blocks the entire write is refused.
4. Another session may be writing at the same time. A revision conflict means \
   somebody else moved first: re-read and reapply, do not retry blindly.

Keep the report's own structure and conventions; you are a guest in a document \
the spec agent maintains.
";

/// Render the report-edit authors that wake the spec, straight from the
/// dispatcher's wake set, in the wire spelling the `track.report_edited`
/// payload actually carries (so the prompt names what the agent will see).
fn spec_wake_authors_prose() -> String {
    crate::dispatcher::SPEC_WAKE_AUTHORS
        .iter()
        .map(|author| format!("`{}`", author.wire_str()))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Substitute the per-spawn placeholders into a prompt template:
/// `{track_id}` and `{spec_wake_authors}`. Lifted out as its own helper so
/// call sites do not need rewriting when the substitution set grows.
pub(crate) fn render_system_prompt(template: &str, track_id: &str) -> String {
    template
        .replace("{track_id}", track_id)
        .replace("{spec_wake_authors}", &spec_wake_authors_prose())
}

#[cfg(test)]
const TASK_BLOCK_PROTOCOL_GOLDEN: &str = concat!(
    "   * Maintain task declarations as report `task` blocks. Read the report with ",
    "`calm.report.read`; for create, pass its `docRev` as `if_doc_rev`, while ",
    "replace passes the target block's `rev` as `if_rev`. Use ",
    "`calm.report.blocks.upsert` for both operations. A live task payload needs a per-track-unique ",
    "`key`, `kind` (`codex`, `claude`, or `terminal`), `goal`, `ready: true`, ",
    "and `declared_by: \"spec\"`; it may also carry `acceptance`, `depends_on` ",
    "sibling keys, `priority`, and usually `gate`. Use `calm.plan.cancel` to ",
    "cancel a pending projected task. Use `calm.plan.list` to inspect status."
);

/// Exact paragraph oracle for the static task-block protocol. The shipped
/// template's fully rendered prompt has a separate whole-document golden;
/// free-text contradictions cannot be proved absent with a keyword list.
#[cfg(test)]
pub(crate) fn validate_spec_prompt_contract(prompt: &str) -> Result<(), String> {
    let start = prompt
        .find("   * Maintain task declarations as report `task` blocks.")
        .ok_or_else(|| "task-block protocol paragraph is missing".to_string())?;
    let remainder = &prompt[start..];
    let end = remainder
        .find("\n   * Every codex or claude task")
        .ok_or_else(|| "task-block protocol paragraph terminator is missing".to_string())?;
    let actual = &remainder[..end];
    if actual != TASK_BLOCK_PROTOCOL_GOLDEN {
        return Err(format!(
            "task-block protocol differs from golden\nexpected: {TASK_BLOCK_PROTOCOL_GOLDEN:?}\nactual:   {actual:?}"
        ));
    }

    Ok(())
}

/// Test-only seam (#838 A1 e2e): render the rendered worker prompt for the
/// provider under test. `codex=true` yields the native-MCP-completion body
/// ([`WORKER_CODEX_SYSTEM_PROMPT`], what `codex_adapter` ships);
/// `codex=false` yields the CLI body ([`WORKER_SYSTEM_PROMPT_PLACEHOLDER`],
/// what `claude_adapter` ships and the RED baseline). Doc-hidden so it does
/// not widen the public prompt API beyond the e2e harness.
#[doc(hidden)]
pub fn render_worker_prompt_for_e2e(track_id: &str, codex: bool) -> String {
    let role = if codex {
        SeededCardRole::WorkerCodex
    } else {
        SeededCardRole::Worker
    };
    render_system_prompt(role.prompt_template(), track_id)
}

/// Test-only seam (#1189): the exact `developer_instructions` string a track
/// assistant's `thread/start` must carry.
///
/// Exposed rather than re-spelled in the test on purpose. An integration test
/// that asserted on a substring ("contains `assistant`") would stay green if the
/// assistant profile were wired to the SPEC prompt, which is one of the two
/// mutations #1189's A2 gate has to catch; a test that re-declared the template
/// would stay green if the adapter stopped rendering the placeholder. Handing
/// out the rendered string makes the assertion an equality against production's
/// own value.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn render_assistant_prompt_for_test(track_id: &str) -> String {
    render_system_prompt(ASSISTANT_SYSTEM_PROMPT_TEMPLATE, track_id)
}

/// Roles that legitimately need role-specific Codex setup.
/// Carved out of [`crate::model::CardRole`] so the seeding helper can
/// only ever be handed a value that maps to a system-prompt template
/// (no general Worker path to silently fall through). PR6 followup of
/// issue #136 — note 3 from the original review.
///
/// User-facing Worker cards still flow through `routes::codex_cards`'s
/// simpler seed path (which writes a no-prompt config.toml inline); they
/// must not reach this helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeededCardRole {
    /// Spec card minted by `routes::tracks::create_track`. Gets
    /// [`SPEC_SYSTEM_PROMPT_TEMPLATE`].
    Spec,
    /// Worker card minted by the dispatcher for a **claude** provider.
    /// Gets [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] — completion is reported
    /// through the `neige` shell CLI (claude has no codex thread for the
    /// native-MCP path and its contract test asserts the CLI surface).
    Worker,
    /// Worker card minted by the dispatcher for a **codex** provider
    /// (#838 Move 2). Gets [`WORKER_CODEX_SYSTEM_PROMPT`] — completion is
    /// reported through the native `calm.task.complete` / `calm.task.fail`
    /// MCP tools, decoupling the kernel-critical completion path from the
    /// channel-3 exec-shell env.
    WorkerCodex,
}

impl SeededCardRole {
    pub(crate) fn prompt_template(self) -> &'static str {
        match self {
            SeededCardRole::Spec => SPEC_SYSTEM_PROMPT_TEMPLATE,
            SeededCardRole::Worker => WORKER_SYSTEM_PROMPT_PLACEHOLDER,
            SeededCardRole::WorkerCodex => WORKER_CODEX_SYSTEM_PROMPT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_system_prompt_substitutes_track_id() {
        let out = render_system_prompt(SPEC_SYSTEM_PROMPT_TEMPLATE, "track-abc");
        assert!(
            out.contains("track `track-abc`"),
            "track id should be substituted; got: {out}"
        );
        assert!(
            !out.contains("{track_id}"),
            "placeholder should be gone; got: {out}"
        );
    }

    #[test]
    fn render_system_prompt_preserves_role_template_content() {
        let spec = render_system_prompt(SeededCardRole::Spec.prompt_template(), "track-abc");
        assert!(spec.contains("You are the spec agent for track `track-abc`."));
        assert!(!spec.contains("calm.update_track_state"));
        assert!(!spec.contains("calm.plan.upsert"));
        assert!(spec.contains("calm.report.blocks.upsert"));
        assert!(spec.contains("`ready: true`"));
        assert!(spec.contains("`declared_by: \"spec\"`"));
        assert!(spec.contains("calm.plan.list"));
        assert!(!spec.contains("calm.task.dispatch"));
        assert!(spec.contains("calm.task.verdict"));

        let worker = render_system_prompt(SeededCardRole::Worker.prompt_template(), "track-abc");
        assert!(worker.contains("You are a worker agent under spec card on track `track-abc`."));
        assert!(worker.contains("neige task-completed"));
    }

    /// #1252 S0-1: the prompt's wake list is *rendered* from
    /// `dispatcher::SPEC_WAKE_AUTHORS`, so a change to who the dispatcher
    /// wakes rewrites the prompt. The expected wire spellings are pinned
    /// here on purpose: they are the independent statement of the contract
    /// that catches a silent shrink of the const.
    #[test]
    fn spec_prompt_renders_the_dispatcher_report_edit_wake_set() {
        let p = render_system_prompt(SPEC_SYSTEM_PROMPT_TEMPLATE, "track-wake");

        assert!(
            !p.contains("{spec_wake_authors}"),
            "wake-author placeholder must be substituted; got: {p}"
        );
        // The exact rendered sequence, stated independently of the const:
        // a silent shrink of `SPEC_WAKE_AUTHORS` fails here.
        let expected_list = "`user` / `plugin` / `assistant`";
        assert_eq!(
            spec_wake_authors_prose(),
            expected_list,
            "the dispatcher wakes the spec on user/plugin/assistant report edits, \
             so that is what the prompt must render"
        );
        assert_eq!(
            p.matches(expected_list).count(),
            2,
            "both wake-set sites must carry the rendered list; got: {p}"
        );

        let rendered_list = spec_wake_authors_prose();
        for excluded in ["spec", "kernel"] {
            assert!(
                !rendered_list.contains(excluded),
                "`{excluded}`-authored edits do not wake the spec, so the rendered \
                 wake list must not name one; got: {rendered_list}"
            );
        }
        assert!(
            p.contains("你不会被自己（`author = \"spec\"`）的编辑唤醒。"),
            "prompt must still state the self-edit exclusion; got: {p}"
        );
        assert!(
            !p.contains("只有用户的会"),
            "prompt must not claim only user edits wake the spec; got: {p}"
        );
    }

    /// #1211 S3 — the prompt is not the guard and the guard is not the
    /// prompt; both have to exist. `mcp_track_rename` pins the guard. This
    /// pins the instruction, because a `calm.track.rename` no agent is ever
    /// told about would leave every track named `Untitled` with a green test
    /// suite: S1 deleted the only other thing that ever named a track.
    ///
    /// It also pins the name-once *expectation*, not just the tool name. An
    /// agent told to rename but not told that a refusal is normal is an agent
    /// that retries a refusal.
    #[test]
    fn spec_prompt_instructs_the_agent_to_name_the_track() {
        let p = render_system_prompt(SPEC_SYSTEM_PROMPT_TEMPLATE, "track-naming");
        assert!(
            p.contains("calm.track.rename"),
            "spec prompt must name the naming tool"
        );
        // The instruction is CONDITIONAL on observed state, not a blanket
        // "every track is unnamed": child tracks are born titled from their
        // parent task's goal, and a create request may still carry a title,
        // so an unconditional "rename it" instruction buys a guaranteed
        // `already_named` refusal — a wasted write attempt on every such track.
        assert!(
            p.contains("If `neige state` shows this track's title is still empty"),
            "spec prompt must condition naming on the observed empty title"
        );
        assert!(
            p.contains("If it already carries a title") && p.contains("not call the tool"),
            "spec prompt must tell the agent to skip the call on an already-titled track"
        );
        assert!(
            !p.contains("A track is created unnamed") && !p.contains("nobody has named it yet"),
            "spec prompt must not claim every track starts unnamed"
        );
        assert!(
            p.contains("Naming is name-once"),
            "spec prompt must state the name-once rule"
        );
        assert!(
            p.contains("already_named") && p.contains("that is not an error"),
            "spec prompt must tell the agent a refusal is normal, not a retry signal"
        );
        // The instruction belongs to the per-turn action list, not to some
        // decorative preamble: it has to sit inside step 2, where the agent
        // decides what to do.
        let step2 = p
            .find("2. Decide what to do next and act:")
            .expect("step 2 is present");
        let step3 = p.find("3. **END YOUR TURN.**").expect("step 3 is present");
        let naming = p
            .find("calm.track.rename")
            .expect("naming instruction present");
        assert!(
            step2 < naming && naming < step3,
            "the naming instruction must live inside step 2's action list"
        );
    }

    #[test]
    fn spec_prompt_documents_claude_plan_kind_and_gate_policy() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        assert!(
            p.contains("(`codex`, `claude`, or `terminal`)"),
            "spec prompt must advertise the accepted task kinds"
        );
        assert!(
            p.contains("Every codex or claude task should declare a verification `gate`"),
            "spec prompt must require gates for both agent/code worker kinds"
        );
        assert!(
            p.contains("terminal tasks are exempt"),
            "spec prompt must not imply terminal tasks require gates"
        );
    }

    #[test]
    fn spec_prompt_pins_callable_task_block_protocol() {
        let p = render_system_prompt(SPEC_SYSTEM_PROMPT_TEMPLATE, "track-contract");
        validate_spec_prompt_contract(&p).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            p.contains("block write still succeeds")
                && p.contains("`gate_required` diagnostic")
                && p.contains("not projected or scheduled")
                && p.contains("unless it provides `no_gate_reason`")
                && p.contains("terminal tasks are exempt"),
            "prompt must describe diagnostic gate admission semantics"
        );
    }

    #[test]
    fn spec_prompt_contract_rejects_negative_context() {
        let prompt = render_system_prompt(SPEC_SYSTEM_PROMPT_TEMPLATE, "track-contract");

        let negated = prompt.replace(
            TASK_BLOCK_PROTOCOL_GOLDEN,
            &format!(
                "Never follow this obsolete rule: {TASK_BLOCK_PROTOCOL_GOLDEN} Swap those anchors instead."
            ),
        );
        assert_ne!(
            negated, prompt,
            "negative-context fixture must alter the prompt"
        );
        assert!(
            validate_spec_prompt_contract(&negated).is_err(),
            "correct tokens inside a negated paragraph must not satisfy the contract"
        );
    }

    /// #1185 §1.5 A — the first-turn read is UNCONDITIONAL.
    ///
    /// The policy that governs a report now travels inside the report, so an
    /// agent that has not read the document does not know the rules it is
    /// about to break. The old sentence gated the read on
    /// `report_startup_read_required`, which is false for every default track —
    /// exactly the tracks that only learn their contract by reading.
    #[test]
    fn spec_prompt_mandates_an_unconditional_first_read() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;
        let step1 = p.find("1. Run `neige state`").expect("step 1 is present");
        let read = p
            .find("Before you write anything to the report in a session, call `calm.report.read` once")
            .expect("unconditional first-read sentence is present");
        let step2 = p
            .find("2. Decide what to do next and act:")
            .expect("step 2 is present");
        assert!(
            step1 < read && read < step2,
            "the first-read sentence must sit after neige state and before step 2"
        );
        assert!(
            !p.contains("If `report_startup_read_required` is true, first call"),
            "the read must not be conditional on the startup bit (#1185 §1.5 A)"
        );
        // The bit survives with a narrower meaning: "does it hold content
        // beyond the default skeleton", not "must you read".
        assert!(p.contains("`report_startup_read_required` tells you whether it already holds"));
        // Activation is scoped to `task` blocks; prose is maintained, not
        // replaced — the fork path used to be ordered to flatten it.
        assert!(p.contains(
            "If the read returns `task` blocks, treat them as the authoritative pre-set plan"
        ));
        assert!(p.contains("Prose blocks are NOT a plan to activate"));

        assert!(p.contains("authoritative pre-set plan"));
        assert!(p.contains("replacing those blocks and setting `ready: true`"));
        assert!(p.contains("block ids and revision as replace anchors"));
        assert!(p.contains("Do not mint duplicate tasks"));
    }

    /// #293 cutover — the spec prompt must be push-native, not pull. It must
    /// carry the turn-reactive guidance (driven by pushed observations, end
    /// the turn, no looping).
    #[test]
    fn spec_prompt_is_push_native_not_pull() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        // No pull loop.
        assert!(
            !p.contains("long-poll"),
            "prompt must not describe a long-poll loop"
        );

        // Turn-reactive guidance present.
        assert!(
            p.contains("turn-reactive") || p.contains("END YOUR TURN"),
            "prompt must carry turn-reactive guidance"
        );
        assert!(
            p.contains("END YOUR TURN"),
            "prompt must tell the agent to end its turn"
        );
        assert!(
            p.contains("re-invoked"),
            "prompt must explain the kernel re-invokes the agent per observation"
        );
        assert!(
            p.contains("Do NOT poll or loop"),
            "prompt must forbid polling / looping"
        );
        // Reads go through the shell CLI; writes still go through MCP.
        assert!(
            p.contains("Run `neige state`")
                && p.contains("calm.report.blocks.upsert")
                && p.contains("calm.plan.list"),
            "prompt must read state via neige and maintain task blocks via MCP"
        );
        assert!(
            !p.contains("calm.update_track_state")
                && !p.contains("calm.task.dispatch")
                && !p.contains("calm.plan.upsert")
                && p.contains("calm.plan.cancel")
                && p.contains("calm.plan.list")
                && p.contains("calm.report.blocks.upsert")
                && p.contains("calm.task.verdict")
                && p.contains("calm.area.outline")
                && p.contains("calm.report.links.backlinks")
                // Signature-anchored: bare "calm.report.write" is now also a
                // prefix of "calm.report.write_markdown", so the loose form
                // would pass even if the compatibility tool disappeared.
                && p.contains("calm.report.write(body,")
                && p.contains("calm.report.edit(old_string,")
                && p.contains("calm.report.write_markdown"),
            "prompt must document retained track/task write tools and omit retired update_track_state"
        );
        assert!(
            !p.contains("Call `calm.track.state`"),
            "prompt must not instruct state reads via MCP"
        );
    }

    #[test]
    fn spec_prompt_documents_neige_reads_for_worker_outputs() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        assert!(
            p.contains("neige state") && p.contains("neige cat") && p.contains("neige ls"),
            "spec prompt must document the shell neige read CLI"
        );
        assert!(
            p.contains("neige cat report.md"),
            "spec prompt must explain why the body-only neige view cannot supply an anchor"
        );
        assert!(
            p.contains("runs/<task_id>"),
            "spec prompt must document run projections by task id"
        );
        assert!(
            p.contains("plan/<key>/gate.log"),
            "spec prompt must document plan gate logs"
        );
        assert!(
            p.contains("READ-ONLY"),
            "spec prompt must state track file views are read-only"
        );
        assert!(
            p.contains("runs/K.md"),
            "spec prompt must document the canonical post-completion read"
        );
        assert!(
            p.contains("calm.report.write(body,") && p.contains("calm.report.edit(old_string,"),
            "spec prompt must document report write/edit MCP tools"
        );
        assert!(
            p.contains("calm.area.outline")
                && p.contains("calm.report.links.backlinks")
                && !p.contains("calm.track.cat")
                && !p.contains("calm.track.ls")
                && p.contains("calm.report.read"),
            "spec prompt must include the anchored report read alongside retained read tools"
        );
        assert!(
            p.contains("[label](neige://wave/<track_id>#<block_id>)"),
            "spec prompt must pin the cross-reference form"
        );
    }

    #[test]
    fn spec_prompt_pins_whole_document_revision_anchor_contract() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;
        assert!(p.contains("`calm.report.read` 返回的 `docRev`") && p.contains("`if_doc_rev`"));
        assert!(p.contains("写响应会返回新的 `docRev`"));
        assert!(p.contains("块级 `if_rev`") && p.contains("不可混用"));
    }

    /// #1185 — the kernel prompt must name NO report section.
    ///
    /// Section vocabulary is policy: it belongs to the document, which carries
    /// it in a leading HTML comment that every read returns. A prompt that
    /// names sections re-imposes one template's shape on every document in the
    /// area, and the "rewrite anything unfamiliar" instruction that used to
    /// accompany it flattened any report that arrived with its own structure.
    ///
    /// The negative loop at the bottom is this slice's main invariant.
    #[test]
    fn spec_prompt_carries_no_section_vocabulary() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        // The mechanism the prompt keeps: structure travels with the document,
        // and flattening it is damage.
        assert!(
            p.contains("报告自带的结构就是规则"),
            "prompt must state that the document's own structure is the rule"
        );
        assert!(
            p.contains("不要因为格式看起来陌生或「旧」就整体重写本文档"),
            "prompt must forbid flattening an unfamiliar-looking report"
        );

        // The section ban must be QUALIFIED by the document's own contract
        // list. Unqualified it contradicts every shipped template:
        // their seeded body carries a single `# Plan` H1, and the contract
        // inside it requires the agent to add 概要 / 已完成 / 决策. An absolute
        // "never add a section" bullet and the "文档里的维护契约优先" fallback
        // two lines below cannot both be obeyed — this keeps them aligned with
        // `track_report_section_rules.md`'s own wording.
        assert!(
            p.contains("不要新增文档契约清单以外的章节"),
            "the section ban must be scoped to the document's contract list (#1185 D2)"
        );
        assert!(
            !p.contains("不要新增、重命名章节"),
            "an unqualified section ban contradicts the shipped templates' own contracts"
        );

        // `# 进行中` was dropped in #1172: the TASKS panel renders the real
        // task runtime state, so making the spec agent hand-maintain a prose
        // mirror of it every turn is pure LLM restatement of kernel-known,
        // already-rendered data. It must not come back via the skeleton either.
        assert!(
            !p.contains("# 进行中"),
            "prompt must NOT reintroduce `# 进行中` — task runtime state is owned by the TASKS panel"
        );
        assert!(
            !crate::track_report::TrackReportPayload::initial()
                .body
                .contains("# 进行中"),
            "the birth skeleton must NOT reintroduce `# 进行中` either"
        );

        // Append-to-progress was the wording that drove the runaway journal.
        assert!(
            !p.contains("append to `# Progress`"),
            "prompt must NOT instruct append-to-progress (root cause of runaway journals)"
        );

        // #1146 S1: the budget must scope to PROSE, not `body`. `body` is the
        // flat projection that also serializes every non-prose block's fence,
        // so a `body`-scoped budget was vacuously false on any track with task
        // blocks — no amount of concise prose could satisfy it.
        //
        // #1185 splits it: the 1000-word soft target is genre judgement and
        // moved into the document's contract; the 2000-word hard ceiling is the
        // kernel's own minimal policy floor and stays here.
        assert!(
            p.contains("散文正文") && p.contains("2000"),
            "prompt must keep the kernel's prose-scoped hard ceiling"
        );
        assert!(
            p.contains("不计入"),
            "prompt must state that non-prose fence projection is excluded from the budget"
        );
        assert!(
            !p.contains("body 控制在"),
            "prompt must NOT reintroduce the vacuous body-scoped budget"
        );

        // The migration instruction is gone, not relocated: it is what
        // flattened self-structured reports.
        assert!(
            !p.contains("整体 REWRITE"),
            "prompt must NOT order a wholesale rewrite of an existing report (#1185)"
        );

        // —— the main invariant ——
        for banned in [
            "# 概要",
            "# 待你定",
            "# 已完成",
            "# 决策",
            "# Goal",
            "# Progress",
            "# Needs attention",
            "# Results",
            "# Timeline",
        ] {
            assert!(
                !p.contains(banned),
                "spec prompt must not name a report section — structure travels with the document (#1185): {banned}"
            );
        }
    }

    /// #1146 S1 — whole-document rewrites must go through the ONLY
    /// id-preserving mouth: `calm.report.read { with_markers: true }` →
    /// `calm.report.write_markdown`. `calm.report.write` re-derives block ids
    /// best-effort (`reassign_ids`) and its new body must carry every
    /// non-prose fence back byte-for-byte or `guard_non_prose_stomp` rejects
    /// the write, so it must NOT be advertised as the preferred mouth.
    #[test]
    fn spec_prompt_routes_whole_document_rewrite_through_the_marker_channel() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        assert!(
            p.contains("calm.report.write_markdown"),
            "prompt must name the id-preserving whole-document write tool"
        );
        assert!(
            p.contains("with_markers"),
            "prompt must name the `with_markers` read that supplies the block-id markers"
        );
        assert!(
            p.contains("<!-- neige:b_xxxx -->"),
            "prompt must show the marker line shape the read emits"
        );
        // Targeted edits stay the first choice.
        assert!(
            p.contains("**首选 · 局部修改** — `calm.report.blocks.upsert`"),
            "prompt must make block-addressed upsert the preferred write"
        );
        // The trap must be spelled out, not merely de-emphasized.
        assert!(
            p.contains("best-effort 重新推导块 id"),
            "prompt must warn that wholesale replace re-derives block ids"
        );
        assert!(
            p.contains("neige-block <kind>") && p.contains("逐字节原样"),
            "prompt must warn that non-prose fences must survive byte-for-byte"
        );
        // The old wording promoted `calm.report.write` as 首选 — that is the
        // exact trap this slice removes.
        assert!(
            !p.contains("整体替换 （首选"),
            "prompt must NOT re-promote calm.report.write as the preferred write"
        );
        // `message`/`lifecycle` are NOT accepted by write_markdown or
        // blocks.*; the prompt must not leave the agent guessing.
        assert!(
            p.contains("`calm.report.blocks.*` 与 `calm.report.write_markdown` 不接受这两个参数"),
            "prompt must state that the block-addressed writes take no message/lifecycle"
        );
    }

    #[test]
    fn worker_prompt_documents_neige_read_cli() {
        let p = WORKER_SYSTEM_PROMPT_PLACEHOLDER;

        assert!(
            p.contains("neige state") && p.contains("neige cat") && p.contains("neige ls"),
            "worker prompt must document the shell neige read CLI"
        );
        assert!(
            p.contains("neige task-completed") && p.contains("neige task-failed"),
            "worker prompt must document task completion through the neige CLI"
        );
        assert!(
            p.contains("completion report is a claim")
                && p.contains("kernel gate may verify it")
                && p.contains("idempotency key the kernel handed you"),
            "worker prompt must describe gate verification and kernel-provided idempotency key"
        );
        assert!(
            p.contains("READ-ONLY") && p.contains("own-track-only"),
            "worker prompt must constrain neige reads to read-only own-track views"
        );
    }

    /// #838 Move 2 — the codex worker prompt reports completion through the
    /// native MCP tools, NOT the `neige task-completed`/`task-failed` CLI.
    /// claude keeps the CLI (covered by the const tests above + the
    /// claude_adapter contract test), so this is the codex-only divergence.
    #[test]
    fn worker_codex_prompt_reports_completion_via_mcp_tools_not_cli() {
        let p = WORKER_CODEX_SYSTEM_PROMPT;

        // Completion is mandated through the native MCP tools.
        assert!(
            p.contains("calm.task.complete") && p.contains("calm.task.fail"),
            "codex worker prompt must mandate the calm.task.complete / calm.task.fail MCP tools"
        );
        // It must NOT mandate the neige completion CLI (that is claude-only).
        assert!(
            !p.contains("neige task-completed") && !p.contains("neige task-failed"),
            "codex worker prompt must NOT mandate the neige completion CLI"
        );
        // Reads still ride the neige CLI for BOTH providers (shared tail).
        assert!(
            p.contains("neige state") && p.contains("neige cat") && p.contains("neige ls"),
            "codex worker prompt must keep the neige read CLI in the shared tail"
        );
        assert!(
            p.contains("READ-ONLY") && p.contains("own-track-only"),
            "codex worker prompt must keep the read-only own-track constraint"
        );
        // The required-arg wording matches the tool schemas: complete needs
        // `idempotency_key`; fail needs `idempotency_key` + a required `reason`.
        assert!(
            p.contains("idempotency_key") && p.contains("required"),
            "codex worker prompt must name idempotency_key and the required reason"
        );
    }

    /// The provider split must not change the claude (CLI) body: the codex
    /// and claude worker prompts share everything except step 3, so the
    /// shared `## Reading track state` tail must be byte-identical in both.
    #[test]
    fn worker_prompts_share_identical_reads_tail() {
        let marker = "## Reading track state";
        let cli_tail = WORKER_SYSTEM_PROMPT_PLACEHOLDER
            .split_once(marker)
            .map(|(_, tail)| tail)
            .expect("CLI worker prompt has a reads tail");
        let mcp_tail = WORKER_CODEX_SYSTEM_PROMPT
            .split_once(marker)
            .map(|(_, tail)| tail)
            .expect("codex worker prompt has a reads tail");
        assert_eq!(
            cli_tail, mcp_tail,
            "both worker prompts must share a byte-identical reads tail"
        );
    }
}
