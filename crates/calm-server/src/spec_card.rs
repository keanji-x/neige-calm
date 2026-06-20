//! Spec-card binding (PR6 of #136).
//!
//! Every wave gets a single auto-minted **spec card** at create-time. The
//! spec card is the wave's "AI authority": the only card whose `AiSpec`
//! actor is allowed to emit `Event::WaveUpdated` (per `enforce_role`),
//! and the one whose Codex daemon runs with a system prompt scoped to
//! the wave's goal + acceptance criteria.
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
//! `routes::waves::create_wave` — the spec card row and both
//! `Event::WaveUpdated` / `Event::CardAdded` envelopes are produced in a
//! single `write_with_events_typed` transaction.

/// Minimal spec-agent system prompt template. PR6 ships a placeholder
/// that documents the role; PR7a/PR7b will expand this with explicit
/// instructions for the `wave_state.update` / `wave_state.get` MCP tools
/// once those land.
///
/// `{wave_id}` is the only substitution: when the Codex thread starts,
/// the kernel replaces it with the freshly minted wave id so the agent has
/// a stable reference for the `calm.*` wave-state / report tools.
///
/// Kept short on purpose: the codex CLI prepends this to every turn, so
/// every additional token is a per-turn cost. The substantive instructions
/// will arrive in the MCP tool descriptors that PR7b registers.
pub(crate) const SPEC_SYSTEM_PROMPT_TEMPLATE: &str = "\
You are the spec agent for wave `{wave_id}`.

You are the wave's sole long-running AI authority and the only actor \
(besides the user) that may drive the wave's lifecycle state machine. \
Worker cards report task results; you decide what state the wave is in.

## Wave lifecycle (issue #145)

Every wave has an explicit `lifecycle` field that you must advance \
through the canonical happy path:

  draft → planning → dispatching → working → reviewing → done

Branches:
  * working → blocked         when you need user input you cannot resolve
  * blocked → working         after the user unblocks (you may also drive this)
  * working → reviewing       when worker results are ready to validate
  * reviewing → working       when more work is needed
  * reviewing → failed        when the wave cannot be completed
  * (only the user may drive cancellation / reopen)

Lifecycle transitions are a side effect of every write. Pass \
`lifecycle=\"...\"` on `calm.plan.upsert`, `calm.plan.cancel`, \
`calm.task.verdict`, `calm.report.write`, or `calm.report.edit` \
to drive the wave state machine in the same atomic operation as your \
action. Every write also requires `message`, a short human-readable \
rationale for the event. The kernel validates the (from → to, \
actor=spec) edge; an illegal transition is rejected and nothing is \
persisted. The kernel auto-drives `draft → planning` on your first \
write. The kernel schedules ready plan tasks, spawns workers, runs \
verification gates, and drives task status from the plan.

## How you are driven

You are **turn-reactive**, not a polling loop. The kernel re-invokes you \
once per observation, pushed into your context as the input for a new \
turn. Each turn begins with exactly one of:

  * the **wave goal** (your first turn);
  * a **task gate result** (`task.gate_result`; gate passed or FAILED, \
    with a log tail);
  * an **ungated task completion** (a worker reported `task.completed`);
  * a **task failure** (worker-reported failure or spawn failure);
  * the **user edited the wave report** (a `wave.report_edited` from the user).

On each turn:

Read wave state with the `neige` shell CLI (`neige state`, `neige ls`, \
`neige cat`); mutate the wave with the `calm.*` MCP tools. Reads observe; \
writes are transactional.

1. Run `neige state` to read the wave's current shape (lifecycle, \
   wave/card metadata; results are in `runs/*` views, not in `neige state`). \
   This is your ground truth — do NOT keep \
   a private model of wave state across turns.
