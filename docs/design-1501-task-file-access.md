# Read a completed task's reported file (#1501)

## One outcome

After an independent Codex task completes, a person opens a file listed in its
accepted report, sees a bounded plain-text preview when possible, and downloads
the file. The same action works from historical attempts after refresh or worker
card deletion. This slice supports one regular file at a time, up to 8 MiB.

Reuse the existing accepted report, retained workspace and namespace stop proof.
No new artifact ledger, snapshot publication, directory archives, share links,
repository input, or generic file browser. Bytes are read from the retained file
at request time; this does not introduce an immutable content-addressed delivery.

## Frozen HTTP contract

Add authenticated User-only:
`GET /api/tracks/{id}/tasks/{key}/attempts/{attempt_id}/artifacts/{index}`

`index` is the zero-based index in this exact attempt's accepted completed report.
The client supplies no filesystem path. Success 200 is JSON:
`{ "attemptId": "...", "index": 0, "name": "result.txt", "size": 3, "contentBase64": "NDIK" }`.
Required fields, exact byte count, standard padded base64 (empty for empty files).
The server DTO is `TaskArtifactFileResponse`; frontend uses a strict core/domain
schema, including expected attempt/index refinement and bounded sizes.

Errors: 401/403 authentication/actor; 404 wrong scope, no declared artifact or
unavailable regular file; 400 unsupported artifact reference; 409 execution or
workspace stop/ownership cannot be established; 413 exceeds 8 MiB. Errors must
not disclose host paths, private journal values or credentials. Use no-store and
nosniff for file responses. Existing report responses remain unchanged.

A bounded JSON response reuses the existing transport and error handling. The
app decodes exact bytes, previews valid UTF-8 without NUL as escaped text (at
most 65,536 characters with a truncation indication), and downloads a Blob with
application/octet-stream. Binary files still offer download. Fetch only after
an explicit file action; release byte/object-URL resources on close or unmount.
No arbitrary model-provided string becomes a navigation URL or rendered HTML.

## Read authority and filesystem boundary

- Reuse one canonical exact-attempt accepted-report lookup, preserving all
  existing event/actor/Track/task/Operation provenance checks. Select a declared
  artifact by index from an accepted completed report, not from event guesses,
  a request path, process output or another attempt's report.
- Require Task done and its exact isolated Operation succeeded, no ambiguous
  operation/verification/spawn/compensation evidence, closed start admission and
  the matching persisted namespace quiescence proof. Share the private stop
  identity validator with retry, keeping retry's failure-only policy unchanged.
  Missing/live/unknown/mismatched evidence stays unavailable. Do no filesystem
  IO while holding the database writer transaction.
- Support ordinary relative references (including `./result.txt`) and
  `/workspace/<relative>` only. Normalize harmless current-directory segments
  before checking the reserved `.codex` subtree; reject parent traversal, other
  absolute paths, URLs, empty segments, backslashes and controls. Never fall back
  to the Track workspace.
- The retained kernel workspace and its adjacent `.owner` marker bind the
  directory inode to the original operation. Open/validate that directory without
  following symlinks and retain the validated descriptor through the file open.
  Reuse the existing secure workspace opener through a narrow descriptor-based
  entry instead of checking then reopening paths or duplicating its syscall logic.
- Refuse symlinks in every component, magic links, non-regular files and hardlinks;
  a FIFO must not block. Cap actual bytes read even if the file's size changes.
  Preserve existing generic workspace/attachment callers' behavior. Map opener
  errors to public file errors without exposing host paths.

## Acceptance

1. Real production task creates/reports a file, completes and stops; authenticated
   exact-attempt API returns its exact bytes/name/size. Browser preview and saved
   download match, including after refresh/history/card deletion.
2. Wrong Track/key/attempt/index, undeclared files, missing/live/forged stop proof,
   different workspace, path traversal/private paths, symlink/ancestor/root swaps,
   hardlinks and special/oversized files fail closed without leaks or blocking.
3. Text/HTML-like/binary/empty/Unicode filenames, loading/error/retry behavior,
   download bytes and object-URL cleanup are checked through real app components
   and browser actions. Keep current task start/retry/history regressions green.
4. Use exact production mutations for the few critical report/proof/root/file
   fences; two independent full reviews and relevant local/CI gates before squash.

## Narrow ownership decision (CR-1501-FILES)

The orchestrator approves the endpoint above, a core/domain file operation/schema,
and optional report rendering composition for the file actions. The frozen generic
transport, global styles and other interfaces remain unchanged. The real generator
updates the frozen frontend OpenAPI document; preserve this exact commit/PR trailer:

OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — add approved completed-task file read contract (#1501)
