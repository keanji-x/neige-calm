# Sheet motion port: single nonmodal owner

Status: footer `87c6e2cd` is accepted. Planner froze design event 518614 for the single ConversationSurface migration; the Sheet owner now consumes the formal PR3014 signed wall-clock sample on upstream `d9b58c672f866e561e40b045b6d5df6c85e0214f`; fresh independent reviews and final integration remain pending. Survey event 517750 supplies the public API evidence. The following contract records that decision, not an implementation pass.

Review tier: L2 because retained native content crosses an interaction/focus authority boundary. Formal independent reviews, original-page preview, stack Tier 1 and exact-head CI remain Planner work.

## Formal upstream sample integration

The kernel checked out upstream `d9b58c672f866e561e40b045b6d5df6c85e0214f`.
Origin is `https://github.com/keanji-x/neige-calm.git`; `git merge-base --is-ancestor
d6b4dda26b38eb8cbc6183ee2a532208f0cf96bd HEAD` succeeded. The local `origin/main`
reference remained at the old base, so all comparisons use explicit verified SHAs.
Old cumulative base/head: `cdbfeea73c137ec6087955e7b22efb8aa422e8e9` /
`9595f1106b322166b395b832bb94b42d3833240d`. `git diff` between those two,
then `git apply --reject /tmp/sheet-cumulative.patch`, applied all 15 files without
rejects or conflicts. No PR2854/2903/2965/shared patch was replayed. Upstream's
Drawer/hover sampling-test revisions remain intact. The inventory adds exactly
one Sheet directory, with the canonical trailer below.

The authoritative contract is PR3014's squash body and production
[`spring.ts`](../fe/web/src/ui/motion/spring.ts), including its whole native-control
and spring browser tests: fill both, current time clamped to trajectory endpoints,
primary public play state/startTime/playbackRate, signed velocity per wall-clock
second. Pending controls use currently exposed facts, not a guessed future ready
state. Paused/zero-rate clocks have zero momentum; a running negative initial time
uses the initial endpoint derivative, including a nonzero initial derivative.
No local rate multiplication, second solver or shared implementation edit exists.
The old sampler at `9595f110:fe/web/src/ui/motion/spring.ts` returned the raw
trajectory derivative. This source and the inherited before logs below remain
historical evidence; the already-fixed shared semantics are not given a fabricated
red by mutating shared code. Current tests consume the real public Sheet and
ConversationSurface, call through the spring factory, and compare actual panel
rectangles against an independently measured unanimated frame and native time
differences. They include running +1/-1/.5, reverse, paused/zero, pending rate and
pause before/after ready, pause/seek/resume, grab/release/reopen/frame and explicit
negative initial/terminal endpoints. New tests live in existing owner targets;
there is no permanent roster or extra CI invocation.

A real Sheet owner defect remained after replay: an extant playback prevented
synchronous settlement even at exactly the destination with zero sampled velocity.
The valid pre-fix public Sheet run `/tmp/sheet-signed-valid-red.{json,log}` reports
24 green / exactly three behavioral reds: paused and zero-rate entry at the closed
endpoint leave `dialog.open` true, and a paused terminal rebase creates an unnecessary
Animation. Sheet now settles exactly `q===target && v===0` synchronously regardless
of whether it previously held a playback. This changes only its owning motion
adapter. It preserves native cleanup, current intent/focus guards and nonzero
momentum. The first trial's six negative-zero comparisons and two incorrect skirt
endpoint assumptions are invalid fixture evidence, preserved separately in
`sheet-signed-new-red`; they are not counted as production reds.

The inherited paused cap-outside no-displacement test expected trajectory momentum.
The initial full owner/production run (`sheet-signed-baseline`) had 55 green / that
one failure. Its assertion now explicitly expects zero paused wall-clock momentum;
its independent nonzero native trajectory slope and actual subsequent paint remain
checked. This is an intentional test contract update, not a relaxed budget.
An initial new-test invocation from repository root failed npm EROFS; only corrected
commands from `fe/` count. No dependency install or evidence relocation was performed.

This integration is behavioral (formal sample consumption and synchronous endpoint
settlement), so fresh dual L2 review must cover the complete cumulative head and
its delta. Old original-page/Tier1/15-E2E results at older heads do not certify this
tree. Planner must rerun invalidated integration checks using newly built/served
artifacts; the old 4360/4361 binaries are not new-head evidence. No ADB, real model,
physical keyboard/device matrix, broad suite or review is claimed here.

### Integrated verification and mutation evidence

Focused command: `python3 /tmp/sheet-focused.py sheet-signed-green` (repository
root; the script records the complete Vitest argv with `cwd=fe`, both browser
projects, all 16 complete observable targets, `--passWithNoTests=false` and four
workers in `/tmp/sheet-signed-green-summary.log`). Result: 265/265 green.
A fresh `python3 /tmp/sheet-signed-import-sweep.py` still finds the same 90 reachable
tests (`/tmp/sheet-signed-reachable-tests.txt`); the inherited whole-target scope
and exclusions below still apply. No new caller or test registry was introduced.
The two old zero-entry retention fixtures (fractional closed travel and rapid
native reopen) now enter or settle first, retaining every original geometry/focus
assertion. The initial 263/265 run is preserved as `sheet-signed-pregreen`.

Before any mutation, `python3 /tmp/sheet-signed-plan.py` serialized every predicted
failing name in `/tmp/sheet-signed-mutation-plan.json`. Each command
`python3 /tmp/sheet-signed-mutate.py <factor>` ran all 16 whole targets / 265 tests,
asserted the single production replacement, restored bytes in finally, compared
SHA-256 and replayed the same full selection green. Each prefix
`/tmp/sheet-signed-<factor>-` has declaration/result, complete red/restored JSON,
logs and command summaries. No shared code/test was mutated; no concurrent reader
or writer was launched.

| Factor | Exact complete red count | Restored green |
| --- | --- | --- |
| active-momentum: zero only an active Sheet sample's inherited velocity | 21 | 265/265 |
| grab-target: replace acquired position with the open target | 17 | 265/265 |
| zero-velocity: old factor zeroing all spring input momentum | 41 | 265/265 |
| endpoint: reintroduce the old active-playback settlement condition | 3 | 265/265 |

The active-momentum factor uniquely checks the signed handoff after the genuine
handle/release seed, while old zero-velocity also observes the seed itself.
Grab/zero predictions were expanded for all new observable cases. Acquisition cap,
cancel, stale completion, focus revoke and retained footer factors/branches are
unchanged: the new cases have no qualified cancellation, injected stale completion,
external focus choice or closed-footer action. Their earlier valid evidence is
retained below; endpoint mutation independently verifies the new synchronous path.

One first active-momentum restoration had 264/265: the unchanged coarse queue
control tap did not invoke delete. Its red set still matched exactly 21 and bytes
were restored; this is not counted as a full valid cycle. Original JSON/result
are preserved under `/tmp/sheet-signed-active-first/`. An unchanged isolated whole
coarse target passed 2/2 (`sheet-signed-coarse-control`), then the identical full
mutation cycle matched 21 and restored 265/265. The root of that isolated missed
tap was not established and is not called fixed or attributed to the environment;
no test retry, timeout or assertion was changed. Subsequent three mutation cycles
all restored 265/265. Preserve this limitation alongside the historical input
observations below.

Complete hover-owner command (cwd `fe`):
`npx vitest run --project browser --project browser-coarse --project web-dom web/src/ui/hover-preview --passWithNoTests=false --maxWorkers=4 --reporter=default --reporter=json --outputFile=/tmp/sheet-signed-hover.json`.
All 9 targets / 159 tests passed in 360.21s (`/tmp/sheet-signed-hover.log`).
Together with the 265 focused cases this is 424 current-tree focused checks,
not a workspace-wide suite. Kernel alone owns text/lint/build and its declared
browser replay; its exact committed range-diff and verdict are delivered with
the task result. No source change follows the final mutation restoration.

