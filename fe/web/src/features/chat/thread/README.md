# Conversation replies

Stored and streamed agent replies use the same Astryx Markdown renderer. The app
binds `ChatThread.imageFiles` to the open conversation's Track workspace, using
`Track.agentCwd` and the existing Track-scoped raw image endpoint.

To display a screenshot or other local image, save it in that workspace and
include a Markdown image reference in the reply:

```markdown
![Screenshot](screenshots/page.png)
```

Workspace-relative paths, absolute paths inside the workspace, and `file://`
paths inside the workspace resolve through the same file port. Encode spaces in
destinations as `%20` (or use Markdown angle-bracket destinations). Paths outside
the workspace are unavailable; the server also enforces containment when opening
the file, including symlinks. Network images retain their URL. Failed image loads
show an unavailable message with the alternative text.

Image resolution only affects image nodes. The app also binds `ChatThread.renderLink`
to the report-owned `ProseLink` entry point for workspace files, source citations,
report references and external previews. History and live replies share that
renderer; files and sources use the open conversation’s Track. Preview reads are
lazy and external pages still require explicit loading. Code examples, copied
response text, user attachments, and transcript persistence retain their existing
contracts. The image renderer owns sizing so screenshots fit narrow drawers.

Review tier: L1. This consumes the existing workspace read boundary without
changing authorization, persistence, or server file access.

`message-entry.tsx` owns ordinary user/agent message presentation. The transcript
owner supplies scalar visual fields, attachments, the gap caption, queued/edit/
replacement marks, live status and the existing scoped image port. Memoization
leaves unchanged stored rows alone while the live tail grows, even if upstream
replaces transcript objects. Changed words or marks still update the same node.
All history remains mounted: this optimization reduces React work, not DOM count,
and preserves exchange markers, browser selection and existing navigation.

Measured through `app/router/chat-performance.browser.test.tsx`, with identical
20/120/300/900-row fixtures and no CI timing threshold. True DOM windowing needs a
separate contract for variable heights, history anchors, jumps, focus and cross-row
selection; #2235 tracks that remaining decision.
