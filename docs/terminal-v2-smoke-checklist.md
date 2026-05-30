# Terminal v2 manual smoke checklist

Companion to `crates/calm-server/tests/ws_terminal_e2e.rs`. Automated
coverage exercises the wire-level happy path
(`ClientHello → ServerHello → Input → RenderPatch → ResizeCommit
→ ResizeApplied → Kill → TerminalExited`) end-to-end against a real
daemon binary backing `/bin/sh`. This file enumerates the things that
**only a real browser** can validate — xterm.js rendering pipeline,
focus / clipboard semantics, drag-resize debouncing in the React
component, multi-tab owner/observer flips, and version-skew UX.

Run before bumping `WEB_COMPAT_VERSION`, before any change to
`web/src/cards/builtins/terminal/*`, and before cutting a v2 release.

## Setup

1. `cargo run -p calm-server` (in one terminal).
2. `npm run dev` in `web/` (in another terminal).
3. Open the dev server URL in a fresh Chrome / Firefox profile (so
   `localStorage` is clean — required for the WEB_COMPAT_VERSION
   case below).
4. Create a cove + wave, then `Add → Terminal`. The terminal card
   should mount xterm.js and show a shell prompt within ~1s.

## Checklist

- [ ] **Snapshot bytes render cleanly.** Open a fresh terminal card —
      xterm.js paints the initial prompt with no garbled escape
      sequences, no leftover ANSI literals, no "ESC[?2004h" visible
      as text.
- [ ] **Typing echoes immediately.** Type `echo hi` then Enter —
      output appears under the prompt without lag, no double-echo,
      no missing characters. The cursor is in raw mode (no Backspace
      eating two chars at once, no Ctrl+W deleting a word locally
      instead of being sent to the PTY).
- [ ] **Drag-resize debounces.** Drag the card's resize handle slowly
      across multiple cell widths. The frontend should send at most
      one `ResizeCommit` per ~100ms idle window (check Network → WS
      frames in DevTools), and the terminal contents should
      reconcile via `ResizeApplied` without snapping back to the old
      geometry.
- [ ] **vim opens, scrolls, exits cleanly.** Run `vim README.md` —
      alt-screen activates (the prompt disappears), `j`/`k` scroll
      smoothly, `:q` returns to the prompt with no residual ANSI
      garbage. (Known caveat per #69: any alt-screen leak should be
      recoverable with `clear`.)
- [ ] **Tab close + reopen triggers fresh `ServerHello`.** Close
      the card. Re-open via card history / wave navigation. A new
      `ClientHello` goes out and a fresh `ServerHello` arrives — the
      restored screen shows the daemon's current viewport, not a
      stale cached copy.
- [ ] **Daemon crash recovery.** With a terminal open, find the
      supervised child PID and `kill -9` it.
      The WS bridge closes, the React component shows an error
      state (or auto-reconnects), and refreshing / re-opening the
      card spawns a fresh daemon via the `resolve_live_sock` cold
      path. No zombie sockets left in `data_dir`.
- [ ] **Framing-skew cleanup.** Drop a stale daemon binary (different
      `FRAME_VERSION`) at the resolved bin path, kill the current
      daemon, then re-attach. The bridge logs an `error!` with
      `terminal framing magic mismatch — closing WS`, clears
      supervisor handle, and the next attach reattaches or respawns cleanly.
- [ ] **Owner-only Input enforcement.** Open the same terminal in
      two browser tabs. The first attach is the owner — it can type.
      The second tab attaches as observer and types — keystrokes
      are rejected with `ProtocolError { code: NotOwner }` and the
      UI surfaces a non-modal "read-only" hint.
- [ ] **OwnerClaim handoff.** From the observer tab, trigger
      "claim owner" (UI button or `?claim=1` URL hint, whichever
      ships). The original owner gets `OwnerChanged { owner_client_id:
      <new uuid> }` and its input is now rejected; the claimer's
      input flows.
- [ ] **`WEB_COMPAT_VERSION` skew banner.** Open DevTools console,
      `localStorage.setItem('compat_override', '1')` (or set
      `WEB_COMPAT_VERSION = 1` in `web/src/api/version.ts` and
      rebuild), refresh. The app should paint an overlay banner
      ("compat v1 not supported — refresh required") that blocks
      interaction. Restore + reload returns to normal.
- [ ] **`kernel_originated_input` is stripped at the WS bridge.**
      In DevTools console, intercept the outgoing `ClientHello`
      and flip `capabilities.kernel_originated_input` to `true`
      before send. Confirm via server logs that the daemon sees
      `false` (the bridge's strip path in `ws/terminal.rs` zeroes
      it). With observer-role tab, observer Input is still
      rejected with `ProtocolError { NotOwner }` — the trust flag
      can't be forged from the browser.
- [ ] **Reconnect mid-session preserves visible state.** Mid-output,
      kill the WS connection (DevTools Network → close WebSocket
      frame). The card auto-reconnects, sends a fresh `ClientHello`
      with `resume_from` set to the last-known `render_rev`, and
      receives either a delta `RenderPatch` chain or a
      `RenderSnapshot` resync. The user-visible screen has no flash
      of blank.
- [ ] **`ChildReady` arrives before injected input fires.** If the
      task-dispatch platform is wired (or via manual probe in the
      WS console), confirm the daemon sends exactly one
      `ChildReady { pty_seq, render_rev }` after the shell prompt
      stabilizes, and that injecting a `\r` before `ChildReady`
      arrives is correctly deferred / dropped per design.
- [ ] **Clean exit on `exit`.** Type `exit` + Enter at the shell
      prompt. `TerminalExited` arrives with code 0; the card shows
      a non-modal "session ended" state and the orphan sweeper
      removes the row within its 30s tick.