## Executable owner contract (design 518614)

L2: native host, retained interaction and focus authority. Only `ui/sheet` owns native `dialog.show()/close()`, unanimated frame, painted panel, opaque pointer lease, one stable portal/footer host, retained permissions and final-exit identity. App supplies intent, refs, viewport-derived frame and business content. Desktop stays Drawer. This migrates only one info/nonmodal/no-scrim/no-snap caller; vendor Switcher/detents and other callers remain untouched.

All props are required (nullable refs are explicit):

```ts
type SheetFrame = Readonly<{ heightPx: number; bottomInsetPx: number }>;
type SheetCloseReason = 'handle' | 'escape' | 'platform';
type SheetProps = Readonly<{
  enabled: boolean;
  open: boolean;
  label: string;
  frame: SheetFrame;
  children: ReactNode;
  footer: ReactNode;
  desktopFooterRef: RefObject<HTMLDivElement | null>;
  dialogRef: Ref<HTMLDialogElement>;
  panelRef: Ref<HTMLDivElement>;
  finalFocusRef: RefObject<HTMLElement | null>;
  focusInput: boolean;
  resolveInitialFocus: (footerHost: HTMLElement) => HTMLElement | null;
  onCloseRequest: (reason: SheetCloseReason) => void;
  onFooterHeight: (heightPx: number) => void;
}>;
```

Refs observe nodes, never authorize caller paint. Height is visible content excluding the 48px skirt. Closed travel is the real panel border-box height. Only `useVisibleViewport` reads viewport geometry; app preserves 80%/92%/fullscreen, navigation clamp, message scroll/padding and footer overlap. Footer is a direct dialog child outside panel transform, with the same bottom inset. One portal preserves real editor DOM/draft/contenteditable selection; reuse drawer retention helpers, not a second selection algorithm.

Handle alone admits primary button 0 immediately, prevents default to preserve focus and uses `touch-action:none`. Header/messages/footer never promote to drag. Downward raw offset paints 1:1; upward paints `max(-48, raw * .35)`. Flick is strictly `abs(rawVelocity) > 1.2px/ms && abs(travel) > 48px`; direction follows total travel. Otherwise dismiss strictly when raw offset exceeds .4 times visible height. Raw velocity classifies intent only. Shared spring receives the derivative of actual paint: 1 downward, .35 in uncapped upward travel, 0 beyond cap, with directional derivative at the kink.

Grab samples active native playback before removing its effect, writes equal inline paint, then cancels and acquires a lease. Release uses last painted q/v, clears lease before capture release, and requests controlled close synchronously; an unaccepted close returns toward open. Reentrant intent must invalidate old release authority. Cancel/lost capture/contextmenu/blur/hidden never dismiss. Close/presentation switch/unmount revoke rather than spring back open. All callbacks check lifetime, intent/playback identity and actual node. Reversal inherits q/v. Frame updates preserve motion contribution q/v while normal layout movement proceeds; do not compensate frame-origin layout shift. Reduced motion reaches the target immediately. Unmount cancels effects, capture, observers, callbacks and queued work; no idle frame loop.

Logical close immediately makes panel and footer inert/aria-hidden and removes dismissal authority. Only the current final exit closes the host and restores connected final focus once; reopen invalidates old completion. Layer dismissal uses the public stack, preserves IME and nested preview ordering, and leaves background interactive. Delayed `focusInput` must stop when the user chooses another focus target. Native/desktop host moves consume a validated contenteditable snapshot once. Full input/textarea selection is not supplied by the existing helper and must not be claimed without separate owner evidence.

Implementation split: `ui/sheet/public.tsx`, same-directory gesture/playback/CSS and browser/coarse tests, each below 800 lines; app retains only assembly and removes obsolete vendor child selectors. Only approved registration changes `fe/module-file-inventory.yaml`; no global styles/rules/dependencies/shared motion edits. Required canonical commit/PR trailer (one physical line):

```text
OWNERSHIP-CHANGE: fe/module-file-inventory.yaml — Register the single nonmodal Sheet UI owner (#3008)
```

## Verification and approved hover notification

Before replacement, add real old-ConversationSurface behavior tests for entry grab continuity and dismiss-qualified pointercancel; no missing-symbol or absent-spy red. Reopen/release are evidence only if actual paint reproduces. Use independent rect/ruler and native timeline phases, then actual paint derivative, never a copied solver. Worker runs focused whole targets and production single-factor mutations; kernel owns text/lint/build and focused replay; Planner owns two independent reviews, original-page/Tier1 and exact-head CI. Do not run broad suites here.

Final focused targets are the two complete `ui/sheet/public` browser/coarse files, `app/router/mobile-conversation-motion.browser.test.tsx` and existing mobile-conversation browser/coarse plus mobile-chat targets. Mutation observable scope must be re-derived from final imports/branches, including relevant production integrations rather than assuming this gate list is exhaustive. Before each grab-target, zero-velocity, cancel-as-release or stale-exit-guard mutation, declare the complete failing names, change one production factor, run every observable whole target, compare all failures, restore bytes/hash, replay green and inspect residue. Existing footer mutation evidence below remains separate.

Planner approved `ui/hover-preview/visual-change.ts` and its same-owner consumer/tests for this attempt. `notifyAncestorVisualChange(ancestor: HTMLElement): void` synchronously transports a DOM notice in that ancestor's document. The consumer checks current requested admission and only its anchor, reading/avoid surfaces or typed parent dependency chain. A notice never opens, re-admits or revives retained paint. Dependencies read current injected ports, including through retained parent tokens. There is no module registry, additional RAF, animation scan or caller dispatch of private `nc-preview-layout`. Existing preview order and parent ports still own propagation.

Sheet publishes after direct paint and through the current shared Playback visual callback; hold/disposal removes the callback before cancelling. The hover regression separately proves actual transform displacement does not notify ResizeObserver, then exercises moving ancestor, nested parent-port propagation, stationary unrelated footer, revocation and cleanup. Sheet's integration test uses actual native pause/seek/resume; preview reservations are deliberately non-overlapping so its stationary-footer assertion does not contradict the existing earlier-portal obstacle policy.

Historical pre-integration status (superseded by the upstream integration below): the #3007 wall-clock signed sample contract was a separate integration prerequisite. Planner message `940f0704639644f6afa2f4b63deb645b` reports local semantic acceptance at `30aa469c`; message `0bb390a9b1364231a3a5d9766b98b92f` reports draft PR [#3014](https://github.com/keanji-x/neige-calm/pull/3014), with independent reviews/CI and integration still pending. That historical checkout had not fetched or consumed it. Those checks used rate 1; paused/rate/reverse wall-clock velocity correctness is not certified here, and there is no local multiplication by playback rate.

## Cap-outside regrab correction (after review B522401)

The ordinary finger cap remains -48px. A spring can legally paint beyond it.
For acquisition at `q0`, the lease temporarily uses `min(-48, q0)` as its lower
paint boundary, with the existing inverse raw anchor (`q0 / .35` when negative).
The first downward move follows at .35; outward movement retains the acquired
extreme without deepening it. Re-entering the ordinary domain (`raw >= -48/.35`)
permanently retires the exception for that lease. Subsequent reversal obeys -48.
Paint and its directional derivative use the same boundary; raw velocity remains
intent only. Same-coordinate input preserves the acquired sample velocity.
Capture identity, cancellation, flick/distance thresholds and spring ownership
are unchanged. No additional inertia, solver, curve or shared-motion patch exists.

