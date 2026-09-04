# Recipe body format

An introduction to what you type into the recipe editor. It is not a full
field reference — it is enough to write a recipe that works, plus the two
mistakes that are easy to make and hard to notice.

A **recipe** is a report you keep: its title becomes the created track's
report summary, and the task blocks in its body become that track's tasks.

## 1. The body is just Markdown

Headings, paragraphs, lists, links — write whatever you want. Prose is stored
exactly as you typed it, byte for byte. The only special thing in a recipe
body is the task block described below.

## 2. A task is a fenced code block

A task is a fenced code block whose **info string** is `neige-block task`, and
whose interior is a single JSON object. Everything else in the body is prose.

Here is a complete, working two-task recipe body. Copy it and edit it:

````markdown
# Plan

Read the change, make it, prove it. Ordinary Markdown: this paragraph is
just prose and is stored exactly as written.

```neige-block task
{
  "key": "inspect",
  "kind": "codex",
  "goal": "Read the requested change and the code it touches.",
  "acceptance": "The change request and the affected code path are written down in this report.",
  "depends_on": [],
  "no_gate_reason": "inspection produces no repo change to verify"
}
```

```neige-block task
{
  "key": "implement",
  "kind": "codex",
  "goal": "Make the change and commit it.",
  "acceptance": "The change is committed in the track worktree.",
  "depends_on": ["inspect"]
}
```
````

The fence lines are strict:

* the opener is **exactly** three backticks at the start of a line (no
  indentation), then the literal word `neige-block`, one space, then the block
  kind;
* the interior must be one JSON **object** — no trailing commas, no comments;
* the closer is a line containing only three backticks, also unindented, with
  nothing after it.

A fence that does not meet all of that is not a task. See the next section for
what happens then.

## 3. The two `kind`s — read this one

There are two different things called `kind`, one line apart, and they mean
different things:

````text
```neige-block task     ← the BLOCK kind, in the fence info string
{
  "kind": "codex",      ← the TASK kind, inside the JSON payload
  ...
````

* The **block kind** is always `task` for a task. (Other block kinds exist —
  `chart.candles`, `table`, `app` — but a recipe is normally all prose and
  tasks.)
* The **task kind** says who runs the task: `"codex"`, `"claude"` or
  `"terminal"`.

They fail in opposite ways, and that asymmetry is the reason this section
exists:

**Getting the outer one wrong can fail silently.** If the opener is not
recognisable as a `neige-block` opener at all — you wrote ```` ```task ```` or
```` ```json ```` instead, or indented the fence, or used four backticks — the
server does not see a block. It reads the whole thing as prose, stores it
verbatim, and answers **201/200**. Nothing is wrong on screen; you simply get
a recipe with zero tasks, and you find out when the track you create from it
has no work in it. Verified: a body whose fence opener is ```` ```task ````
saves successfully and comes back with the fence untouched, `"kind": "codex"`
and all.

(If the opener *is* a `neige-block` opener but the block kind is misspelled —
```` ```neige-block tasks ```` — you do get a 400: ``unknown block kind `tasks`
— known data kinds: chart.candles, table, app, task``.)

**Getting the inner one wrong is a clean 400.** `"kind": "gpt"`, or no `kind`
at all, is rejected at save with an exact message:

```
track recipe body: invalid `task` block payload:
kind: required; must be one of "codex" | "claude" | "terminal"
```

So: a save that errors tells you precisely what to fix. A save that *succeeds*
but produced no tasks is the case to watch for — check that your saved recipe
renders the task fences as blocks, not as a code listing.

## 4. What the server rewrites when you save

The body you send is **not** stored byte for byte. Before storing, the server
normalizes every task block. That is why the editor shows you the server's
response after a save instead of your draft: the rewrite is something you
should see the moment it happens, not the next time you open the recipe.

What changes:

* **Every parseable `neige-block` fence is re-rendered into canonical form** —
  JSON keys sorted alphabetically, two-space indent, arrays of scalars kept on
  one line. Your key order and spacing inside a fence are not preserved.
* **Three task fields are forced to server-chosen values**, so a recipe cannot
  carry one track's authority into every track made from it: `declared_by` and
  `ready` are overwritten with the two values shown in the round trip below,
  whatever you wrote, and `released_by_user` is removed entirely.
* **Tombstone blocks are dropped**, leaving a blank line where they were. A
  tombstone would block re-declaring its key in every track made from the
  recipe.

Prose is untouched, and so is any fence the parser could not read (that is the
silent case in §3).

Here are the first two of those, on a round trip that was actually executed
against the server. What you send:

````markdown
Prose is stored exactly as written.

```neige-block task
{"kind": "codex", "key": "implement", "goal": "Make the change.",
 "declared_by": "user", "ready": true, "released_by_user": "local-owner"}
```
````

What comes back — and what is stored:

````markdown
Prose is stored exactly as written.

```neige-block task
{
  "declared_by": "spec",
  "goal": "Make the change.",
  "key": "implement",
  "kind": "codex",
  "ready": false
}
```
````

The prose line came back byte-identical. Inside the fence, the keys are sorted
and re-indented, `declared_by` and `ready` hold the server's values rather than
yours, and `released_by_user` is gone.

## 5. The fields you actually need

For an ordinary task block, only three fields are required from you:

| Field  | Required | Value |
| ------ | -------- | ----- |
| `key`  | yes | Identifier for the task, matching `^[a-z0-9][a-z0-9._-]{0,63}$` |
| `kind` | yes | `"codex"`, `"claude"` or `"terminal"` |
| `goal` | yes | Non-empty string: what the task is to achieve |

Useful optional ones:

| Field | Value |
| ----- | ----- |
| `acceptance` | Non-empty string: how you will know it is done |
| `depends_on` | Array of other tasks' `key`s; omit or `[]` for none |
| `no_gate_reason` | Non-empty string: why this task has no verification command |
| `gate` | A verification command for the task |
| `cwd` | Absolute path |
| `priority` | Integer |

`declared_by` and `ready` are also required by the schema, but you do not have
to write them — the server sets both (§4) before it validates.

Anything not in the task vocabulary is rejected, so a typo'd field name is a
400 rather than a field that silently does nothing.
