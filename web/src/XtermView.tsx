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
 *  via `DaemonMsg::ProtocolError(UnsupportedVersion)` and the overlay below. */
const PROTOCOL_VERSION = 2;

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

  // Live-apply theme changes without rebuilding the Terminal + WS.
  // xterm.js exposes `term.options` as a mutable bag; assigning
  // `term.options.theme = ...` is the official re-theming path. Putting
  // this in its own effect keeps the (heavy) bridge-mount effect's deps
  // small and lets us drop `theme` from there.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.theme = theme === 'dark' ? DARK_THEME : LIGHT_THEME;
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
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
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

    const send = (msg: ClientMsg) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(msg));
      }
    };

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
      // 1006 = abnormal closure (network / proxy cut, no Close frame).
      // 1011 = server-side heartbeat trip (see ws/terminal.rs PONG_TIMEOUT).
      // 1001 = endpoint going away (server restart, page navigation).
      setCloseInfo({ code: e.code, reason: e.reason || '' });
      dlog('XtermView', 'WS close', {
        code: e.code,
        reason: e.reason,
        wasClean: e.wasClean,
      });
      // Don't clobber a more-specific terminal state (`exited`,
      // `protocol-error`) — those overlays carry richer information than
      // the generic close code.
      setStatus((prev) =>
        prev === 'exited' || prev === 'protocol-error' ? prev : 'closed',
      );
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
      // Only clear the ref if it's still pointing at *this* term. A
      // strict-mode double-invoke teardown can run after the next mount
      // has already installed its own term; without this guard we'd null
      // out the new instance.
      if (termRef.current === term) termRef.current = null;
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
    <div className="xterm-view">
      <div ref={containerRef} className="xterm-container" />
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
              ? ' (refresh required for protocol v2)'
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