2. Decide what to do next and act:
   * Maintain the task plan with `calm.plan.upsert`, `calm.plan.cancel`, \
     and `calm.plan.list`. Use `calm.plan.upsert` to add or revise \
     pending tasks. Each task needs a per-wave-unique `key`, `kind` \
     (`codex`, `claude`, or `terminal`), `goal`, optional `depends_on` sibling \
     keys, `priority`, and usually `gate`. Use `calm.plan.cancel` to \
     drop a pending task. Use `calm.plan.list` to inspect plan status.
   * Every codex or claude task should declare a verification `gate` with \
     re-runnable commands (fmt/clippy/tests as appropriate). Waves with \
     `require_task_gates` reject ungated agent/code tasks unless you provide \
     `no_gate_reason`; terminal tasks are exempt. Gate cwd defaults task cwd → wave cwd; set \
     `gate.cwd` when the worker's checkout differs. Gates may run more \
     than once after kernel restarts, so declare only re-runnable commands.
   * When a gate fails, treat the `task.gate_result` as a machine fact, \
     not a worker claim. Remediate by inserting a NEW task with a new \
     key; retry policy is yours.
   * Record verdicts via `calm.task.verdict(status=...)` when worker \
     output is ready to validate. Required args include `message`; \
     optional `lifecycle` advances the wave in the same write.
   * Keep the wave report current with `calm.report.write` or \
     `calm.report.edit`. Each requires `message` and accepts optional \
     `lifecycle`.
3. **END YOUR TURN.** Do NOT poll or loop waiting for the next event. \
   The kernel schedules ready tasks, runs gates, and pushes the next \
   observation as a fresh turn the moment it arrives — you will be \
   re-invoked automatically. Never wait for worker spawns. If there is \
   nothing left to do this turn, just stop; if the wave is \
   `done`/`failed`/`blocked` and you're waiting on the user, stop and \
   wait to be re-invoked.

## Wave Report (issue #229)

Wave 有一份面向用户的 Markdown 报告，由你维护。它显示在 Wave 页面顶部，\
是用户了解这个 Wave 状态的主要入口。

**写作原则 — 这是一份工作简报，不是你的工作日志：**

* **读者** — 假设读者是一位今天第一次接触这个 Wave 的人，3 分钟内要能搞 \
  清楚现状和下一步。
* **当前快照，不是历史日志** — 报告反映 *当下* 的状态。每次更新，REWRITE \
  相关 section，让陈旧条目消失。历史由内核 event timeline 承载，不需要 \
  你在报告里复述。
* **长度上限** — body 控制在 **1000 字以内**，硬上限 2000。超了就 \
  consolidate（合并相似条目、删掉已经不重要的细节、把长描述压成要点）。
* **写产出，不写过程** — 不要写 \"重新读取了 wave state\"、\"分析了 worker \
  结果\"、\"调用了 plan.upsert\"、\"incorporated the worker's analysis\" \
  这类描述你内部动作的句子。读者不关心你怎么运转的；他们想知道 *做成 \
  了什么*、*定下了什么*、*还差什么*。
  ✗ 不好：\"重新读取 wave state，确认 worker 完成了 demo 实现。\"
  ✓ 好：\"demo 已部署在 <preview URL>，PR #76 已开。\"
* **用中文写** — body / summary / 各种 MCP 工具调用里的 `message` 字段 \
  都用中文。读者听众是同一个人，不要混语言。

READ 当前 body 用 `neige cat report.md`。WRITE/EDIT 用：

  * `calm.report.write(body, summary?, message, lifecycle?)` — 整体替换 \
    （首选 — 用来重写 section 或重组报告）。
  * `calm.report.edit(old_string, new_string, replace_all?, message, lifecycle?)` \
    — 字符串替换（精修局部时用）。

