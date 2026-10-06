# Component preview sessions

On a Linux development host with a systemd user manager, install the locked frontend dependencies and use:

```sh
node fe/tools/preview/session.mjs start --port 5224
node fe/tools/preview/session.mjs status --port 5224
node fe/tools/preview/session.mjs restart --port 5224
node fe/tools/preview/session.mjs stop --port 5224
```

The managed entry defaults to a production build. Add `--mode development` for live editing or `--kind links` for the link surface. Configuration is passed through CLI arguments; the worker binds loopback and refuses an occupied port. The raw worker remains `node fe/tools/preview/server.mjs <port> <kind> <mode>` (development by default) for foreground debugging.

A transient user unit keeps the worker independent of the requesting shell. It restarts after normal or failed exits, waits three seconds between attempts and permits at most three starts in a minute. Stop is explicit. The unit records owning worktree, launch revision, dirty state, preview kind/mode/port and a unique session identity. Commands refuse a unit from another owner or kind; changing an existing session's mode requires explicit restart.

Start probes the exact route and its declared initial scripts/styles, checks the unique identity and rejects HTML fallbacks, missing assets and redirects. `status` distinguishes ready, unhealthy and offline. Resource checks do not replace a real browser check: exercise the visible route before delivery. A saved URL must be revalidated through status before reuse. Source metadata describes the worktree at launch; it does not promise an immutable source checkout. Keep that worktree in use, and stop the unit before archival or branch changes. Restart after source changes to rebuild production assets.

`forwarding: not-verified` is intentional. The desktop/gateway owns forwarding and does not expose its lease/status through this CLI. Request the desktop to open the URL separately, report queued separately from verified client navigation, and stop/unregister its forwarding through that owner's API before archival. Expiry and gateway coordination remain in #2267; this tool owns only the process, host readiness and explicit shutdown contract.

The motion route includes actual chat/disclosure/progress components and Cards-owned local registration with the production BoardHost/CardHead. Preview cards have no kernel mapping, backend write, terminal or iframe connection. Use delete/reset and interrupt compaction with real pointer drag or southeast resize. No preview-only trajectory or grid implementation is used.
