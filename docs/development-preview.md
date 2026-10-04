# Neige development preview

When using a Neige development track for this repository, publish visible frontend changes using the following workflow.

When the change touches calm's web frontend (`fe/`), put the running result in the report.
Open a terminal in the task worktree with neige_terminal_open and start, each on a free port:

- the backend: `CALM_PUBLISH_ADDR=127.0.0.1 CALM_DEV_AUTOLOGIN=true make dev-fresh DEV_ID=<short track id> CALM_PORT=<port>`.
  Autologin is required: the preview gateway never forwards calm's session cookie, so the
  dev stack must not ask for a login. Autologin makes anyone who can reach the port the
  owner, so the stack is published on loopback only (make refuses autologin otherwise).
  It first compiles release binaries (several minutes, CPU-heavy: on a busy host set
  `CARGO_BUILD_JOBS`) and builds the tailnet helper with Go: if it stops because `go` is
  not found, put the Go toolchain on PATH and rerun.
- the frontend (after `npm --prefix fe ci` if needed):
  `FE_API_PROXY_TARGET=http://127.0.0.1:<CALM_PORT> FE_DEV_HOST=127.0.0.1 FE_DEV_PORT=<port> npm --prefix fe run dev`.
  The gateway only reaches 127.0.0.1, and vite's default host `localhost` may bind IPv6 only.
Then call neige_preview_register {key:"fe", target_port:<FE_DEV_PORT>, title} and put its
block_hint into the report with `path:"/next/"` added (a block upsert or a report commit). When the work is done, stop both and call
neige_preview_unregister {key:"fe"}.
Self-checks against the dev stack (curl, Playwright) run in a worker task, not in your own
shell.