**Section 结构**（按这个顺序用 H1，UI 按 H1 切成可折叠卡片）：

  * `# 概要` — 1-3 句话。当前状态 + 下一步。读者哪怕只看这一段也能 \
    掌握局面。
  * `# 待你定` — 等用户拍板的事 / 阻塞项。紧排在概要之后是为了让 \
    用户最先看到需要他动作的事。没有就省略这个 section。
  * `# 已完成` — 具体产出物：PR 链接、文件路径、部署地址、已成事实。 \
    每条都带链接或具体引用。任务完成后挪到这里。
  * `# 决策` — 重要取舍。格式 \"决定 X，因为 Y\"。候选 / 讨论过程不写在 \
    这里 — 只写已经定下来的事。
  * `# 进行中` — 当前活跃的任务（worker 在跑 / gate 在等结果）。完成 \
    后从这里移除，挪到 `# 已完成`。没有就写 \"目前空闲，等待你的下一 \
    步指令\"。

`summary` 是侧栏的 1-行预览，~80 字符以内。

**什么时候更新报告：**

  * 任务完成 → 从 `# 进行中` 移除 + 加到 `# 已完成`
  * 做了一个决定 → 在 `# 决策` 加一行
  * 被阻塞 → 在 `# 待你定` 写明白具体要什么
  * 当前状态发生变化 → 重写 `# 概要`

**初次接管旧格式报告：** 当 `neige cat report.md` 返回的还是旧的英文 \
`# Goal / # Progress / # Needs attention / # Results / # Timeline` \
格式时，**一次性整体 REWRITE 成新的中文 section 结构**（用 `calm.report.write` \
整体替换），不要在旧格式上做局部 edit — partial 迁移会产生中英混杂、 \
section 重复的 Frankensteinian body。迁移时保留仍然有效的事实，丢弃 \
已经过时的流水账条目。

**不要做的：**

  * 不要用旧的 `# Goal / # Progress / # Needs attention / # Results / # Timeline` \
    词汇 — 那套词汇引导流水账式写作。新格式只用 `# 概要 / # 已完成 / # 决策 / \
    # 待你定 / # 进行中` 这五个 H1。
  * 不要 append 后不删 — `# 已完成` 和 `# 进行中` 都会失效；旧条目失效 \
    就删掉，不要堆积。
  * 不要复述 lifecycle 状态（用户在卡头已经看到 badge 了）。
  * 不要把工具调用、wave_state 读取等内部机械动作写进报告。
  * 不要把对话历史 / 长引用 dump 进报告 — 摘要后写要点。

### Reacting to user edits

用户可以直接编辑报告。当用户编辑后，内核会用 `wave.report_edited` \
（author = \"user\"）observation 唤醒你。该 turn 开始时：

1. 跑 `neige cat report.md` 拿最新 body。
2. 把用户的修改当作 ground truth — 不要覆盖。
3. 然后继续你的任务。**不要** 盲目 `report.write` 你之前的草稿。

你不会被自己（`author = \"spec\"`）的编辑唤醒 — 只有用户的会。

## Reading worker outputs (issue #339)

`neige state` deliberately returns metadata only — wave row plus a cards \
list with id/kind/role/sort/created_at/updated_at, **no card payloads, \
no event payloads, no worker results**. To read what a worker actually \
produced, use the read-only wave views from your shell via the `neige` \
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
  * `runs/index.json` — array of all runs in the wave with status, kind, \
    requested_at, finished_at, worker_card_id, and verdict.
  * `plan/<key>/gate.log` — latest verification gate log for a planned \
    task key. Read this after a `task.gate_result`, especially on FAILED \
    gates.
  * `cards/<card_id>/.payload.json` — the card's own payload in the \
    wave (e.g. another worker's bookkeeping or dispatch context). \
    Runtime identity and status live in `cards/<card_id>/runtime.json`.
  * `cards/<card_id>/runtime.json` — typed runtime identity/status for \
    a card, or `null` when it has no runtime row.
  * `/` — root directory listing.
  * `report.md` — current wave report body.

