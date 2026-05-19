import { useEffect, useRef, useState } from 'react';
import { Terminal, type ITheme } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

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
  // Bumping this re-runs the WS effect, which rebuilds the terminal +
  // reconnects. Cheap manual respawn affordance for the user when the
  // shell exits or the daemon dies — the kernel auto-revives dead
  // daemons on attach, so a reconnect is usually enough.
  const [reconnectKey, setReconnectKey] = useState(0);
  const restart = () => {
    setStatus('connecting');
    setReconnectKey((k) => k + 1);
  };

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

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
    } catch {
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

    // Heartbeat liveness detection. The server pings every 10s and closes
    // after 30s of silence. The browser auto-pongs at the protocol layer
    // (we don't see it from JS), and pong-received events aren't surfaced
    // to JS either — so the only signal we can use is "did we see ANY
    // message lately." If nothing arrives for 40s (server's 30s window +
    // headroom), the underlying daemon or network is gone; mark closed and
    // tear down. We deliberately do NOT auto-reconnect — terminals carry
    // state, so the user reconnects via the Restart button (which also
    // triggers the kernel's daemon-respawn path).
    const DEAD_AFTER_MS = 40_000;
    let liveness: ReturnType<typeof setTimeout> | null = null;
    const bumpLiveness = () => {
      if (liveness) clearTimeout(liveness);
      liveness = setTimeout(() => {
        if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
          setStatus('closed');
          try {
            ws.close();
          } catch {
            /* already closed */
          }
        }
      }, DEAD_AFTER_MS);
    };

    ws.onopen = () => {
      setStatus('open');
      send({ Attach: { cols: term.cols, rows: term.rows } });
      bumpLiveness();
    };
    ws.onmessage = (e) => {
      bumpLiveness();
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
    ws.onclose = () => {
      if (liveness) {
        clearTimeout(liveness);
        liveness = null;
      }
      setStatus('closed');
    };
    ws.onerror = () => setStatus('closed');

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
          lastCols = term.cols;
          lastRows = term.rows;
          send({ Resize: { cols: term.cols, rows: term.rows } });
        }
      });
    };
    const ro = new ResizeObserver(onResize);
    ro.observe(container);

    return () => {
      if (liveness) {
        clearTimeout(liveness);
        liveness = null;
      }
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
          <span>disconnected</span>
          <button onClick={restart} className="xterm-restart">
            Restart
          </button>
        </div>
      )}
    </div>
  );
}