Two full Sheet baseline runs each reported 29 pass / 6 behavioral red:
`/tmp/sheet-cap-baseline{,-repeat}.{json,log}`. Real native capture plus the shared
spring's nonterminal WAAPI phase reproduced 4.833374px and 60.849213px first-move
jumps. Eight added owner cases use independent unanimated frame/actual rect,
cover both input depths/directions, return/reversal before and across zero,
frame layout, release/cancel/stale up/regrab, outward v=0 and no-displacement v.
The 1ms second grab retains the existing <1px acquisition budget for analytic
sample vs native interpolation (observed .082px); first-move delta is independently
checked within .05px of .35. No original assertion or retry changed.

Commands actually run from the checkout root (Vitest commands use `cwd=fe`):

- `npx vitest run --project browser --project browser-coarse web/src/ui/sheet --passWithNoTests=false --reporter=json --outputFile=/tmp/sheet-cap-baseline.json` (repeat has its own output path).
- `python3 /tmp/sheet-focused.py sheet-cap-pregreen`: all 16 whole observable targets / 215 pass. Fresh import sweep still finds 90 reachable tests; `/tmp/sheet-cap-reachability.txt` and `/tmp/sheet-cap-observers.txt` retain evidence. The inherited conservative 16-target scope also covers production caller/footer/focus/viewport regressions; no permanent test-name registry was added.
- `cd fe && node /tmp/sheet-review-b-component.mjs`: original probe unchanged, now first move .349976px; `/tmp/sheet-cap-component-green.log`.
- `cd fe && npx eslint web/src/ui/sheet/gesture.ts web/src/ui/sheet/public.browser.test.tsx`: pass. An earlier invocation from the root failed to find the frontend ESLint config; it is not counted as validation.
- `python3 /tmp/sheet-cap-mutate.py <factor>`: exclusive serial production-only single-factor mutation, predeclared complete red names, all 16 whole targets, finally byte/hash restoration, then same full green replay. Plan and exact commands: `/tmp/sheet-cap-mutation-plan.json`, `/tmp/sheet-cap-mutate.py`, `/tmp/sheet-focused.py`.

| Factor | Complete red set | Restore | Evidence prefix in `/tmp/` |
| --- | --- | --- | --- |
| acquisition-cap (only remove acquisition lower-bound protection) | 7, exact | 215/215 | `sheet-cap-acquisition-cap-` |
| grab-target (old factor, expanded observations) | 11, exact | 215/215 | `sheet-cap-grab-target-` |
| zero-velocity (old factor, expanded observations) | 12, exact | 215/215 | `sheet-cap-zero-velocity-` |

Each prefix has `declaration.json` (all predicted names before mutation),
`result.json` (complete actual set and hashes), `red`/`restored` JSON and logs.
The prior cancel/stale-exit/footer factors retain their evidence below: their
mutated branches and original assertions are untouched; new cases do not cancel
qualified downward travel, close the host or consume retained closed footer
interaction. Grab and zero-velocity were rerun because their expected sets grow.
Text/lint/build and Sheet+hover+production-session replay belong to this attempt's
kernel gate, not these local mutation runs; broad suites remain CI-owned.

Historical cap-fix evidence only: this was a rate-1 slice, not final sample integration. #3014 signed
wall-clock sample semantics remain unconsumed; pausing above freezes positions,
not proof of correct paused/rate/reverse velocity. Unknown vendor-history missed
input and the earlier Calendar strict-locator red remain unexplained, not fixed
or attributed to the environment. A new source head invalidates old-page preview
as evidence for the new head; fresh L2 reviews, exact-head CI and original-page /
Tier 1 checks remain Planner work. No app, Shell, vendor or shared code is changed.

## Preserved recovery evidence: old-entry red at d9406f52

Head before changes: `87c6e2cd1fd39a9fda3a8210faec35052d612ead`, initially clean. Only this document and [the new production-entry motion target](../fe/web/src/app/router/mobile-conversation-motion.browser.test.tsx) change. No production implementation, inventory, dependencies, shared motion or hover owner edits. The two entry phases pause/await/seek the real CSS transition, then acquire with native browser mouse input and compare actual panel rect. Cancellation uses native acquisition and explicitly synthetic PointerEvents; it observes actual controlled native close, not an absent spy. No model/solver is mocked or copied.

Commands actually run (test commands from `fe/`, text gates from repository root):

- `npx vitest run --project browser web/src/app/router/mobile-conversation-motion.browser.test.tsx --passWithNoTests=false`: initial two-case target, 2 behavioral failures, 15.69s (`/tmp/sheet-owner-baseline-red1.log`). Grab jumps 347.652px; cancel closes native dialog. Then parameterized entry phases independently, same whole-target command: 3/3 behavioral failures, 4.51s (`/tmp/sheet-owner-baseline-red2.log`): entry phase .15 jumps 347.652px, phase .3 jumps 139.811px, cancellation leaves `dialog.open === false`.
- `npx vitest run --project browser --project browser-coarse web/src/app/router/mobile-conversation-motion.browser.test.tsx web/src/app/router/mobile-conversation.browser.test.tsx web/src/app/router/mobile-conversation.coarse.browser.test.tsx web/src/app/router/mobile-chat.browser.test.tsx --passWithNoTests=false`: 39 pass / the same 3 fail, four whole targets, 51.52s (`/tmp/sheet-owner-baseline-c3.log`). This repeats the stable old-entry red alongside accepted footer coverage.
- `npx eslint web/src/app/router/mobile-conversation-motion.browser.test.tsx --max-warnings=0`: final exit 0 (`/tmp/sheet-owner-eslint.log`). First run found direct React state import/dynamic DOM selectors; those were corrected using guarded UI state and literal selectors.
- `scripts/local-ratchet-gates.sh`: exit 0 (`/tmp/sheet-owner-ratchet.log`); `git diff --check`: exit 0. No package install or upgrade was run.

At the end of the preceding failed attempt, replacement had not begun. The hover request has since been approved and implemented as described above. New release/reopen motion evidence, new UI owner tests, mutations, complete post-migration observable classification and kernel gate remain unrun. Existing footer reopen evidence below does not prove motion reversal. No preview/device/Tier1/review/CI completion is claimed. The three intentionally red regression cases are a recovery handoff, not a mergeable implementation. #3007 formal sample integration remains a separate final prerequisite.

## Historical implementation and local evidence (before sample integration)

`ui/sheet/public.tsx` owns one nonmodal native host, retained content authority, footer relocation, focus and observers. `motion.ts` owns paint and `createPlaybackTarget`; `gesture.ts` owns the handle lease and the frozen policy. All are below 800 lines. App now only assembles intent, refs, viewport-derived frame and conversation content; desktop Drawer remains the presentation owner. The old application panel marker is applied by its observing app ref without touching paint. The UI's stable body carries the existing drawer surface marker only while present; children unmount after final exit while the body preserves the native ancestry of previously observed refs. Vendor private child CSS selectors were deleted. Sheet uses existing semantic layer and hairline tokens.

Initial native geometry is read after `show()` makes it available. Acquisition writes sampled paint before cancelling fill. A no-travel tap cannot turn inherited entry displacement into a dismissal. Down/cancel/release clear capture authority before native capture release; raw classification velocity and painted spring velocity remain distinct. Frame rebases preserve motion q/v while ordinary layout moves. A presentation switch consumes the original pre-render selection snapshot rather than replacing it after a child layout mutation. Closing immediately inerts both panel and stationary footer. Only a current, still-closed host completion restores final focus, once; completed closed-frame updates do not restore again.

