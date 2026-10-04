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

Resolution only affects image nodes. Code examples, ordinary links, copied
response text, user attachments, and transcript persistence retain their existing
contracts. The image renderer owns sizing so screenshots fit narrow drawers.

Review tier: L1. This consumes the existing workspace read boundary without
changing authorization, persistence, or server file access.
