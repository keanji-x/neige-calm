# Mobile recovery verification (#1712)

This records the S1/S2 implementation checks from 2026-09-16. It does not claim
completion of the separately delivered host service or one-scan enrollment work.

## Application and transport checks

- The first regression used `ProductionApp` and the real router with whoami held
  pending. It failed because the saved Track had no local navigation/frame, then
  passed after introducing the presentation gate. Business routes and version
  requests remain gated behind identity validation.
- The final local FE suite passed 3,301 tests with one pre-existing skipped test.
  Complete lint, architecture, ownership, and TypeScript checks passed. Bundled
  builds use the real frontend generator and declare every packaged asset.
- Real Chromium tests mount the production application/router and event driver
  with scripted network responses. They preserve the mounted Track during
  recovery, wait for a real `replay-complete` frame before showing connected,
  and capture 390-pixel cold, warm, and restored views. A separate production
  mount regression proves logged-out pairing verification is inside the viewport.
- Actual plugin mutation hooks reproduce and prevent stranded pending leases.
  Local leases are acquired after admission and released in `finally`; stale
  response/cache callbacks remain fenced. Both enable and uninstall are covered,
  including overlapping old/new intents on one row and deferred `onMutate`.
- Actual attachment and command hooks reproduce an old picked file being posted
  after its file read crossed a recovery generation. The required lazy `readBytes`
  port now captures admission before file reading and reuses that permit at send.
- Five production mutations were applied individually: unauthorized delivery,
  write admission, response checkpoint, upload permit reuse, and onMutate
  generation validation. Every complete failing-test set matched its prediction;
  every source was restored byte-for-byte and its 16-test focused suite was green.

Local evidence is retained in `/tmp/1712-final-mutations/`,
`/tmp/1712-followup-final-{lint,tests}.log`, and the related `/tmp/1712-*.log`
files. Browser screenshots are in `fe/test-results/1712-*.png`. These are local
artifacts, not claims of a real online server or physical network-switch test.

## Native checks and observed regressions

The ARM64 app and separate `.instrumented` test APKs were compiled, and all 12
Kotlin/JVM cases passed. Mobile launcher/scanner Chromium tests passed 16 cases,
mobile Node tests passed 17 cases, and the mobile Go proxy tests passed. The
installable debug APK verifier checked six bundled assets and both native
libraries, and confirmed that the instrumentation CA was absent.

The coordinating task ran the isolated APK on an actual API 35 Android device.
Two production defects were reproduced and corrected without changing their
assertions:

1. Activity recreation created a new WebView while the process-owned Tauri
   plugin retained its destroyed Activity. `MainActivity.onWebViewCreate` now
   hands ownership to the existing plugin, cancels the former generation, and
   restores the saved route. The same test also checks the real Tauri bridge:
   correctly formed save/configuration and camera requests from bundled remote
   content must receive an explicit permission denial and leave profiles intact.
2. A callback from the retired WebView could overwrite the new view's resume
   pointer. The observer now captures its issuing generation and verifies both
   the observer instance and WebView owner. The exact same test APK first
   observed `/next/track/retired-callback`, then retained the expected
   `/next/settings/network` with only the production fence changed.

Those original test invocations completed their assertions but did **not** finish
with a successful instrumentation runner: explicitly closing the last Activity
then caused `FORTIFY: pthread_mutex_lock called on a destroyed mutex` and
`Process crashed`.

## Existing last-Activity close limitation

A real detached worktree at `5f2ff75c6c95a2a775594672c69d3bc13db93189` reproduced
the same runner termination with only one new test: empty launcher, no configured
network candidate, then `ActivityScenario.close()`. Its tracked production diff
was empty. The baseline app SHA-256 was
`f0c5aea31bc5324069c6a2397d44fec1b6eb9e02773e1a48f633e06752219ada` and its test APK
SHA-256 was `496998804349a473c1d983c8728f01d3363c34c022f0a892ea2635508103b7a4`.

The pinned `tauri-runtime-wry 2.11.4` routes the final destroyed window through
`ExitRequested` and `ControlFlow::Exit`; Tauri's `run` documentation explicitly
states that completion ends the host process. The observed FORTIFY is recorded
as an existing framework/device-combination limitation, **not fixed by this
change**. An in-process test runner cannot treat its own host termination as a
successful test teardown.

The recovery tests therefore retain the Activity until after the runner reports.
The external driver then owns process termination. This does not remove any
recovery, recreation, permission, or retired-callback assertion.

## Real process-death acceptance driver

After installing the matching `.instrumented` app/test pair, run:

```sh
python3 mobile/tests/native/run-recovery-process.py \
  --adb /path/to/adb --serial AUTHORIZED_TEST_DEVICE \
  --output-dir /tmp/neige-recovery-process-results
```

The driver addresses only `io.neigecalm.next.instrumented`. It does not install
packages, clear application data, change networking, or operate release builds.
It requires a complete `OK (1 test)` and successful instrumentation result for
every stage, rejecting `Process crashed` even after an assertion-level pass.

Stages are cold offline entry, warm/recreated view plus ACL/history fences,
seed from real visited history, normal app launch, explicit force-stop, and
read-only verification in a new process. Android ends the instrumented target
after the seed runner completes, so the driver explicitly starts the ordinary
Activity and records its live PID before force-stop. That PID must disappear.
Seed, normal-app, and check PIDs are recorded; the check PID must differ from both
prior PIDs, with the same saved profile identity, configuration revision, and
route. The check stage does not reseed them.

The complete external-driver result and final rebuilt APK checks remain with the
coordinating task at this checkpoint. API 26, physical Wi-Fi/cellular switching,
and long-idle real terminal/server coverage must be reported separately rather
than inferred from Chromium network fixtures.