Focused owner checks include native intermediate grab, independent rect/time paint derivatives (including cap), exact gesture boundaries, five cancellation sources and stale up, secondary/right-button rejection, unanimated frame rebase, actual reversal direction, runtime reduced motion, StrictMode/unmount, delayed focus cancellation, background interaction, native preview pause/seek/resume and idle RAF cleanup. Browser ESM `{ spy: true }` wraps the real spring/motion factories without replacing returned playbacks, equations or DOM. The stale-exit check also captures the real motion completion argument via a call-through method spy and invokes that old owner callback after reopen; this explicitly tests host authority separately from the shared target's own identity fence. The actual superseded native finished promise is also awaited. This injected stale callback is not claimed as a browser delivering an uncancelled old effect.

Commands and evidence:

- `fe/`: new hover notification whole target: 2/2 pass, `/tmp/sheet-hover-notice.log`; its first test proves ResizeObserver stays silent after an actual 60px transform.
- `fe/`: original four complete production targets after migration: 42/42 pass, `/tmp/sheet-third.log` (43.55s). The old red log remains `/tmp/sheet-owner-baseline-c3.log`, unchanged.
- `fe/`: new Sheet browser/coarse whole targets: 21/21 pass, `/tmp/sheet-ui5.log` (24.63s), before adding the final five focus/cleanup/admission cases.
- Expanded 16 whole browser/coarse targets: 205/205 pass, `/tmp/sheet-pregreen-final.log` and JSON (exact duration in log). `python3 /tmp/sheet-focused.py sheet-pregreen-final` records the full command in `/tmp/sheet-pregreen-final-summary.log`. This includes all 12 inherited native/footer integration targets, both Sheet targets, new production motion and the desktop conversation-motion control. The earlier expanded runs exposed missing app markers/layering and a removed observed body's ancestry; those were fixed without changing Shell or its assertions. A disconnected navigation observer produced NaN during a host move; it now refuses an unavailable navigation frame, and the final expanded run has no NaN warning.
- Targeted ESLint and Stylelint run locally; the kernel owns the authoritative text/lint/build/focused replay. Current gate and mutation evidence is recorded below once run. Remote CI, original-page preview, stack Tier 1, device/keyboard matrices and formal L2 reviews remain Planner-owned and are not claimed here.

### Mutation observable scope

A fresh relative-import sweep found 90 reachable test files (`/tmp/sheet-reachable-tests.txt`), with input/native-time observers separately enumerated in `/tmp/sheet-input-observers.txt`. New gesture factors require handle acquisition; only the new Sheet targets, new production motion and existing mobile-conversation/coarse targets exercise it. Other mobile consumers can observe host exit/reopen, inherited velocity or retained footer, so the complete inherited C12 is retained conservatively for every factor, plus desktop conversation-motion. The 16-target list is `/tmp/sheet-mutation-targets.json`; this is dated evidence, not a permanent registration or CI list. No shared focus/motion implementation changes, so tests of their other independent callers cannot observe these Sheet-only factors.

The other reachable browser files either remain desktop, never acquire a handle/native sample, or exercise initial open/content and child overlays without native conversation exit/reentry. Their initial velocity is zero and the mutated branches are not taken. Node/contract files have no native paint clock. Vendor-only hover/Switcher files do not import Sheet and remain isolation controls, run separately as the full hover owner suite. Ordinary new tests/variants need only their owner/test files; the only permanent inventory change is the new Sheet directory, with the canonical trailer above. Focused replay intentionally runs once again in the kernel; broad suites remain CI-owned.

### Additional frame-notification regression

After the first mutation round, inspection found bottom-inset-only layout movement during a lease had no ResizeObserver signal and returned before a paint notice. A real full owner-target regression was red twice: anchor rects moved -100px while the panel preview moved 0px (`/tmp/sheet-frame-notice-red.log`, `/tmp/sheet-frame-notice-red2.log`, 26 pass / 1 fail). The fix keeps q/v and pointer anchor, publishes panel geometry after removing any old native fill, and publishes the moved footer host through the same ancestor notification. Complete Sheet browser/coarse replay: 28/28 pass, `/tmp/sheet-frame-notice-green.log`. This adds no observer, clock or global invalidation. Critical mutation evidence is rerun on this final behavior.

The frame is a required numeric CSS-pixel declaration: viewport resize/scroll comes from the sole `useVisibleViewport`, and navigation ResizeObserver observes the independent navigation layout source. Sheet does not infer frame from animated paint. Actual panel replacement disposes the old controller/lease; latest required declarations drive new lifetime setup. Changing a forwarded observer ref does not replace the painted node or let an old completion target a new native host. This is the specific check prompted by Planner message `d92ba7b72d774b5b80de72de92a13042`; no #3007 edge code is consumed or modified.

### Final production mutations and restoration

The final source baseline ran 16 whole targets / 207 tests green in `/tmp/sheet-frame-mutation-base.log`. For each factor the declaration (including every expected red name) was serialized before writing production code to `/tmp/sheet-<factor>-declaration.json`; the single replacement was asserted present, with original bytes saved separately. `/tmp/sheet-mutate.py <factor>` ran `python3 /tmp/sheet-focused.py sheet-<factor>-red`, restored original bytes in `finally`, asserted byte equality, then ran the same 16 complete targets as `sheet-<factor>-restored`. JSON results reject an incomplete test count or target count, not merely a process exit code. No tests, solver, dependency or fixture logic were mutated.

#### `grab-target`

Production factor: `const sample = motion.hold();` → `const sample = { ...motion.hold(), value: 0 }; motion.direct(sample);` in `fe/web/src/ui/sheet/gesture.ts`. Complete actual red set equals the predeclared set (3 failed / 204 passed):

- `grabs native intermediate paint against the unanimated frame`
- `grabs the painted conversation sheet during entry without a position jump at phase 0.15`
- `grabs the painted conversation sheet during entry without a position jump at phase 0.3`

Restored replay: 207/207, exit 0. SHA-256 before/restored: `59768f412be45e2146800d6106f44d76dfcc98a7885ffc2e10f4c6d9821adf10`. Full results `/tmp/sheet-grab-target-result.json`, red/restore logs `/tmp/sheet-grab-target-red.log` and `/tmp/sheet-grab-target-restored.log`.

#### `zero-velocity`

Production factor: `from.value, to, from.velocity, paint` → `from.value, to, 0, paint` in `fe/web/src/ui/sheet/motion.ts`. Complete actual red set equals the predeclared set (4 failed / 203 passed):

- `hands actual paint velocity to the spring for travel -60`
- `hands actual paint velocity to the spring for travel 60`
- `preserves native q/v on a running frame rebase and restores focus only once`
- `reverses from native paint and velocity and fences the superseded exit`

Restored replay: 207/207, exit 0. SHA-256 before/restored: `3e55b55e4d4e8e3ae8fd923afb03155a8d69e8d4dd5b9cfca4a5f611b3e454e9`. Full results `/tmp/sheet-zero-velocity-result.json`, red/restore logs `/tmp/sheet-zero-velocity-red.log` and `/tmp/sheet-zero-velocity-restored.log`.

#### `cancel-release`

Production factor: `if (!cancelled && old.travel !== 0` → `if (true && old.travel !== 0` in `fe/web/src/ui/sheet/gesture.ts`. Complete actual red set equals the predeclared set (7 failed / 200 passed):

- `cancels a dismiss-qualified handle drag without closing the real conversation`
- `cancels a real captured touch without dismissing and ignores its stale release`
- `cancels qualified travel on blur before releasing capture and ignores stale up`
- `cancels qualified travel on contextmenu before releasing capture and ignores stale up`
- `cancels qualified travel on hidden before releasing capture and ignores stale up`
- `cancels qualified travel on lostpointercapture before releasing capture and ignores stale up`
- `cancels qualified travel on pointercancel before releasing capture and ignores stale up`

