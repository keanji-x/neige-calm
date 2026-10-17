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

Each session has a separate build directory, so a failed occupied-port launch cannot rewrite another worker’s assets. Start requires a running supervisor and the same positive main-process identity before and after the HTTP checks. It probes the exact route and its declared initial scripts/styles, checks the unique identity and rejects wrong script/style MIME types, HTML fallbacks, missing assets and redirects. `status` distinguishes ready, unhealthy and offline. Resource checks do not replace a real browser check: exercise the visible route before delivery. A saved URL must be revalidated through status before reuse. Source metadata describes the worktree at launch; it does not promise an immutable source checkout. Keep that worktree in use, and stop the unit before archival or branch changes. Restart after source changes to rebuild production assets.

`forwarding: not-verified` is intentional. The desktop/gateway owns forwarding and does not expose its lease/status through this CLI. Request the desktop to open the URL separately, report queued separately from verified client navigation, and stop/unregister its forwarding through that owner's API before archival. Expiry and gateway coordination remain in #2267; this tool owns only the process, host readiness and explicit shutdown contract.

The motion route includes actual chat/disclosure/progress components and Cards-owned local registration with the production BoardHost/CardHead. Preview cards have no kernel mapping, backend write, terminal or iframe connection. Use delete/reset and interrupt compaction with real pointer drag or southeast resize. No preview-only trajectory or grid implementation is used.

## Track presentation profiling

Build the actual application with embedded source maps, then pass that exact output
directory to the maintained probe from `fe/`:

```sh
node tools/preview/profile-build.mjs
node tools/preview/track-profile.mjs --build /tmp/neige-profile-XXXXXX --count 300 --width 1400 --label candidate
```

The probe starts its own loopback preview of the existing build, opens a fresh
Chromium context and exercises real Track navigation, router, report and Worker
code. Only API wire replies are synthetic. Each report block has distinct prose,
links and numbers; the initial small Track has two blocks. The default flow selects
large → small → large → small → large, covering first entry and two cached returns.
The native mobile workspace controls are used when desktop Track controls are hidden.

`--detail-delay-ms 1000` delays large-Track detail replies. `--scenario rapid`
attempts large → small → large with a 20 ms host delay between selections; it requires
concurrently visible desktop Track controls. Actual pointer-event times and handling
delays are retained, since a busy renderer may delay the attempt. Superseded visits
are recorded as incomplete content visits; only the final selected report must finish.
Use `--reduced-motion` for a separate reduced-motion cohort.

The explicit build's `profile-build.json` and embedded source maps identify its
revision, dirty state, React mode and actual `WEB_COMPAT_VERSION`; the probe never
copies a current version number from another checkout. It hashes all build files
and verifies they remain unchanged. Missing or conflicting source metadata fails.
The output destination is resolved through existing physical ancestors before any
write, including symlink ancestors. It must be outside the build. `--output NEW_DIR`
selects a fresh artifact directory and refuses overwriting prior evidence. The default is an ignored,
unique directory under `fe/test-results/track-presentation/`.

`report.json` retains every attempted selection, observed shell/pending/report state,
first/full fixture heading count, resource timing, Worker create/post/reply/terminate,
main-thread rAF timestamps and observed callback intervals, long tasks, pointer
handling and queued-task pulses, page
errors and build hashes. It includes separate shell, first report, full report and
post-content frame summaries. Frame records retain their target identity, so an old
Track still visible at the first callback is not reported as the selected shell. `track.png` records success; incomplete runs retain
failure details/screenshots and exit nonzero. Cleanup closes only this probe's browser
and preview handle. It does not signal unrelated servers or remove the build/artifacts.

DOM observation does not establish paint, INP, read eligibility or interactive
readiness. rAF gaps describe the main thread, not compositor FPS. Worker observation
adds one reply/error listener, and the queued timer is task-availability evidence,
not actual input response. Full content means every heading in this fixture is in
the DOM and the declared report region is no longer busy; native Find and selection
still need real browser checks separately.

Run normal-production baseline/candidate samples serially without concurrent heavy
verification. Retain every attempted sample, alternate at least three matched pairs
and compare the same count, viewport, scenario and motion preference. Report shell
and body tradeoffs separately. `--cpu` writes a diagnostic `cpu.cpuprofile`; CPU and
`profile-build.mjs --react` cohorts must stay separate from normal-production latency
comparisons. This tool applies no CI latency threshold and claims no smoothness
from a rendering count or DOM timestamp alone.