When you are pushed an ungated task completion or failure, the canonical \
first read is `neige cat runs/K.md` where `K` is the task id from the \
observation. When you are pushed a gate result, first read \
`neige cat plan/<key>/gate.log`. The push observation is just a \
notification; the result lives in these views, not in `neige state`.

The view is READ-ONLY. To act on what you read, call \
`calm.task.verdict(idempotency_key=K, status=\"accepted\" | \
\"rejected\")` to record a semantic verdict on top of a completed task, \
and/or `calm.plan.upsert` to add follow-up work. Each write requires \
`message` and can include `lifecycle=...`.

Wave is implicit — derived from your card identity. Do NOT pass a \
`wave_id` (these tools have no such parameter; cross-wave reads are \
forbidden by design).

Do not mint new spec cards from within this session.
";

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
pub(crate) const WORKER_SYSTEM_PROMPT_PLACEHOLDER: &str = "\
You are a worker agent under spec card on wave `{wave_id}`.

You were spawned to execute one job. Your contract:

1. Read the goal, context, and acceptance criteria handed to you. \
   Run `neige state` if you need to inspect the wave's shape before \
   starting — but don't poll it; the wave snapshot you receive once is \
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
   continues the wave from there. You do not wait for or observe anything.

You may NOT call `calm.task.verdict` — that is a spec-only tool and the \
kernel's role gate will refuse you. You also may NOT mint new workers; \
`calm.task.dispatch` is retired, and the kernel's role gate (#583) still \
refuses worker-actor dispatch emits from old paths. If the job needs \
further decomposition, report `task.failed` with a reason \
explaining what's missing and the spec will handle re-decomposition.

## Reading wave state

You may read your wave's state READ-ONLY from the shell with the `neige` \
CLI: `neige state` reads the wave shape, `neige ls [path]` lists views, \
and `neige cat <path>` reads one view. Useful paths include `/`, \
`runs/index.json`, \
`runs/<idempotency_key>.md`, `runs/<idempotency_key>.json`, \
`cards/<card_id>/.payload.json`, and `cards/<card_id>/runtime.json`. \
`.payload.json` is the card's own payload; runtime identity/status lives \
in `runtime.json`. These views are own-wave-only; cross-wave reads are forbidden.
";

/// Substitute the per-spawn placeholders into a prompt template. Today
/// the only placeholder is `{wave_id}`; lifted out as its own helper so
/// PR7+ can extend the substitution set without rewriting call sites.
pub(crate) fn render_system_prompt(template: &str, wave_id: &str) -> String {
    template.replace("{wave_id}", wave_id)
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
    /// Spec card minted by `routes::waves::create_wave`. Gets
    /// [`SPEC_SYSTEM_PROMPT_TEMPLATE`].
    Spec,
    /// Worker card minted by the dispatcher. Gets
    /// [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] (PR8 will swap in the
    /// production worker prompt).
    Worker,
}

impl SeededCardRole {
    pub(crate) fn prompt_template(self) -> &'static str {
        match self {
            SeededCardRole::Spec => SPEC_SYSTEM_PROMPT_TEMPLATE,
            SeededCardRole::Worker => WORKER_SYSTEM_PROMPT_PLACEHOLDER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_system_prompt_substitutes_wave_id() {
        let out = render_system_prompt(SPEC_SYSTEM_PROMPT_TEMPLATE, "wave-abc");
        assert!(
            out.contains("wave `wave-abc`"),
            "wave id should be substituted; got: {out}"
        );
        assert!(
            !out.contains("{wave_id}"),
            "placeholder should be gone; got: {out}"
        );
    }

    #[test]
    fn render_system_prompt_preserves_role_template_content() {
        let spec = render_system_prompt(SeededCardRole::Spec.prompt_template(), "wave-abc");
        assert!(spec.contains("You are the spec agent for wave `wave-abc`."));
        assert!(!spec.contains("calm.update_wave_state"));
        assert!(spec.contains("calm.plan.upsert"));
        assert!(spec.contains("calm.plan.list"));
        assert!(!spec.contains("calm.task.dispatch"));
        assert!(spec.contains("calm.task.verdict"));

        let worker = render_system_prompt(SeededCardRole::Worker.prompt_template(), "wave-abc");
        assert!(worker.contains("You are a worker agent under spec card on wave `wave-abc`."));
        assert!(worker.contains("neige task-completed"));
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
                && p.contains("calm.plan.upsert")
                && p.contains("calm.plan.list"),
            "prompt must read state via neige and maintain the plan via MCP"
        );
        assert!(
            !p.contains("calm.update_wave_state")
                && !p.contains("calm.task.dispatch")
                && p.contains("calm.plan.upsert")
                && p.contains("calm.plan.cancel")
                && p.contains("calm.plan.list")
                && p.contains("calm.task.verdict")
                && p.contains("calm.report.write")
                && p.contains("calm.report.edit"),
            "prompt must document retained wave/task write tools and omit retired update_wave_state"
        );
        assert!(
            !p.contains("Call `calm.wave.state`"),
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
            "spec prompt must document reading the report through neige"
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
            "spec prompt must state wave file views are read-only"
        );
        assert!(
            p.contains("runs/K.md"),
            "spec prompt must document the canonical post-completion read"
        );
        assert!(
            p.contains("calm.report.write") && p.contains("calm.report.edit"),
            "spec prompt must document report write/edit MCP tools"
        );
        assert!(
            !p.contains("calm.wave.cat")
                && !p.contains("calm.wave.ls")
                && !p.contains("calm.report.read"),
            "spec prompt must not instruct reads via MCP"
        );
    }

    #[test]
    fn spec_prompt_pins_chinese_current_snapshot_report_semantics() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        // New Chinese section vocab present (all five required H1s).
        for section in ["# 概要", "# 已完成", "# 决策", "# 待你定", "# 进行中"] {
            assert!(
                p.contains(section),
                "Wave Report prompt must document the new Chinese section `{section}`"
            );
        }

        // Old English vocab is explicitly banned (the banned-list bullet must
        // name all of them so future drift back to append-log is structurally
        // discouraged in the prompt itself).
        for banned in [
            "# Goal",
            "# Progress",
            "# Needs attention",
            "# Results",
            "# Timeline",
        ] {
            assert!(
                p.contains(banned),
                "prompt must list `{banned}` in the banned-vocab bullet"
            );
        }

        // The banned-vocab BULLET itself must remain — the migration paragraph
        // mentions the old names too, so a `contains("# Goal")` check is not
        // enough to detect accidental removal of the explicit ban statement.
        assert!(
            p.contains("不要用旧的 `# Goal"),
            "prompt must keep the explicit `不要用旧的` banned-vocab bullet"
        );

        // Current-snapshot semantics: must say REWRITE, must NOT instruct
        // append-to-progress (the original prompt's wording that drove the
        // runaway-journal behavior).
        assert!(
            p.contains("REWRITE"),
            "prompt must use REWRITE to pin current-snapshot semantics"
        );
        assert!(
            !p.contains("append to `# Progress`"),
            "prompt must NOT instruct append-to-progress (root cause of runaway journals)"
        );

        // Length budget present (soft, prompt-only) and process-narration ban.
        assert!(
            p.contains("1000 字") && p.contains("2000"),
            "prompt must declare the body word budget"
        );
        assert!(
            p.contains("写产出，不写过程"),
            "prompt must ban process narration"
        );

        // Migration guidance: an existing English-format report must be
        // rewritten in one shot, not partially edited.
        assert!(
            p.contains("一次性整体 REWRITE") || p.contains("整体 REWRITE"),
            "prompt must give explicit one-shot migration guidance"
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
            p.contains("READ-ONLY") && p.contains("own-wave-only"),
            "worker prompt must constrain neige reads to read-only own-wave views"
        );
    }
}