Restored replay: 207/207, exit 0. SHA-256 before/restored: `59768f412be45e2146800d6106f44d76dfcc98a7885ffc2e10f4c6d9821adf10`. Full results `/tmp/sheet-cancel-release-result.json`, red/restore logs `/tmp/sheet-cancel-release-red.log` and `/tmp/sheet-cancel-release-restored.log`.

#### `stale-exit`

Production factor: `if (intent.current !== token || latest.current.open || !latest.current.enabled || dialog !== node || !node.isConnected) return;` → `// MUTATION: remove the host completion authority guard.` in `fe/web/src/ui/sheet/public.tsx`. Complete actual red set equals the predeclared set (1 failed / 206 passed):

- `reverses from native paint and velocity and fences the superseded exit`

Restored replay: 207/207, exit 0. SHA-256 before/restored: `51526b2ae32c7fae17ea466af9575179c142f39fb2a57cc9da508bad6efe5c8a`. Full results `/tmp/sheet-stale-exit-result.json`, red/restore logs `/tmp/sheet-stale-exit-red.log` and `/tmp/sheet-stale-exit-restored.log`.

#### `footer-inert`

Production factor: `inert={enabled && !open}` → `inert={false}` in `fe/web/src/ui/sheet/public.tsx`. Complete actual red set equals the predeclared set (2 failed / 205 passed):

- `revokes retained native footer interaction at close commit: focus and keyboard`
- `revokes retained native footer interaction at close commit: trusted pointer`

Restored replay: 207/207, exit 0. SHA-256 before/restored: `51526b2ae32c7fae17ea466af9575179c142f39fb2a57cc9da508bad6efe5c8a`. Full results `/tmp/sheet-footer-inert-result.json`, red/restore logs `/tmp/sheet-footer-inert-red.log` and `/tmp/sheet-footer-inert-restored.log`.

#### Invalid rounds and limitations retained

The first pre-frame-fix mutation round is preserved as `/tmp/sheet-round1-*`; it is superseded by final-tree evidence. One final stale-exit red round had an additional `keeps history failures distinct from an empty workspace and can retry` failure: the global alert locator saw both the intended history error and a Calendar tasks alert. That case never opens/exits Sheet and cannot execute the changed completion guard. The round is invalidated, not counted; `/tmp/sheet-final-stale-unmatched-*` preserves its evidence. Its restore encountered module fetch failures and ran only 45 tests, also not a pass. A previous restore had a browser session connection timeout and ran zero tests. The final repeat, unchanged production factor/tests/expectations, matched exactly one red and restored all 207 green.

An earlier high-load expanded replay intermittently missed an edge click on the independent vendor MobileHistory Settings button. Diagnostic instrumentation saw no pointer/click events delivered to the button, dialog still open and Sheet never opened. A temporary 100-iteration probe passed both old entry (original HEAD surface/CSS, then exact bytes/hash restored) and new entry; independent complete Shell target passed 33/33. The diagnostic source was removed from the workspace and preserved only at `/tmp/sheet-history-probe-source.tsx`; logs are `/tmp/sheet-history-old-entry.log`, `/tmp/sheet-history-probe3.log`, `/tmp/sheet-shell-focus.log` and baseline restoration summaries. No Shell/MobileHistory code or test changed, no timeout/assertion was weakened. Root attribution is still unknown; mutation runs use four workers. This is a stated environment/integration limitation, not proof that every previously observed failure was fixed.

Full hover owner replay before the frame-only notification fix: `fe/ npx vitest run --project browser --project web-dom web/src/ui/hover-preview --passWithNoTests=false` → 9 targets / 156 tests passed, `/tmp/sheet-hover-full.log` (366.35s). The final gate includes that owner again. Targeted ESLint and Stylelint exited 0; no dependency install/upgrade occurred. `git diff --check` is clean. No permanent mutation manifest/test-name pin, broad suite, independent review, preview stack, Tier 1 or CI was run by this worker.

## Historical owner and API survey evidence

- `fe/module-file-inventory.yaml` registers `fe/web/src/app/router` as writable owner `app/router`. The only production caller of `ConversationSurface` is `app/router/public.tsx:747`; its `compactComposer` gate at line 410 requires report composer, compact viewport, enabled and non-inline presentation. The side conversation sets `inline: true` and removes `reportComposer` at lines 349–351.
- Astryx Core 0.6.3 public `dist/BottomSheet/BottomSheet.d.ts` declares panel ref, label, height, snap points, purpose, and standalone `isOpen`/`onOpenChange`/`finalFocusRef`; switcher children use `sheetId`. `BottomSheetSwitcher.d.ts` declares native dialog ref, `activeSheet`, `onActiveSheetChange`, `hasScrim`, and describes no-scrim `show()` semantics. It does not declare a painted-position/velocity or drag-handoff port. These are installed public types, not an inferred private naming convention.
- Existing app composition uses one `footerHost` and one React portal. The stationary footer is appended directly to the native dialog, outside the BottomSheet panel. Vendor panel inertness therefore cannot revoke the footer. App logical `open` supplies the missing footer authority binding; desktop behavior retains its existing owner.
- `ui/drawer/restore-focus.ts` owns `usePortalFocusRetention` and `useNativeDialogFinalFocus`. The former validates connected selection boundaries and consumes a snapshot before focusing; the latter restores only after native exit while logically closed. Neither helper changes in this slice.

## Footer outcome and preserved boundaries

At native logical close commit, the retained footer wrapper becomes `inert` and `aria-hidden`. Reopen clears both immediately on the same wrapper/portal/editor, retaining draft and contenteditable range. The real editor reopen regression also verifies that an old exit cannot close the reopened dialog or take final focus; after the final exit the opener receives focus.

The production change is one binding line in `mobile-conversation-surface.tsx`. The regression uses real `ChatComposer` public textbox `Message` (no trigger menu in this fixture) and button `Send`. Focus/keyboard and trusted browser pointer delivery are separate cases; forced driver action bypasses actionability but still records `PointerEvent.isTrusted === true`. No `HTMLElement.click` substitutes for it.

No gesture, height, snap, Switcher, vendor, shared motion/focus, Shell, dependencies, rules, retries or test tolerances change. The original dock geometry assertion is preserved byte-for-byte. Its earlier failure has not reproduced in accepted C3/C8 evidence; attribution remains unknown.

## Verification ownership and prior evidence

Original behavioral red is preserved rather than repeated: `/tmp/footer-red2.log`, r1 failure event 518489, reports 37 pass / 2 fail (44.73s). Actual failures: programmatic refocus succeeds after close, and a trusted pointer invokes real send with `Retained draft`. `/tmp/footer-green1.log` reports C3 39/39 (43.66s). The earlier incorrect locator run is not behavioral-red evidence.

Planner terminal `eb3176fca8804349a423d498cb1d0eb0`, card `dcd5d92622b946cba9285491dd042452`, ran original C8 at head `35914ee3`: 101/101, exit 0, 69.31s (task-provided evidence). This worker added the other four relevant targets as a pregreen: 69/69, exit 0, 79.57s, `/tmp/footer-extra-green.log`. Source/test bytes were unchanged, so together these establish C12 pregreen. C12 has 170 tests.

The worker owns focused mutation and restoration runs. The declared kernel gate owns text ratchets, frontend lint/build, and one complete C3 replay with empty selection rejected. CI owns broad suites; this worker does not launch them or formal reviews.

## Observable classification at the inherited footer snapshot

Relative static imports/re-exports and literal dynamic imports from current `fe/web/src` tests were recursively followed to the real production surface (`/tmp/footer-reachable-tests.txt`). The current sweep yields 87 reachable files: 48 browser and 39 DOM/contract files. Production callers, branch predicates, viewports, close/reopen paths and assertions were then inspected. This is a dated evidence inventory, not a permanent test registration or CI constraint; later test additions need no edits to this document.

