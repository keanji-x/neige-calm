#573 r5 fixes

- Server HTTP wave-file test: isolated the router AppState EventBus from
  the MCP fixture EventBus in `tests/support/wave_file.rs`.
- Root cause: `request_codex()` logged `codex.job_requested` on the same
  bus that `AppState::from_parts()` subscribed a dispatcher to; the test
  also created the worker manually, so the dispatcher could race and mint
  a second worker card.
- Web invalidation: added `runtime.started`, `runtime.status_changed`, and
  `runtime.superseded` to the wave-file derived event helper.
- Runtime events now invalidate owning wave detail, card overlays, and
  `['wave-files', waveId]`, matching `cards/<id>/.payload.json` runtime
  status projection.
- A11y menu focus: changed Menu hover focus from `onMouseEnter` to
  `onMouseMove`.
- Root cause: a keyboard-opened menu can mount under a stationary pointer;
  mount-time mouseenter could move roving focus from Terminal (index 0) to
  Codex (index 1) before the e2e assertion.
- Added/updated unit coverage in `eventBridge.test.tsx` and
  `Menu.test.tsx` for the changed contracts.
