import { useEffect, useRef } from 'react';
import { useState } from './shared/state';
import { Terminal, type ITheme } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { dlog } from './util/debug';
import { makeUuid } from './util/uuid';
import { MONO_STACK } from './font-stack';
import type {
  ClientMsg,
  DaemonMsg,
  ProtocolErrorCode,
  Role,
} from './api/generated-terminal';
import { LIGHT_THEME_RGB, DARK_THEME_RGB } from './api/themeRgb';

// Cool-neutral light xterm theme matching Calm's palette. Same numbers as
// the previous useTerminalCore-backed version; only the wire below changed.
const LIGHT_THEME: ITheme = {
  background: '#ffffff00',
  foreground: '#2a2f3a',
  cursor: '#2a2f3a',
  cursorAccent: '#ffffff',
  selectionBackground: 'rgba(60, 100, 200, 0.22)',
  black: '#1a1d22',
  red: '#c43b3b',
  green: '#2f8c3c',
  yellow: '#a07a14',
  blue: '#3464c2',
  magenta: '#8b3b9a',
  cyan: '#2a8a8a',
  white: '#d9dbe0',
  brightBlack: '#5b626d',
  brightRed: '#e0625b',
  brightGreen: '#4faa5e',
  brightYellow: '#c89a30',
  brightBlue: '#5c87d8',
  brightMagenta: '#aa5cb8',
  brightCyan: '#4cb0b0',
  brightWhite: '#f6f7f9',
};

const DARK_THEME: ITheme = {
  ...LIGHT_THEME,
  background: '#ffffff00',
  foreground: '#d8dbe2',
  cursor: '#d8dbe2',
  selectionBackground: 'rgba(140, 180, 255, 0.22)',
};

interface XtermViewProps {
  /** `Terminal.id` from the kernel — addresses the daemon socket on the server. */
  terminalId: string;
  theme?: 'light' | 'dark';
  /**
   * Lift the daemon-assigned role (from `ServerHello.client_role`) out to the
   * parent card so the role indicator can live in `<CardHead>`'s status slot
   * instead of as a corner overlay inside the xterm view. `null` is reported
   * on reconnect / disconnect so the parent can clear any badge. Owners are
   * the common single-user case and intentionally don't render a badge there
   * — the parent decides what (if anything) to show per role.
   */
  onRoleChange?: (role: Role | null) => void;
}

/** Last close info, surfaced in the gray "disconnected" overlay so the user
 *  (and we) can tell at a glance whether it was a proxy cut (1006), a
 *  server-side heartbeat trip (1011), a clean server close (1001), etc. */
interface CloseInfo {
  code: number;
  reason: string;
}

/** Wire version the frontend speaks. Must match
 *  `crates/calm-session/src/lib.rs::PROTOCOL_VERSION`. A mismatch surfaces
 *  via `DaemonMsg::ProtocolError(UnsupportedVersion)` and the overlay below.
 *  Bumped 2 → 3 in #177 for the `ClientMsg::TerminalThemeUpdate` variant
 *  the daemon uses to update its OSC 10/11 defaults and nudge a
 *  focus-aware TUI to re-query on host theme toggles. */
const PROTOCOL_VERSION = 3;

/**
 * UI status for the v2 terminal protocol. Slimmed-down state machine
 * compared to v1: a clean break is fine (compat is gated by
 * `WEB_COMPAT_VERSION`) so we don't carry transitional states.
 *
 *   connecting    — WebSocket opening
 *   handshaking   — WebSocket open, awaiting `ServerHello`
 *   connected     — `ServerHello` received, streaming
 *   closed        — WS closed (or errored) before exit
 *   exited        — daemon sent `TerminalExited` (terminal mode child exited)
 *   protocol-error — daemon sent `ProtocolError`; connection terminated
 */
type Status =
  | 'connecting'
  | 'handshaking'
  | 'connected'
  | 'closed'
  | 'exited'
  | 'protocol-error';

interface ProtocolError {
  code: ProtocolErrorCode;
  message: string;
}

interface ExitInfo {
  code: number | null;
}

