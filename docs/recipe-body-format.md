# Template and recipe working instructions

Selecting a starting point supplies the Planner with a working method and a
report format. It does not enqueue a checklist of tasks. The user's message
supplies the actual request; a discussion-only request does not become an
execution request merely because a template was selected.

## Write the method in ordinary Markdown

A saved recipe has a title and a Markdown body. Describe the desired approach,
constraints and expected results. The Planner receives this content at thread
startup, before its first user turn, without needing to read the report to
discover it.

For example:

```markdown
<!-- Working method
Investigate the requested behavior using code, documentation and tests.
Do not modify the repository. Distinguish observations from inferences.
Delegate concrete independent investigations only when useful.
-->

# Findings

# Evidence

# Open Questions
```

Keep working instructions in a closed HTML comment when they should not appear
in the rendered report. The Planner receives comments as well as visible prose.
Report prose and non-task blocks (such as tables and applications) remain in
the initial report. Reports with a machine-readable section contract must keep
its canonical header on the first line; see the built-in template files for
examples.

Built-in and operator templates use the same body format after their existing
`+++` TOML front matter (`id` and `title`). No new template-file fields are
required.

## Tasks are created for actual work

Do not write `neige-block task` fences to preallocate generic steps such as
inspect, implement and verify. The Planner creates concrete delegated tasks
when the actual request calls for them. Task dependencies, verification gates,
capacity limits and required user approvals continue to apply.

Existing recipes and operator files may still contain task fences. They are
validated and retained in the startup snapshot as reference material, but they
do not instantiate task blocks or queued attempts in a new track. The template
listing therefore returns an empty `tasks` array. This does not cancel or
remove tasks in tracks that already exist.

Recipe saves still canonicalize structured fences, remove retired task
tombstones and source-track references, reset task authorship/readiness and
remove copied user releases. These transformations prevent old declarations
from carrying another track's authority or references. A saved task example
is not an executable task and cannot grant permission.

## Creation-time snapshot

A named starting point (built-in template, operator template or saved recipe)
is copied into a kernel-owned `template_context` field on the Planner card,
in the same transaction that creates the track. Its required fields are
`version: 1`, `title` and `body`. The existing card events retain this snapshot
for replay. Ordinary card writes cannot replace or erase it.

Fresh Planner threads, including reset threads, receive that snapshot. Editing
or deleting the source recipe, changing operator files, upgrading built-in
templates or editing the report does not change an existing track's snapshot.
A snapshot is not the current report: the Planner still reads the latest
report before editing it.

The snapshot is not injected into assistant conversations or plain chat.
Plugin inputs and tool permissions still require the current plugin binding;
template prose grants no authority.

Tracks created before this change have no snapshot and keep their existing
report-read behavior. No migration guesses their original template version
or rewrites their tasks. Explicit report forks copy report content and retain
their existing task handling, but do not inherit a Planner startup snapshot.