C12 below includes direct tests and conservative integration consumers of native close/host transition. All 12 whole targets run in both configured projects, whose suffix partition selects each target once. It does not filter for predicted failing test names.

| Included target (relative to `fe/web/src/app/`) | Reason |
| --- | --- |
| [router/assistant-provider.browser.test.tsx](../fe/web/src/app/router/assistant-provider.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |
| [router/chat-link-hover.browser.test.tsx](../fe/web/src/app/router/chat-link-hover.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |
| [router/conversation-switch.browser.test.tsx](../fe/web/src/app/router/conversation-switch.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |
| [router/mobile-chat.browser.test.tsx](../fe/web/src/app/router/mobile-chat.browser.test.tsx) | Direct surface or production mobile conversation/real editor authority coverage |
| [router/mobile-conversation.browser.test.tsx](../fe/web/src/app/router/mobile-conversation.browser.test.tsx) | Direct surface or production mobile conversation/real editor authority coverage |
| [router/mobile-conversation.coarse.browser.test.tsx](../fe/web/src/app/router/mobile-conversation.coarse.browser.test.tsx) | Direct surface or production mobile conversation/real editor authority coverage |
| [router/mobile-history.browser.test.tsx](../fe/web/src/app/router/mobile-history.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |
| [router/report-readme.browser.test.tsx](../fe/web/src/app/router/report-readme.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |
| [router/side-conversation.browser.test.tsx](../fe/web/src/app/router/side-conversation.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |
| [router/terminal-workbench.coarse.browser.test.tsx](../fe/web/src/app/router/terminal-workbench.coarse.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |
| [shell/mobile.browser.test.tsx](../fe/web/src/app/shell/mobile.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |
| [shell/unified-mobile-header.browser.test.tsx](../fe/web/src/app/shell/unified-mobile-header.browser.test.tsx) | Native conversation close, nested layer/focus, draft or host transition consumer |

Excluded browser targets below still import the router; exclusion is specific to the two footer mutations, not an assertion that these are fake callers or unimportant tests. For open-only consumers the mutated expression matches production until native close; they never consume retained closed footer focus/send or rapid close→reopen. Cleanup/unmount does not exercise that authority. For desktop consumers `nativeSheet` is false. Escape in a child menu/confirmation/source/history is not a native Conversation close.

| Excluded target (relative to `fe/web/src/app/`) | Branch/observation reason |
| --- | --- |
| [auth/auth-intent-supersession.browser.test.tsx](../fe/web/src/app/auth/auth-intent-supersession.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [auth/logout-recovery.browser.test.tsx](../fe/web/src/app/auth/logout-recovery.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [auth/recovery.browser.test.tsx](../fe/web/src/app/auth/recovery.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [auth/scan-enrollment.browser.test.tsx](../fe/web/src/app/auth/scan-enrollment.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [providers/read-state-refresh.browser.test.tsx](../fe/web/src/app/providers/read-state-refresh.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |
| [providers/recovery.browser.test.tsx](../fe/web/src/app/providers/recovery.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/chat-performance.browser.test.tsx](../fe/web/src/app/router/chat-performance.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |
| [router/chat-send-visibility.browser.test.tsx](../fe/web/src/app/router/chat-send-visibility.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |
| [router/conversation-canonical.browser.test.tsx](../fe/web/src/app/router/conversation-canonical.browser.test.tsx) | Mobile native conversation stays open; Escape/cancel belongs to Retry confirmation |
| [router/conversation-compact.browser.test.tsx](../fe/web/src/app/router/conversation-compact.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |
| [router/conversation-motion.browser.test.tsx](../fe/web/src/app/router/conversation-motion.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |
| [router/conversation-read.browser.test.tsx](../fe/web/src/app/router/conversation-read.browser.test.tsx) | Native conversation stays open (or no conversation opened); no closed footer interaction or native rapid reopen |
| [router/conversation-recovery.browser.test.tsx](../fe/web/src/app/router/conversation-recovery.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |
| [router/conversation-restart.browser.test.tsx](../fe/web/src/app/router/conversation-restart.browser.test.tsx) | Mobile cases keep native conversation open; the explicit close is only the desktop `/` case |
| [router/conversation-send-feedback.browser.test.tsx](../fe/web/src/app/router/conversation-send-feedback.browser.test.tsx) | Native conversation stays open (or no conversation opened); no closed footer interaction or native rapid reopen |
| [router/conversation-steer.browser.test.tsx](../fe/web/src/app/router/conversation-steer.browser.test.tsx) | Native conversation stays open (or no conversation opened); no closed footer interaction or native rapid reopen |
| [router/conversation-title.browser.test.tsx](../fe/web/src/app/router/conversation-title.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |
| [router/daily-planner.browser.test.tsx](../fe/web/src/app/router/daily-planner.browser.test.tsx) | Mobile cases do not consume closed conversation footer; explicit close cases use desktop geometry |
| [router/empty-states.browser.test.tsx](../fe/web/src/app/router/empty-states.browser.test.tsx) | Native conversation stays open (or no conversation opened); no closed footer interaction or native rapid reopen |
| [router/first-message-continuity.browser.test.tsx](../fe/web/src/app/router/first-message-continuity.browser.test.tsx) | Native conversation stays open (or no conversation opened); no closed footer interaction or native rapid reopen |
| [router/first-message-geometry.browser.test.tsx](../fe/web/src/app/router/first-message-geometry.browser.test.tsx) | Native conversation stays open (or no conversation opened); no closed footer interaction or native rapid reopen |
| [router/mentions.browser.test.tsx](../fe/web/src/app/router/mentions.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |
| [router/new-track-admin.browser.test.tsx](../fe/web/src/app/router/new-track-admin.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/new-track-isolation.browser.test.tsx](../fe/web/src/app/router/new-track-isolation.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/new-track-provider.browser.test.tsx](../fe/web/src/app/router/new-track-provider.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/normal-empty-surfaces.browser.test.tsx](../fe/web/src/app/router/normal-empty-surfaces.browser.test.tsx) | Native conversation stays open (or no conversation opened); no closed footer interaction or native rapid reopen |
| [router/planner-approvals.browser.test.tsx](../fe/web/src/app/router/planner-approvals.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/planner-asks.browser.test.tsx](../fe/web/src/app/router/planner-asks.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/recipe-recovery.browser.test.tsx](../fe/web/src/app/router/recipe-recovery.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/report-source.browser.test.tsx](../fe/web/src/app/router/report-source.browser.test.tsx) | Mobile flow opens source preview/card, not closed native Conversation footer; conversation Escape case remains desktop |
| [router/report-source.hover.browser.test.tsx](../fe/web/src/app/router/report-source.hover.browser.test.tsx) | Mobile flow opens source preview/card, not closed native Conversation footer; conversation Escape case remains desktop |
| [router/template-plugin-guides.browser.test.tsx](../fe/web/src/app/router/template-plugin-guides.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/template-readiness.browser.test.tsx](../fe/web/src/app/router/template-readiness.browser.test.tsx) | Auth/history/new-track/settings/other overlay flow; no native conversation close with retained footer consumption |
| [router/terminal-workbench.browser.test.tsx](../fe/web/src/app/router/terminal-workbench.browser.test.tsx) | Desktop conversation closes; later compact resize inspects terminal host, no retained native footer interaction |
| [router/track-admin.browser.test.tsx](../fe/web/src/app/router/track-admin.browser.test.tsx) | Native conversation stays open (or no conversation opened); no closed footer interaction or native rapid reopen |
| [router/track-view-state.browser.test.tsx](../fe/web/src/app/router/track-view-state.browser.test.tsx) | Conversation cases stay desktop; no native retained footer authority |

The following 39 reachable DOM/contract targets are excluded: their Vitest `web-dom` project uses jsdom, not a native inert/focus/pointer host, and they contain no assertion of this footer binding. They remain genuine import/shape/route consumers; they cannot supply native interaction-red evidence for these mutations.

- [auth/production-app.contract.test.tsx](../fe/web/src/app/auth/production-app.contract.test.tsx)
- [auth/session-gate.contract.test.tsx](../fe/web/src/app/auth/session-gate.contract.test.tsx)
- [router/conversation-compact.test.tsx](../fe/web/src/app/router/conversation-compact.test.tsx)
- [router/conversation-input-response.test.tsx](../fe/web/src/app/router/conversation-input-response.test.tsx)
- [router/conversation-presentation.test.tsx](../fe/web/src/app/router/conversation-presentation.test.tsx)
- [router/conversation-read.test.tsx](../fe/web/src/app/router/conversation-read.test.tsx)
- [router/conversation-steer.test.tsx](../fe/web/src/app/router/conversation-steer.test.tsx)
- [router/daily-planner.test.tsx](../fe/web/src/app/router/daily-planner.test.tsx)
- [router/delete-confirm.test.tsx](../fe/web/src/app/router/delete-confirm.test.tsx)
- [router/first-message-continuity.test.tsx](../fe/web/src/app/router/first-message-continuity.test.tsx)
- [router/first-message-queue-writes.test.tsx](../fe/web/src/app/router/first-message-queue-writes.test.tsx)
- [router/live-replies.test.tsx](../fe/web/src/app/router/live-replies.test.tsx)
- [router/mobile-report-navigation.test.tsx](../fe/web/src/app/router/mobile-report-navigation.test.tsx)
- [router/new-card.test.tsx](../fe/web/src/app/router/new-card.test.tsx)
- [router/new-conversation-composer.test.tsx](../fe/web/src/app/router/new-conversation-composer.test.tsx)
- [router/new-track-admin.test.tsx](../fe/web/src/app/router/new-track-admin.test.tsx)
- [router/new-track-route.test.tsx](../fe/web/src/app/router/new-track-route.test.tsx)
- [router/planner-asks.test.tsx](../fe/web/src/app/router/planner-asks.test.tsx)
- [router/planner-conversation.test.tsx](../fe/web/src/app/router/planner-conversation.test.tsx)
- [router/planner-role-admin.test.tsx](../fe/web/src/app/router/planner-role-admin.test.tsx)
- [router/public.contract.test.tsx](../fe/web/src/app/router/public.contract.test.tsx)
- [router/read-fallbacks.contract.test.tsx](../fe/web/src/app/router/read-fallbacks.contract.test.tsx)
- [router/recipes-route.test.tsx](../fe/web/src/app/router/recipes-route.test.tsx)
- [router/report-readme.test.tsx](../fe/web/src/app/router/report-readme.test.tsx)
- [router/task-recovery.test.tsx](../fe/web/src/app/router/task-recovery.test.tsx)
- [router/terminal-workbench.test.tsx](../fe/web/src/app/router/terminal-workbench.test.tsx)
- [router/track-cards-panel.test.tsx](../fe/web/src/app/router/track-cards-panel.test.tsx)
- [router/track-conversation.test.tsx](../fe/web/src/app/router/track-conversation.test.tsx)
- [router/track-presentation.test.tsx](../fe/web/src/app/router/track-presentation.test.tsx)
- [router/track-reopen.test.tsx](../fe/web/src/app/router/track-reopen.test.tsx)
- [router/track-report-preparation.test.tsx](../fe/web/src/app/router/track-report-preparation.test.tsx)
- [router/track-untitled.test.tsx](../fe/web/src/app/router/track-untitled.test.tsx)
- [router/write-failures.contract.test.tsx](../fe/web/src/app/router/write-failures.contract.test.tsx)
- [shell/settings-overlay.test.tsx](../fe/web/src/app/shell/settings-overlay.test.tsx)
- [shell/settings-planners.test.tsx](../fe/web/src/app/shell/settings-planners.test.tsx)
- [shell/settings-plugins-config.test.tsx](../fe/web/src/app/shell/settings-plugins-config.test.tsx)
- [shell/settings-plugins-install.test.tsx](../fe/web/src/app/shell/settings-plugins-install.test.tsx)
- [shell/settings-write-failures.contract.test.tsx](../fe/web/src/app/shell/settings-write-failures.contract.test.tsx)
- [shell/sidebar-membership.test.tsx](../fe/web/src/app/shell/sidebar-membership.test.tsx)

Other non-reachable UI/vendor tests do not render this app-owned footer binding. Shared focus/motion and vendor implementations are unchanged; their isolated tests cannot observe a mutation of this specific JSX wrapper. Frontend e2e/original-page checks and stack Tier 1 remain separate integration work, not replaced by C12.

## Critical single-factor mutation protocol

Exclusive worker window only; the preceding design task had finished, no review/mutation readers were scheduled. Production backup: `/tmp/footer-original.tsx`. Original SHA-256: `c9719da6760c8dcd75ec927799a2623280c6b32a33fd4b318d8cc8dfeb82bacf`; regression test SHA-256: `27d359f0d465da3960055afea502d5895d1c8382aad20eb3e98d57949d5948e1`.

The command, from `fe/`, for every mutant and restored green is:

```sh
npx vitest run --project browser --project browser-coarse \
  web/src/app/router/mobile-conversation.browser.test.tsx \
  web/src/app/router/mobile-conversation.coarse.browser.test.tsx \
  web/src/app/router/mobile-chat.browser.test.tsx \
  web/src/app/router/mobile-history.browser.test.tsx \
  web/src/app/router/chat-link-hover.browser.test.tsx \
  web/src/app/router/side-conversation.browser.test.tsx \
  web/src/app/router/conversation-switch.browser.test.tsx \
  web/src/app/router/terminal-workbench.coarse.browser.test.tsx \
  web/src/app/router/report-readme.browser.test.tsx \
  web/src/app/router/assistant-provider.browser.test.tsx \
  web/src/app/shell/unified-mobile-header.browser.test.tsx \
  web/src/app/shell/mobile.browser.test.tsx \
  --passWithNoTests=false --reporter=default --reporter=json \
  --outputFile.json=/tmp/footer-<run>-actual.json
```

For the restoration replay only the JSON output filename changes to `footer-<run>-restored.json`. `/tmp/footer-mutation-run.py` runs the full command and writes back original bytes in `finally`, verifies the original hash, then runs full C12 green and compares every failed assertion in the JSON report against the predeclared list. No test is edited during mutation.

M1 predeclared complete expected red (before writing the mutant):

1. `mobile-conversation.browser.test.tsx` — `revokes retained native footer interaction at close commit: focus and keyboard`
2. `mobile-conversation.browser.test.tsx` — `revokes retained native footer interaction at close commit: trusted pointer`

Only `inert={nativeSheet && !props.open}` changed to `inert={false}`; `aria-hidden` and all tests stayed unchanged. Applied SHA-256: `96c1b6a307d4e2311b240c9b49834c91068d0a90dfd316ec12f8149790c88576`. Full C12: 168 pass / 2 fail, exit 1, 72.22s. Complete actual red equals the two names above: real editor refocus succeeds and trusted pointer sends `Retained draft`. No missing/additional failure. Restored original bytes/hash; full C12 170/170, exit 0, 70.25s. Logs: `/tmp/footer-m1.log`, `/tmp/footer-m1-actual.json`, `/tmp/footer-m1-restored.log`, `/tmp/footer-m1-restored.json`, `/tmp/footer-m1-evidence.log`, predeclaration `/tmp/footer-m1-expected.json`.

M2 predeclared complete expected red (before writing the mutant):

1. `mobile-conversation.browser.test.tsx` — `restores the same real footer editor immediately on native reopen and ignores the old exit`
2. `mobile-chat.browser.test.tsx` — `keeps a rapid native reopen focused inside and restores the opener only after the final exit`

Only the inert predicate changed to `nativeSheet && (!props.open || (nativePresented && footerHost.firstElementChild?.hasAttribute('inert') === true))`. This deliberately preserves an already-applied inert state across an in-flight native reopen, while leaving initial open and reopen after native exit alone; aria visibility is unchanged. It is a production-binding mutant, not a parallel test implementation. Applied SHA-256: `f2e391800234cd7d1ca2a689939eb4a2bc62fa951ce2141fae7eda942c7b97c4`. Full C12: 168 pass / 2 fail, exit 1, 69.01s. Complete actual red equals the two names above. The real editor case reaches its `footer.inert === false` assertion and fails; the existing input case fails actual `document.activeElement === editor` (body remains focused), so reopening authority is not evidenced by an attribute check alone. No missing/additional failure. Restored original bytes/hash; full C12 170/170, exit 0, 73.46s. The complete actual-red comparison returned true for both runs. Logs: `/tmp/footer-m2.log`, `/tmp/footer-m2-actual.json`, `/tmp/footer-m2-restored.log`, `/tmp/footer-m2-restored.json`, `/tmp/footer-m2-evidence.log`, predeclaration `/tmp/footer-m2-expected.json`.

## Historical footer-only remaining work and proposed future contract

Full Sheet drag acquisition, painted velocity handoff, reversal, cancel, reduced motion, snap/multi-sheet behavior and integration migration are unfinished. The separate owner design remains a proposal for Planner decision; this footer slice neither consumes its solution nor freezes a new public port. Future UI work must reuse shared motion, declare host/gesture/focus ownership and obtain its own production-entry red and complete observable mutation evidence. No claim of original-page/device preview, soft-keyboard device coverage, Tier 1 pass, remote CI or overall migration completion is made here.

## Revocable exit focus (audit 538449)

Planner approved a narrow amendment to design 518614: each logical close owns
one default final-focus permission until its native exit completes. A successful
native `focusin` to a connected, available external element permanently revokes
that permission for this close, including if the chosen node later disappears.
The Sheet verifies actual activeElement, native node/lifetime and current closed
intent. BODY/document-root blur, retained panel/footer descendants, disabled,
inert, hidden or aria-unavailable elements and synthetic/stale events grant no
choice. Geometry changes keep the same permission; reopen, presentation change
and disposal remove its listener. Native close and retained teardown still run.
No-choice exit restores the connected destination once with preventScroll after
native close. Reduced/synchronous exit creates no artificial choice window.
The existing opening focusInput guard and snapshot → native close → attached
selection-restoration order remain unchanged. No app-specific focus selector,
new timer, retry, native-close compensation or shared-helper change is added.

The original-page audit is an inherited-policy/contract gap, not evidence that
PR #3015 introduced a regression. `/tmp/sheet-focus35/findings.txt` and the four
`report-{320,390}-{button,escape}-external.json` traces show one successful
external choice with native open/inert exit still running, followed 477–537ms
later by Quick chat restoration. Base source has the same default policy; this
worker does not claim a base browser comparison. The task's `findings/` spelling
refers to the actual flat directory and `findings.txt`.

A real ConversationSurface production target first reported four behavioral
reds (320/390 × button/Escape) with its no-choice control green:
`cd fe && npx vitest run --project browser web/src/app/router/mobile-conversation-motion.browser.test.tsx --passWithNoTests=false --reporter=default --reporter=json --outputFile=/tmp/sheet-choice-production-red.json`
(`/tmp/sheet-choice-production-red.log`). After the owner fix its eight tests
and 42 Sheet owner tests passed together (`/tmp/sheet-choice-first-green.json`).
Owner coverage now additionally exercises coarse focus, removed choice nodes,
geometry rebases/unavailable frames, rapid reopen/independent closes, invalid
focus attempts, runtime reduced exit, zero-window reduced close and host moves.
Actual Chromium native close leaves an already-held external focus intact when
default restoration is skipped; removing that external node leaves BODY, with
no revived default. This verifies this browser, not every native platform.

An initial test invocation from the repository root failed with npm EROFS while
resolving Vitest; the corrected command uses the installed frontend tool from
`fe/`. The first owner red also exposed an invalid fixture assumption that one
frame would force Chromium inert blur; it now uses actual native editor.blur()
and verifies BODY, rather than waiting longer to create a choice window.

Final focused command: `python3 /tmp/sheet-focused.py sheet-choice-pregreen`,
16 whole targets / 232 pass. Fresh relative-import reachability still finds 90
consumers (`/tmp/sheet-choice-reachability.txt`). Production has one Sheet caller,
ConversationSurface through router/public. The existing conservative 16-target
scope above includes native close, reopen, focus, host selection and viewport
consumers. Excluded browser branches remain desktop-only, open-only or close
nested non-Sheet layers; DOM/contract targets cannot observe native focus.
No permanent roster or registration was added: ordinary new cases edit their
own target only. Kernel gate alone owns text/lint/build and its focused replay;
CI owns broad suites. Changed-file eslint passed (`sheet-choice-final-eslint.log`).

Exclusive serial production-only mutation commands actually run:
`python3 /tmp/sheet-choice-mutate.py revoke` and
`python3 /tmp/sheet-choice-mutate.py stale-exit` (cwd repository root).
`/tmp/sheet-choice-mutation-plan.json` predeclares every expected failing name.
Revoke changes only the successful-choice latch assignment: complete actual red
13 exactly matches prediction (production 4, coarse 4, owner 5), 219 pass.
Stale-exit removes the two checks of the same completion-authority factor:
only `reverses from native paint and velocity and fences the superseded exit`
fails as predicted, 231 pass. Both runs execute all 16 targets / 232 tests,
restore original bytes in finally, then each replay all 232 green. Both restore
SHA-256 `ad9b806aa9009e8cd7372965230292514db49da0b01a0c55f866861adf2ddbe0`.
Declarations, full red/restored JSON/logs and comparison/hash records use
`/tmp/sheet-choice-{revoke,stale-exit}-*`. Completion is checked again after native
close because native focus handlers can synchronously supersede the close.
Unchanged gesture cap/grab/paint-velocity/cancel and footer factors retain their
prior evidence; these new focus cases do not observe additional gesture-velocity
or retained-interaction mutant failures. The changed completion factor was rerun.

Planner message `a7fbfd316fc740fe97e89f890a79b024` reports PR #3014 merged as
`d6b4dda26b38eb8cbc6183ee2a532208f0cf96bd`. At that historical focus head the checkout had not consumed it; that evidence remains rate 1. The upstream integration above supersedes this prerequisite. Planner still owns two fresh L2 reviews, exact final-head CI and original-page checks.
No device/base browser claim or final merge approval is implied.

Gate run 1 passed text checks but lint found 37 errors in six pre-existing
ignored original-page/probe `.js/.cjs` harness scripts under `fe/test-results`.
Those scripts were relocated, with byte/hash checks, to
`/tmp/sheet-choice-ignored-lint-scripts/`; the exact reversible path/hash manifest
is `/tmp/sheet-choice-ignored-lint-relocation.json`. Screenshots/JSON/logs stay
in place. No product/config/dependency or lint-ignore change was made. Build and
focused replay were not reached in that run; the gate must be rerun unchanged.