/**
 * Direct bridge to calm-server's `/api/terminals/:id` WS endpoint, speaking
 * the v2 terminal protocol (issue #44). Frames are JSON-encoded `ClientMsg`
 * / `DaemonMsg` from the `calm-session` Rust crate (TS types regenerated
 * via `npm run gen:api`). `Vec<u8>` rides as a plain JS `Array<number>`.
 *
 * Roles: this component always sends `role_hint: 'Owner'` — the browser is
 * the user's primary interaction surface. The daemon's `OwnerRegistry` may
 * still assign `Observer` (e.g. another client already holds owner), in
 * which case `Input` frames are rejected with `NotOwner`. The assigned role
 * is reported up to the parent card via `onRoleChange` so the
 * `<CardHead>` status slot can render an `observing` pill when relevant;
 * owners (the common single-user case) render no badge at all.
 * `kernel_originated_input` would never apply to a browser tab so we leave
 * it out of the capability set.
 */
export function XtermView({
  terminalId,
  theme = 'light',
  onRoleChange,
}: XtermViewProps) {
  // #177 — Playwright instrumentation. Gated on `?testMounts=1` so
  // production users never carry the side effect. A real mount bumps
  // `window.__xtermMounts__` by 1; unmount decrements. The e2e
  // regression spec (`web/e2e/a11y-177-theme-toggle-no-remount.spec.ts`)
  // reads this between theme-toggle steps to pin "no remount on theme
  // toggle" as a contract.
  useEffect(() => {
    if (typeof window === 'undefined') return;
    const url = new URL(window.location.href);
    if (url.searchParams.get('testMounts') !== '1') return;
    const w = window as unknown as { __xtermMounts__?: number };
    w.__xtermMounts__ = (w.__xtermMounts__ ?? 0) + 1;
    return () => {
      if (w.__xtermMounts__ !== undefined) w.__xtermMounts__ -= 1;
    };
  }, []);

  const containerRef = useRef<HTMLDivElement | null>(null);
  // Live ref to the active xterm.js Terminal instance so a sibling effect
  // can re-apply the theme without tearing down the WebSocket + replay
  // state. xterm.js reads `term.options.theme` lazily on every render
  // cycle, so reassigning it triggers an immediate repaint at the next
  // flush. See the theme-apply effect below.
  const termRef = useRef<Terminal | null>(null);
  // Latest theme prop, captured into a ref so the main bridge-mount effect
  // (which omits `theme` from its deps on purpose — see the theme-apply
  // effect below) can still read the *current* theme when constructing
  // the Terminal on (re)mount or after a reconnect.
  const latestThemeRef = useRef<'light' | 'dark'>(theme);
  latestThemeRef.current = theme;
  const [status, setStatus] = useState<Status>('connecting');
  const [closeInfo, setCloseInfo] = useState<CloseInfo | null>(null);
  const [protocolError, setProtocolError] = useState<ProtocolError | null>(null);
  const [exitInfo, setExitInfo] = useState<ExitInfo | null>(null);
  // Role lives entirely in the parent now (via `onRoleChange`) so the badge
  // can sit in `<CardHead>`'s status slot instead of overlaying the terminal.
  // We capture the latest callback into a ref so the heavy bridge-mount
  // effect doesn't need it in its deps — a callback identity flip from the
  // parent shouldn't tear down the WebSocket.
  const onRoleChangeRef = useRef<XtermViewProps['onRoleChange']>(onRoleChange);
  onRoleChangeRef.current = onRoleChange;
  // Bumping this re-runs the WS effect, which rebuilds the WS and re-attaches
  // to the daemon. The daemon survives WS disconnects (it owns the PTY and a
  // replay buffer / render rev), so reconnect is usually enough; if the
  // daemon also died, the server's `resolve_live_sock` respawns it.
  const [reconnectKey, setReconnectKey] = useState(0);
  const reconnect = () => {
    setStatus('connecting');
    setCloseInfo(null);
    setProtocolError(null);
    setExitInfo(null);
    onRoleChangeRef.current?.(null);
    setReconnectKey((k) => k + 1);
  };

  // #177 — live `send` from the WS-mount effect, captured so the
  // theme-effect can post `TerminalThemeUpdate` without owning the
  // WebSocket itself. Cleared back to `null` on teardown.
  const sendRef = useRef<((msg: ClientMsg) => void) | null>(null);
  // #177 — buffer for a `TerminalThemeUpdate` produced before the WS
  // effect populates `sendRef`. On a fresh mount the theme-effect can
  // fire before the bridge-mount effect runs (React effects execute in
  // declaration order, but the bridge-mount effect bails early on
  // `!container` during the strict-mode double-invoke), so without this
  // buffer the dispatch would no-op. The WS-mount effect drains this
  // right after assigning `sendRef.current = send`.
  const pendingThemeRef = useRef<ClientMsg | null>(null);

  // Live-apply theme changes without rebuilding the Terminal + WS.
  // xterm.js exposes `term.options` as a mutable bag; assigning
  // `term.options.theme = ...` is the official re-theming path. Putting
  // this in its own effect keeps the (heavy) bridge-mount effect's deps
  // small and lets us drop `theme` from there.
  //
  // #177 — also dispatch `TerminalThemeUpdate` over the WS on every
  // run of this effect, including the initial mount. We deliberately
  // do NOT gate on a "did theme change since last run?" check: a
  // remount (Suspense flash, persist-query hydration, anything else
  // that re-runs the lazy chunk) resets any per-component `prev`
  // bookkeeping and would skip the dispatch — exactly the bug we're
  // closing. The unconditional POST is safe because suppression lives
  // on the daemon side, not here: (a) the session state machine drops
  // the update when fg/bg already equal the current defaults (the
  // mount-time no-op case), and (b) the daemon's only mid-session
  // write is `ESC[I`, gated on whether the PTY child has opted into
  // DECSET 1004 (focus event reporting). A focus-aware TUI like codex
  // enables 1004 and treats `ESC[I` as `FocusGained`, re-querying OSC
  // 10/11 — the daemon then synthesizes the reply from the updated
  // defaults. An interactive shell at its prompt drives the line via
  // a raw-mode editor (zsh's ZLE) but never enables 1004, so without
  // the gate a stray `ESC[I` would land in its line buffer. (Pre-#305
  // the daemon also wrote unsolicited `OSC 10;rgb:… OSC 11;rgb:…`
  // pairs; that double-belt was dropped in #305 in favor of the
  // solicited-only loop.) See crates/calm-session `on_client_frame`
  // TerminalThemeUpdate + daemon `Effect::TerminalThemeUpdate`, gated
  // on `RenderPlane::focus_event_tracking`.
  useEffect(() => {
    const term = termRef.current;
    if (term) {
      term.options.theme = theme === 'dark' ? DARK_THEME : LIGHT_THEME;
    }
    const rgb = theme === 'dark' ? DARK_THEME_RGB : LIGHT_THEME_RGB;
    const msg: ClientMsg = {
      TerminalThemeUpdate: { fg: rgb.fg, bg: rgb.bg },
    };
    if (sendRef.current) {
      sendRef.current(msg);
    } else {
      // WS effect hasn't installed `send` yet. Buffer here; the
      // WS-mount effect drains immediately after assigning
      // `sendRef.current` so the frame still reaches the daemon
      // (via the `pendingFrames` queue inside `send` when readyState
      // is CONNECTING, or directly once OPEN).
      pendingThemeRef.current = msg;
    }
  }, [theme]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    dlog('XtermView', 'mount START', {
      terminalId,
      containerW: container.offsetWidth,
      containerH: container.offsetHeight,
    });

    const term = new Terminal({
      theme:
        latestThemeRef.current === 'dark' ? DARK_THEME : LIGHT_THEME,
      fontFamily: MONO_STACK,
      fontSize: 12.5,
      convertEol: true,
      allowProposedApi: true,
      cursorBlink: true,
    });
    termRef.current = term;
    // OSC-echo regression instrumentation. Gated on `?testMounts=1` (so
    // production never carries it) exactly like `__xtermMounts__` above.
    // Registers a per-terminal buffer serializer keyed by `terminalId`,
    // so the e2e spec (`web/e2e/new-terminal-osc-echo.spec.ts`) can dump
    // the rendered grid of a SPECIFIC card (a wave can have several
    // xterm-backed cards — e.g. the auto-minted codex spec card plus an
    // AddPanel New-terminal card — and only the cooked-shell terminal
    // can manifest the echo bug). The spec asserts no OSC 10/11 reply
    // bytes land in the grid as literal caret text (`]10;rgb:` /
    // `]11;rgb:`). We read the buffer rather than the DOM
    // because xterm's canvas/webgl renderer doesn't mirror glyphs into
    // navigable DOM nodes.
    if (typeof window !== 'undefined') {
      const url = new URL(window.location.href);
      if (url.searchParams.get('testMounts') === '1') {
        const w = window as unknown as {
          __xtermDumps__?: Record<string, () => string>;
        };
        const dumps = (w.__xtermDumps__ ??= {});
        dumps[terminalId] = () => {
          const buf = term.buffer.active;
          const lines: string[] = [];
          for (let i = 0; i < buf.length; i += 1) {
            const line = buf.getLine(i);
            if (line) lines.push(line.translateToString(true));
          }
          return lines.join('\n');
        };
      }
    }
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    // #177 — suppress xterm.js's built-in OSC 10/11/12 auto-reply.
    // The daemon is the sole authoritative responder (it knows the
    // host browser's *real* surface color via `--terminal-fg/-bg`
    // and the `TerminalThemeUpdate` stream below); xterm.js's local
    // reply would race a wrong value back (its `clearColor` is the
    // transparent `#ffffff00` we configure above, which serializes
    // to `rgb:ffff/ffff/ffff/0000` and codex parses as pure white).
    // Returning `true` from the OSC handler short-circuits xterm's
    // default behavior — the bytes are consumed, no reply is sent,
    // and the daemon's reply is the only thing on the wire.
    term.parser.registerOscHandler(10, () => true);
    term.parser.registerOscHandler(11, () => true);
    term.parser.registerOscHandler(12, () => true);
    // Tab-trap mitigation — issue #236 followup. xterm.js creates a
    // `<textarea class="xterm-helper-textarea" tabindex="0">` inside the
    // container; once focus lands on it, xterm's keydown handler captures
    // every Tab (forwarded to the PTY as `\t`) so the browser never moves
    // focus off the terminal. That's fine for users who clicked into the
    // terminal deliberately — but it turns the terminal into a one-way
    // focus trap during plain Tab navigation across the wave page, which
    // breaks keyboard-only nav (`web/e2e/a11y-keyboard.spec.ts`) the
    // moment a wave has any xterm-backed card. Demote the textarea out
    // of the natural Tab order; users still engage the terminal by
    // clicking (xterm.js's mousedown handler focuses it), and once
    // focused all keys (including Tab → tab-completion) still flow to
    // the PTY. The a11y contract (`docs/a11y-contract.md` §2.4) already
    // documents "xterm.js owns keys once the body is interacted with" —
    // this just makes the "interacted with" gate explicit.
    const helperTextarea = container.querySelector<HTMLTextAreaElement>(
      'textarea.xterm-helper-textarea',
    );
    if (helperTextarea) {
      helperTextarea.setAttribute('tabindex', '-1');
    }
    try {
      fit.fit();
      dlog('XtermView', 'fit DONE (initial)', {
        cols: term.cols,
        rows: term.rows,
        containerW: container.offsetWidth,
        containerH: container.offsetHeight,
      });
    } catch (e) {
      dlog('XtermView', 'fit FAILED (initial)', e);
      /* container may not be laid out yet on first frame */
    }

    const wsUrl = `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${
      location.host
    }/api/terminals/${encodeURIComponent(terminalId)}`;
    const ws = new WebSocket(wsUrl);

    // #177 — queue frames produced before the WS finishes its handshake.
    // The theme-effect (sibling below) can fire between `new WebSocket(…)`
    // and `ws.onopen` — the pre-#177 `send()` silently dropped such
    // frames and the daemon never learned about the toggle. Buffer here
    // and flush in `ws.onopen` (after the ClientHello). On WS close /
    // teardown the queue is GC'd along with the closure, so there's no
    // zombie-message risk.
    const pendingFrames: ClientMsg[] = [];
    const send = (msg: ClientMsg) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(msg));
      } else {
        pendingFrames.push(msg);
      }
    };
    // #177 — surface `send` so the theme-effect (above) can post
    // `TerminalThemeUpdate` without owning a WebSocket of its own.
    // Cleared in the teardown below.
    sendRef.current = send;
    // #177 — drain a `TerminalThemeUpdate` buffered by the theme-effect
    // before this WS effect ran. `send()` itself handles the
    // not-yet-OPEN case via `pendingFrames`, so this works on a cold
    // mount (where readyState is CONNECTING and the message rides the
    // `pendingFrames` queue until `ws.onopen` drains it) AND on a
    // reconnect (same path).
    if (pendingThemeRef.current) {
      send(pendingThemeRef.current);
      pendingThemeRef.current = null;
    }

    // Per-connection client id. The daemon's `OwnerRegistry` keys on this
    // so the same browser tab survives WS reconnects without losing
    // ownership. We can't call `crypto.randomUUID()` directly: it is
    // restricted to secure contexts (https + localhost), so the LAN-http
    // case (http://192.168.x.x:4040) hits `TypeError: crypto.randomUUID
    // is not a function`. `makeUuid()` falls back to a v4 synthesized
    // from `crypto.getRandomValues`, which is always available — see
    // `util/uuid.ts`.
    const clientId = makeUuid();
    // Monotonic resize epoch. Bumped on every `ResizeCommit` so a
    // `ResizeApplied` echo can be matched to its request (and stale
    // applies from a previous epoch ignored).
    let resizeEpoch = 0;
    let lastCols = term.cols;
    let lastRows = term.rows;
    // Track the latest render_rev / pty_seq the daemon emitted. Future
    // PRs use these to send `RenderAck` for back-pressure; today we just
    // keep them current for the (unimplemented) resume path.
    let renderRev = 0;
    let ptySeq = 0;

    // Liveness detection is owned server-side (ws/terminal.rs: 10s ping,
    // 30s pong_timeout — closes with 1011 on timeout). The browser's WS
    // impl handles TCP-level death itself and fires onclose/onerror. We
    // previously kept a 40s client-side timer too, but it only observed
    // JS-level `onmessage` (Text/Binary), NOT browser auto-pongs — so a
    // healthy WS attached to an idle codex prompt (no PTY output for 40s)
    // would false-positive close as code 1006. Server-side heartbeat
    // already covers the real failure modes; the client-side timer was
    // redundant and harmful.

    ws.onopen = () => {
      setStatus('handshaking');
      send({
        ClientHello: {
          protocol_version: PROTOCOL_VERSION,
          terminal_id: terminalId,
          client_id: clientId,
          desired_size: {
            cols: term.cols,
            rows: term.rows,
            pixel_width: null,
            pixel_height: null,
          },
          cell_size: null,
          // None = just current viewport. We deliberately don't ask for
          // history yet — the server-side scrollback story (CellGrid
          // patches, line-granular replay) lands in a follow-up PR.
          initial_scrollback: 'None',
          resume_from: null,
          // The browser is the user's primary interaction surface, so we
          // hint Owner. The daemon may still hand us Observer if someone
          // else (CLI client, another tab) already owns the session.
          role_hint: 'Owner',
          capabilities: {
            render_encodings: ['Vt'],
            supports_scrollback: true,
            supports_sixel: false,
            supports_images: false,
            // Browser is an untrusted ingress; the WS bridge force-strips
            // this to false on every ClientHello regardless of what we
            // send, but we declare false here to match the trust model
            // documented on the field (see crates/calm-session/src/lib.rs).
            kernel_originated_input: false,
          },
        },
      });
      // #177 — flush frames queued before the WS finished its handshake.
      // Typical culprit: a theme toggle in the brief window between
      // `new WebSocket(…)` and `ws.onopen`. Without this drain, the
      // toggle would be silently dropped at the readyState check in
      // `send()` and the daemon's OSC 10/11 defaults would never
      // update to match the new host theme. Drains via `ws.send`
      // directly (bypasses the queueing branch — we're definitely
      // OPEN inside `onopen`).
      while (pendingFrames.length > 0) {
        const queued = pendingFrames.shift()!;
        ws.send(JSON.stringify(queued));
      }
    };

    ws.onmessage = (e) => {
      let msg: DaemonMsg;
      try {
        msg = JSON.parse(typeof e.data === 'string' ? e.data : '') as DaemonMsg;
      } catch {
        return;
      }
      // Dispatch over the externally-tagged enum. Each branch narrows the
      // payload via TypeScript's discriminated-union rules; this is why
      // `DaemonMsg` is sourced from `generated-terminal.ts`.
      if ('ServerHello' in msg) {
        const sh = msg.ServerHello;
        onRoleChangeRef.current?.(sh.client_role);
        setStatus('connected');
        // Snapshot may be bigger or smaller than the viewport we opened
        // with; resize the local terminal to match before writing the
        // replay so the cursor lines up.
        if (sh.snapshot.cols !== term.cols || sh.snapshot.rows !== term.rows) {
          term.resize(sh.snapshot.cols, sh.snapshot.rows);
          lastCols = sh.snapshot.cols;
          lastRows = sh.snapshot.rows;
        }
        if (sh.snapshot.scrollback) {
          term.write(Uint8Array.from(sh.snapshot.scrollback));
        }
        term.write(Uint8Array.from(sh.snapshot.data));
        renderRev = sh.snapshot.render_rev;
        ptySeq = sh.snapshot.pty_seq;
        return;
      }
      if ('RenderPatch' in msg) {
        const p = msg.RenderPatch;
        if (p.encoding === 'Vt') {
          term.write(Uint8Array.from(p.data));
        }
        renderRev = p.render_rev;
        ptySeq = p.pty_seq;
        return;
      }
      if ('RenderSnapshot' in msg) {
        // Standalone snapshot — daemon decided we need a hard re-sync
        // (typically because a `ResizeCommit` triggered a model reframe).
        const s = msg.RenderSnapshot;
        if (s.cols !== term.cols || s.rows !== term.rows) {
          term.resize(s.cols, s.rows);
          lastCols = s.cols;
          lastRows = s.rows;
        }
        term.clear();
        term.write(Uint8Array.from(s.data));
        renderRev = s.render_rev;
        ptySeq = s.pty_seq;
        return;
      }
      if ('ResizeApplied' in msg) {
        const r = msg.ResizeApplied;
        // Stale-epoch guard: a `ResizeApplied` from a previous request
        // (out-of-order on a slow network) shouldn't clobber the now-newer
        // local geometry. We track epoch monotonically below.
        if (r.epoch < resizeEpoch) return;
        lastCols = r.cols;
        lastRows = r.rows;
        renderRev = r.render_rev;
        ptySeq = r.pty_seq;
        return;
      }
      if ('SnapshotRequired' in msg) {
        // Daemon is about to send a fresh snapshot. Clear local state and
        // wait — the `RenderSnapshot` will arrive next.
        term.clear();
        return;
      }
      if ('TerminalExited' in msg) {
        const t = msg.TerminalExited;
        setExitInfo({ code: t.code });
        setStatus('exited');
        term.writeln(
          `\r\n\x1b[2m[process exited${
            t.code != null ? ` (code ${t.code})` : ''
          }]\x1b[0m`,
        );
        return;
      }
      if ('ProtocolError' in msg) {
        setProtocolError({
          code: msg.ProtocolError.code,
          message: msg.ProtocolError.message,
        });
        setStatus('protocol-error');
        return;
      }
      if ('OwnerChanged' in msg) {
        // Single-user mode only: role is set on ServerHello and never
        // flips mid-session, so we just log for debug. When multi-client
        // handoff lands, this is the dispatch point to call
        // onRoleChangeRef.current?.(newRole) — but the daemon's payload
        // here only carries `owner_client_id`, so we'd need to compare
        // against our own clientId to derive the new role for THIS
        // client. Defer wiring until that semantic is validated.
        dlog('XtermView', 'OwnerChanged', msg.OwnerChanged);
        return;
      }
      if ('Backpressure' in msg) {
        // Wire shape only in this PR — the daemon never emits it yet, but
        // log if it ever shows up so we can debug. Future work: implement
        // policy-aware throttling.
        dlog('XtermView', 'Backpressure', msg.Backpressure);
        return;
      }
      // Chat-mode variants (`HelloChat`, `ChatEvent`, `ChildExited`) never
      // reach the terminal card path — the kernel routes them through the
      // codex card instead. We ignore them silently here for forwards-
      // compatibility against a misconfigured backend.
    };

    ws.onclose = (e) => {
      // Capture close code/reason so the gray "disconnected" overlay can
      // tell us what kind of disconnect this was without devtools.
      // 1000 + `child-exited` reason = daemon's clean child-exit close
      //   (see ws/terminal.rs::CLOSE_REASON_CHILD_EXITED). We map this
      //   to the `exited` overlay even if the prior `TerminalExited`
      //   JSON frame got dropped on a slow link — distinguishes a
      //   process finishing from a connection drop.
      // 1006 = abnormal closure (network / proxy cut, no Close frame).
      // 1011 = server-side heartbeat trip (see ws/terminal.rs PONG_TIMEOUT).
      // 1001 = endpoint going away (server restart, page navigation).
      setCloseInfo({ code: e.code, reason: e.reason || '' });
      dlog('XtermView', 'WS close', {
        code: e.code,
        reason: e.reason,
        wasClean: e.wasClean,
      });
      const isChildExitClose =
        e.code === 1000 && e.reason === 'child-exited';
      // Don't clobber a more-specific terminal state (`exited`,
      // `protocol-error`) — those overlays carry richer information
      // than the generic close code. A `child-exited` close promotes
      // us to `exited` even if the JSON exit frame never arrived.
      setStatus((prev) => {
        if (prev === 'exited' || prev === 'protocol-error') return prev;
        if (isChildExitClose) return 'exited';
        return 'closed';
      });
      // Role is undefined once the WS is gone — parent clears any pill.
      onRoleChangeRef.current?.(null);
    };
    ws.onerror = (e) => {
      dlog('XtermView', 'WS error', e);
      setStatus((prev) =>
        prev === 'exited' || prev === 'protocol-error' ? prev : 'closed',
      );
      onRoleChangeRef.current?.(null);
    };

    const dataSub = term.onData((d) => {
      const bytes = Array.from(new TextEncoder().encode(d));
      // Browser typing path: `input_seq: 0` means "no ack requested"
      // (option (b) from issue #115). The daemon writes the bytes and
      // stays silent — no `DaemonMsg::InputAck` frame is emitted on the
      // hot typing path. Only kernel-originated transient clients
      // (DaemonClient::inject_stdin) use non-zero seqs to await
      // deterministic delivery confirmation.
      send({ Input: { data: bytes, input_seq: 0 } });
    });

    // Batch resize work to one tick per animation frame and skip cases
    // where fit() didn't actually change the grid. RGL's resize handle
    // fires the ResizeObserver on every mousemove; without the rAF guard
    // the terminal re-fits and re-renders constantly, which shows up as a
    // 1-2px shake on the inner canvas.
    let pending = false;
    const onResize = () => {
      if (pending) return;
      pending = true;
      requestAnimationFrame(() => {
        pending = false;
        try {
          fit.fit();
        } catch {
          return;
        }
        if (term.cols !== lastCols || term.rows !== lastRows) {
          dlog('XtermView', 'resize → fit', {
            from: { cols: lastCols, rows: lastRows },
            to: { cols: term.cols, rows: term.rows },
            containerW: container.offsetWidth,
            containerH: container.offsetHeight,
          });
          // Bump epoch on every commit so the daemon can ignore stale
          // applies. `lastCols/Rows` stay at their previous value until
          // `ResizeApplied` confirms — otherwise a debounce / coalesce
          // could swallow a subsequent intentional resize back to the
          // same size.
          resizeEpoch += 1;
          send({
            ResizeCommit: {
              epoch: resizeEpoch,
              cols: term.cols,
              rows: term.rows,
            },
          });
        }
      });
    };
    const ro = new ResizeObserver(onResize);
    ro.observe(container);

    // Surface ack ref so future tests / devtools can inspect; unused at
    // runtime so the variable doesn't trip TS's no-unused warning.
    void renderRev;
    void ptySeq;

    return () => {
      ro.disconnect();
      dataSub.dispose();
      try {
        ws.close();
      } catch {
        /* already closed */
      }
      term.dispose();
      // Tear down this terminal's test-only buffer-dump hook (only
      // present under `?testMounts=1`). Keyed by `terminalId` so we only
      // remove our own entry, never a sibling card's.
      if (typeof window !== 'undefined') {
        const w = window as unknown as {
          __xtermDumps__?: Record<string, () => string>;
        };
        if (w.__xtermDumps__) delete w.__xtermDumps__[terminalId];
      }
      // Only clear the ref if it's still pointing at *this* term. A
      // strict-mode double-invoke teardown can run after the next mount
      // has already installed its own term; without this guard we'd null
      // out the new instance.
      if (termRef.current === term) termRef.current = null;
      // #177 — symmetric guard for `sendRef`. Same strict-mode
      // double-invoke risk as `termRef`: a teardown that runs after
      // the next mount installed its own `send` would null out the
      // new value. Only clear if we still own it.
      if (sendRef.current === send) {
        sendRef.current = null;
      }
      // Parent should reset any role pill when the bridge tears down — the
      // next mount will re-emit on `ServerHello`. Sync here (not via
      // onclose) so a strict-mode unmount or a `terminalId` change clears
      // the parent state even if no close frame fires.
      onRoleChangeRef.current?.(null);
    };
    // `theme` deliberately omitted: a theme flip should NOT rebuild the
    // WebSocket / Terminal. The sibling effect above mutates
    // `term.options.theme` in place. `latestThemeRef` captures the current
    // value for the initial constructor.
  }, [terminalId, reconnectKey]);

  return (
    <div className="xterm-view" data-terminal-id={terminalId}>
      {/* The xterm container is the canvas-style render surface xterm.js
       *  paints into — `.xterm-rows` / `.xterm-fg-*` spans the library
       *  emits per-cell are presentational decoration, not navigable
       *  text. Marking the wrapper `aria-hidden` (with `role=presentation`
       *  for older AT) excludes the entire xterm DOM subtree from axe
       *  scans and screen-reader content trees. The interactive surface
       *  (typing, paste, accessibility tree) is the `xterm-helper-textarea`
       *  xterm.js mounts inside the container — that node carries its
       *  own ARIA wiring and bypasses `aria-hidden` because it's the
       *  focusable input. Issue #236 followup: PR #239's sync-spawn
       *  makes the daemon's bold-green `runner@runner` shell prompt
       *  visible the moment the wave-list snapshot is taken, which
       *  triggered a `.xterm-fg-10.xterm-bold` color-contrast 2.81:1
       *  violation. Bumping xterm's palette to clear 4.5:1 would break
       *  parity with the user's terminal expectations (and would still
       *  flag the next palette index that happens to be brighter); the
       *  semantically-correct path is to scope axe (and AT) to the real
       *  text content, which lives outside the canvas render. The a11y
       *  contract (`docs/a11y-contract.md` §2.4) already documents
       *  "xterm.js owns keys once the body is interacted with" — this
       *  just makes the AT side of that contract explicit. */}
      <div
        ref={containerRef}
        className="xterm-container"
        aria-hidden="true"
        role="presentation"
      />
      {status === 'connecting' && (
        <div className="xterm-status">connecting…</div>
      )}
      {status === 'handshaking' && (
        <div className="xterm-status">handshaking…</div>
      )}
      {status === 'exited' && (
        <div className="xterm-status xterm-status-closed">
          <span>
            process exited
            {exitInfo?.code != null && (
              <span className="xterm-close-info"> (code {exitInfo.code})</span>
            )}
          </span>
          <button onClick={reconnect} className="xterm-restart">
            Restart
          </button>
        </div>
      )}
      {status === 'protocol-error' && protocolError && (
        <div
          className="xterm-status xterm-status-closed"
          role="alert"
          aria-live="assertive"
        >
          <span>
            protocol error: {protocolError.code}
            {protocolError.message ? ` — ${protocolError.message}` : ''}
            {protocolError.code === 'UnsupportedVersion'
              ? ' (refresh required for protocol v3)'
              : ''}
          </span>
          <button
            onClick={() => location.reload()}
            className="xterm-restart"
          >
            Refresh
          </button>
        </div>
      )}
      {status === 'closed' && (
        <div className="xterm-status xterm-status-closed">
          <span>
            disconnected
            {closeInfo && (
              <span className="xterm-close-info">
                {' '}— {closeInfo.code}
                {closeInfo.reason ? ` (${closeInfo.reason})` : ''}
              </span>
            )}
          </span>
          <button onClick={reconnect} className="xterm-restart">
            Reconnect
          </button>
        </div>
      )}
    </div>
  );
}
