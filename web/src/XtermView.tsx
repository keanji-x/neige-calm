import { useEffect, useRef } from 'react';
import { useState } from './shared/state';
import { Terminal, type ITheme } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { dlog } from './util/debug';

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
}

/** Last close info, surfaced in the gray "disconnected" overlay so the user
 *  (and we) can tell at a glance whether it was a proxy cut (1006), a
 *  server-side heartbeat trip (1011), a clean server close (1001), etc. */
interface CloseInfo {
  code: number;
  reason: string;
}

/**
 * Direct bridge to calm-server's `/api/terminals/:id` WS endpoint. Frames
 * are JSON-encoded `ClientMsg` / `DaemonMsg` from the `calm-session`
 * Rust crate; bytes ride as plain JS arrays of u8 values.
 */
export function XtermView({ terminalId, theme = 'light' }: XtermViewProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [status, setStatus] = useState<'connecting' | 'open' | 'closed'>(
    'connecting',
  );
  const [closeInfo, setCloseInfo] = useState<CloseInfo | null>(null);
  // Bumping this re-runs the WS effect, which rebuilds the WS and re-attaches
  // to the daemon. The daemon survives WS disconnects (it owns the PTY and a
  // replay buffer), so reconnect is usually enough; if the daemon also died,
  // the server's `resolve_live_sock` respawns it transparently.
  const [reconnectKey, setReconnectKey] = useState(0);
  const reconnect = () => {
    setStatus('connecting');
    setCloseInfo(null);
    setReconnectKey((k) => k + 1);
  };

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    dlog('XtermView', 'mount START', {
      terminalId,
      containerW: container.offsetWidth,
      containerH: container.offsetHeight,
    });

    const term = new Terminal({
      theme: theme === 'dark' ? DARK_THEME : LIGHT_THEME,
      fontFamily: '"SF Mono", ui-monospace, "Menlo", monospace',
      fontSize: 12.5,
      convertEol: true,
      allowProposedApi: true,
      cursorBlink: true,
    });
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

    const send = (msg: unknown) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(msg));
      }
    };

    // Liveness detection is owned server-side (ws/terminal.rs: 10s ping,
    // 30s pong_timeout — closes with 1011 on timeout). The browser's
    // WS impl handles TCP-level death itself and fires onclose/onerror.
    // We previously kept a 40s client-side timer here too, but it only
    // observed JS-level `onmessage` (Text/Binary), NOT browser auto-pongs
    // — so a healthy WS attached to an idle codex prompt (no PTY output
    // for 40s) would false-positive close as code 1006. Server-side
    // heartbeat already covers the real failure modes; the client-side
    // timer was redundant and harmful.

    ws.onopen = () => {
      setStatus('open');
      send({ Attach: { cols: term.cols, rows: term.rows } });
    };
    ws.onmessage = (e) => {
      let msg: Record<string, unknown>;
      try {
        msg = JSON.parse(typeof e.data === 'string' ? e.data : '');
      } catch {
        return;
      }
      // DaemonMsg variants from neige-session: `Hello`, `Stdout`,
      // `HelloChat`, `ChatEvent`, `ChildExited`. Only the terminal-mode
      // ones matter here.
      if ('Hello' in msg) {
        const replay = (msg.Hello as { replay: number[] }).replay;
        term.write(Uint8Array.from(replay));
        return;
      }
      if ('Stdout' in msg) {
        const bytes = msg.Stdout as number[];
        term.write(Uint8Array.from(bytes));
        return;
      }
      if ('ChildExited' in msg) {
        const code = (msg.ChildExited as { code: number | null }).code;
        term.writeln(
          `\r\n\x1b[2m[process exited${code != null ? ` (code ${code})` : ''}]\x1b[0m`,
        );
        ws.close();
        return;
      }
      // Hello/Chat events: ignored — calm-server doesn't use chat mode for
      // built-in terminal cards.
    };
    ws.onclose = (e) => {
      // Capture close code/reason so the gray "disconnected" overlay can
      // tell us what kind of disconnect this was without devtools.
      // 1006 = abnormal closure (network / proxy cut, no Close frame).
      // 1011 = server-side heartbeat trip (see ws/terminal.rs PONG_TIMEOUT).
      // 1001 = endpoint going away (server restart, page navigation).
      setCloseInfo({ code: e.code, reason: e.reason || '' });
      dlog('XtermView', 'WS close', { code: e.code, reason: e.reason, wasClean: e.wasClean });
      setStatus('closed');
    };
    ws.onerror = (e) => {
      dlog('XtermView', 'WS error', e);
      setStatus('closed');
    };

    const dataSub = term.onData((d) => {
      const bytes = Array.from(new TextEncoder().encode(d));
      send({ Stdin: bytes });
    });

    // Batch resize work to one tick per animation frame and skip cases
    // where fit() didn't actually change the grid. RGL's resize handle
    // fires the ResizeObserver on every mousemove; without the rAF guard
    // the terminal re-fits and re-renders constantly, which shows up as a
    // 1-2px shake on the inner canvas.
    let pending = false;
    let lastCols = term.cols;
    let lastRows = term.rows;
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
          lastCols = term.cols;
          lastRows = term.rows;
          send({ Resize: { cols: term.cols, rows: term.rows } });
        }
      });
    };
    const ro = new ResizeObserver(onResize);
    ro.observe(container);

    return () => {
      ro.disconnect();
      dataSub.dispose();
      try {
        ws.close();
      } catch {
        /* already closed */
      }
      term.dispose();
    };
  }, [terminalId, theme, reconnectKey]);

  return (
    <div className="xterm-view">
      <div ref={containerRef} className="xterm-container" />
      {status === 'connecting' && (
        <div className="xterm-status">connecting…</div>
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
