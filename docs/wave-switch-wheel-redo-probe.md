# wave-switch-wheel-redo-probe

## 段 A 实测：实际 route.kind = xterm, activeCard = true

Probe confirmed `useWheelRouter.handleWheel` was called after wave switch.

Raw failing route:

- `route.kind`: `xterm`
- `activeCardFound`: `true`
- event target / point target: `.xterm-screen`
- `pointWheelCard`: restored RGL `.wave-card` with `data-card-id`
- `decide()`: `{ kind: "pass", reason: "edge" }`
- xterm adapter state: `lines=-19`, `bufferType=normal`, `viewportY=0`, `baseY=0`
- `.scroll.scrollTop`: `589 -> 289` (`outerDelta=-300`)
- `.xterm-screen` text: unchanged

Second instrumented run captured why xterm had no scrollback:

- `ServerHello.snapshot`: `scrollbackBytes=3961`, `dataBytes=764`, `cols=60`, `rows=27`
- immediate `RenderSnapshot`: `scrollbackBytes=0`, `dataBytes=764`, `cols=60`, `rows=27`
- old client path called `term.clear()` for that no-scrollback `RenderSnapshot`, wiping the just-restored local xterm scrollback ring.

After the fix, the same probe showed:

- `route.kind`: `xterm`
- `activeCardFound`: `true`
- xterm adapter state: `lines=-19`, `viewportY=176`, `baseY=176`
- `decide()`: `{ kind: "consume" }`
- `.scroll.scrollTop`: `545 -> 545` (`outerDelta=0`)
- `.xterm-screen` text changed from `wheel-redo-174...` to `wheel-redo-156...`

## 段 B fix：改了 XtermView 的 snapshot replay

Changed `web/src/XtermView.tsx` as a 10-line diff across 3 code sites:

- `ServerHello.snapshot.data` final write now calls `term.scrollToBottom()` after replay.
- `RenderSnapshot` no longer calls `term.clear()` unless the snapshot includes replacement `scrollback`.
- `RenderSnapshot.data` final write also calls `term.scrollToBottom()`.

The key behavior change is preserving existing xterm scrollback on no-scrollback resize/resync snapshots. `TerminalModel::snapshot_vt()` already emits `ESC[2J` to repaint the viewport without clearing scrollback, so unconditional `term.clear()` was too destructive.

Temporary probe hooks in `useWheelRouter.ts`, `xtermAdapter.ts`, and `XtermView.tsx` were removed.

## 段 C regression spec：本地跑结果

Added `web/e2e/wheel-wave-switch-routing.spec.ts`.

Spec hardens the prior flaky setup by:

- setting viewport to `1280x1600`
- logging in with `owner/dev`
- waiting 500 ms for RGL settle after wave switch
- asserting `.xterm-screen` center is inside viewport before wheeling
- asserting `.scroll.scrollTop` is unchanged after wheel-up
- asserting `.xterm-screen` text changes after wheel-up

Validation:

- `pnpm exec tsc -b`: pass
- `pnpm exec eslint . --max-warnings 0`: pass
- `pnpm exec vitest run src/input/`: pass, 2 files / 40 tests
- `pnpm exec playwright test --config=/tmp/probe.playwright.config.ts wheel-wave-switch-routing.spec.ts --reporter=list`: pass, 1 test
