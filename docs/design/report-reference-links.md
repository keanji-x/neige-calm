# Report reference links

## Outcome and review

Prose, task goals, acceptance and withdrawal reasons share the report Markdown
and link contract. Explicit Markdown links, issue/PR references and qualified
source paths are navigable/previewable. Terminal commands, gate commands, fenced
code and ordinary code identifiers remain literal.

Review tier L2: new automatic destinations must retain repository and file-root
admission, and the change spans structured fields and report consumers.

## Ownership and admission

- Extract the existing report prose/inline renderer into the same document owner.
  Task fields consume that renderer; no second Markdown parser or preview policy.
- Pure report-reference recognition belongs in core/domain. Issue/PR shorthands
  use a typed repository context derived only from explicit GitHub links in
  visible report prose. Hidden HTML/comments, code, absent or mixed repositories
  cannot supply context. No project/template/repository identity is hardcoded.
- Recognize only clear qualified file paths; existing file parsing and injected
  workspace-root admission decide whether they become controls. Preserve display
  spelling and support existing source-position suffixes plus line ranges.
- Do not auto-fetch referenced files/images while rendering. Preview reads remain
  lazy and use the existing owning ports. Terminal/gate strings are never parsed.
- Preserve heading/outline identifiers, task disclosure/status, keyboard focus,
  escaping, source ownership and existing explicit navigation contracts.

## Evidence and acceptance

4140's #2420 report had a clickable explicit link in prose but a plain `issue
#2420` in a task goal. ReportTaskBlock inserts goal and acceptance as strings.
The code-reading change #2480 did not alter those fields.

Add a red production-ReportDocument regression for this exact shorthand and task
field Markdown; then cover multiple repositories, absent context, unsafe links,
file-root escapes, line/range suffixes, terminal literals and existing links.
Actual Chromium must verify hover, click, disclosure and desktop/mobile layouts.
Mutation-verify the small admission assertions in an exclusive worktree; run
focused tests, text gates, frontend lint/build/unit gates and required CI. L2
reviews use separate frozen worktrees, rechecked as the repository requires.
