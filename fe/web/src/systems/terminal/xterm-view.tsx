// Terminal protocol, rendering and connection lifecycle. Reconnect replaces only the WebSocket.
import { forwardRef, useEffect, useImperativeHandle, useRef } from 'react';
import { Terminal, type ITheme } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { useState } from '../../ui/state/public.ts';
import type { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { dlog } from './debug.ts';
import { makeUuid } from './uuid.ts';
import { MONO_STACK } from './font-stack.ts';
import {
  createXtermWheelTarget,
  type XtermWheelTarget,
} from './xterm-adapter.ts';
import {
  copyTextToClipboard,
  createOsc52Handler,
  osc52HostMayWrite,
} from './osc52.ts';
import type {
  ClientMsg,
  DaemonMsg,
  ProtocolErrorCode,
  Role,
} from './generated-terminal.ts';
import { LIGHT_THEME_RGB, DARK_THEME_RGB } from './theme-rgb.ts';

// Cool-neutral light xterm theme matching Calm's palette.
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

/** Child exit info from the daemon; `exit_code` and `signal_killed` are mutually exclusive at the source. `null` clears a prior badge. */
export interface ExitChange {
  exit_code: number | null;
  signal_killed: boolean;
}

interface XtermViewProps {
  recovery?: RecoveryAccess | null;
  terminalId: string;
  theme?: 'light' | 'dark';
  /** The daemon-assigned role, or `null` on reconnect/disconnect so the parent can clear a badge. */
  onRoleChange?: (role: Role | null) => void;
  /** Child-exit info lifted to the parent; idempotent — the same payload may arrive twice (JSON frame, then WS close). `null` on reconnect. */
  onExitChange?: (exit: ExitChange | null) => void;
  onStatusChange?: (status: TerminalConnectionStatus) => void;
  /** The view stays mounted after first open; hidden cards must not ResizeCommit or accept OSC 52. */
  visible?: boolean;
}

export interface XtermViewHandle {
  refresh(): void;
  getWheelTarget(): XtermWheelTarget | null;
}

/** Last close info, surfaced in the disconnected overlay. */
interface CloseInfo {
  code: number;
  reason: string;
}

/** Must match `crates/calm-session/src/lib.rs::PROTOCOL_VERSION`. */
const PROTOCOL_VERSION = 4;

// Four-plus rows/cols worth of host surface avoids xterm/FitAddon
// bootstrapping against a collapsed card body.
const MIN_MOUNT_WIDTH_PX = 80;
const MIN_MOUNT_HEIGHT_PX = 24;
// Below 8x4 is not a usable PTY viewport; suppress owner-claim commits
// from transient or failed fits so they cannot poison the shared model.
const MIN_COMMIT_COLS = 8;
const MIN_COMMIT_ROWS = 4;

function isNonDegenerateMountSize(width: number, height: number): boolean {
  return width >= MIN_MOUNT_WIDTH_PX && height >= MIN_MOUNT_HEIGHT_PX;
}

/** UI status for the terminal protocol. */
export type TerminalConnectionStatus =
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

/** Bridge to calm-server's `/api/terminals/:id` WS endpoint; frames are JSON `ClientMsg`/`DaemonMsg` from `calm-session`, `Vec<u8>` riding as `Array<number>`. */
export const XtermView = forwardRef<XtermViewHandle, XtermViewProps>(function XtermView({
  terminalId,
  recovery = null,
  theme = 'light',
  onRoleChange,
  onExitChange,
  onStatusChange,
  visible = true,
}, ref) {
  // Playwright instrumentation, gated on `?testMounts=1`: counts real mounts in `window.__xtermMounts__`.
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

  const rootRef = useRef<HTMLDivElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  // Live ref so the theme effect can re-theme without tearing down the WebSocket.
  const termRef = useRef<Terminal | null>(null);
  // The bridge-mount effect omits `theme` from its deps on purpose; it reads the current value here.
  const latestThemeRef = useRef<'light' | 'dark'>(theme);
  latestThemeRef.current = theme;
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const flushResizeRef = useRef<(() => void) | null>(null);
  useEffect(() => {
    if (visible) flushResizeRef.current?.();
  }, [visible]);
  const [status, setStatus] = useState<TerminalConnectionStatus>('connecting');
  const onStatusChangeRef = useRef(onStatusChange);
  onStatusChangeRef.current = onStatusChange;
  useEffect(() => { onStatusChangeRef.current?.(status); }, [status]);
  const [closeInfo, setCloseInfo] = useState<CloseInfo | null>(null);
  const [protocolError, setProtocolError] = useState<ProtocolError | null>(null);
  const [exitInfo, setExitInfo] = useState<ExitInfo | null>(null);
  void exitInfo;
  // Synchronous mirror of `exitInfo` so the `ws.onclose` backstop can skip when a `TerminalExited` frame already delivered the code.
  const exitInfoRef = useRef<ExitInfo | null>(null);
  // Captured in a ref so a callback identity flip from the parent does not tear down the WebSocket.
  const onRoleChangeRef = useRef<XtermViewProps['onRoleChange']>(onRoleChange);
  onRoleChangeRef.current = onRoleChange;
  // Matches the hardcoded `role_hint: 'Owner'` in ClientHello.
  const wantedOwnerRef = useRef<boolean>(true);
  const onExitChangeRef = useRef<XtermViewProps['onExitChange']>(onExitChange);
  onExitChangeRef.current = onExitChange;
  // Reconnect only the browser transport. The mounted xterm and its output
  // stay available until a successful attach supplies an authoritative replay.
  const reconnectRef = useRef<(() => void) | null>(null);
  useImperativeHandle(
    ref,
    () => ({
      refresh: () => reconnectRef.current?.(),
      getWheelTarget: () => {
        if (!rootRef.current) {
          return null;
        }
        return createXtermWheelTarget({
          root: rootRef.current,
          terminalRef: termRef,
        });
      },
    }),
    [],
  );
  const [layoutRetryKey, setLayoutRetryKey] = useState(0);
  const [geometryDeferred, setGeometryDeferred] = useState(false);
  const lastFailedMountSizeRef = useRef<{ w: number; h: number } | null>(null);

  // Live `send` from the WS-mount effect so the theme effect can post without owning the socket.
  const sendRef = useRef<((msg: ClientMsg) => void) | null>(null);
  // A `TerminalThemeUpdate` produced before the WS effect installed `sendRef`; drained there.
  const pendingThemeRef = useRef<ClientMsg | null>(null);

  // Live-apply theme without rebuilding the Terminal + WS. The `TerminalThemeUpdate` dispatch is deliberately unconditional: a remount resets any per-component bookkeeping, and suppression lives on the daemon side.
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
      pendingThemeRef.current = msg;
    }
  }, [theme]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const deferUntilUsableGeometry = () => {
      setGeometryDeferred(true);
      const ro = new ResizeObserver(() => {
        const nextWidth = container.offsetWidth;
        const nextHeight = container.offsetHeight;
        const lastFailedMountSize = lastFailedMountSizeRef.current;
        if (
          lastFailedMountSize &&
          nextWidth === lastFailedMountSize.w &&
          nextHeight === lastFailedMountSize.h
        ) {
          return;
        }
        if (!isNonDegenerateMountSize(nextWidth, nextHeight)) return;
        ro.disconnect();
        setLayoutRetryKey((k) => k + 1);
      });
      ro.observe(container);
      return () => {
        ro.disconnect();
      };
    };
    const removeTestDumpHook = () => {
      if (typeof window === 'undefined') return;
      const w = window as unknown as {
        __xtermDumps__?: Record<string, () => string>;
      };
      if (w.__xtermDumps__) delete w.__xtermDumps__[terminalId];
    };
    const mountWidth = container.offsetWidth;
    const mountHeight = container.offsetHeight;
    if (!isNonDegenerateMountSize(mountWidth, mountHeight)) {
      dlog('XtermView', 'mount DEFERRED tiny container', {
        w: mountWidth,
        h: mountHeight,
      });
      lastFailedMountSizeRef.current = { w: mountWidth, h: mountHeight };
      return deferUntilUsableGeometry();
    }
    setGeometryDeferred(false);
    dlog('XtermView', 'mount START', {
      terminalId,
      containerW: mountWidth,
      containerH: mountHeight,
    });

    const term = new Terminal({
      theme:
        latestThemeRef.current === 'dark' ? DARK_THEME : LIGHT_THEME,
      fontFamily: MONO_STACK,
      fontSize: 12.5,
      // Mirrors `SCROLLBACK_MAX_LINES` in `crates/calm-server/src/terminal_renderer/mod.rs`; keep in lockstep.
      scrollback: 2000,
      convertEol: true,
      allowProposedApi: true,
      cursorBlink: true,
      disableStdin: true,
    });
    termRef.current = term;
    // OSC-echo e2e instrumentation, gated on `?testMounts=1`: a per-terminal buffer serializer, read from the buffer because the canvas renderer mirrors no glyphs into the DOM.
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
    // Suppress xterm.js's OSC 10/11/12 auto-reply: the daemon is the sole responder, and xterm's transparent `clearColor` would race back as pure white.
    term.parser.registerOscHandler(10, () => true);
    term.parser.registerOscHandler(11, () => true);
    term.parser.registerOscHandler(12, () => true);
    term.parser.registerOscHandler(
      52,
      createOsc52Handler(copyTextToClipboard, () =>
        osc52HostMayWrite(container, visibleRef.current),
      ),
    );
    // VS Code's default Mac sendSequence keybindings (Cmd+Left/Right/Backspace → ^A/^E/^U); pure-Cmd only.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown') return true;
      if (e.isComposing) return true;
      // xterm.js sends CR for both Enter and Shift+Enter. This host maps
      // Shift+Enter to ESC CR (the chord hosted TUIs use for a newline).
      if (
        e.key === 'Enter' &&
        e.shiftKey &&
        !e.ctrlKey &&
        !e.metaKey &&
        !e.altKey
      ) {
        term.input('\x1b\r', true);
        e.preventDefault();
        return false;
      }
      if (!e.metaKey || e.ctrlKey || e.altKey) return true;
      if (e.key === 'ArrowLeft') {
        term.input('\x01', true);
        e.preventDefault();
        return false;
      }
      if (e.key === 'ArrowRight') {
        term.input('\x05', true);
        e.preventDefault();
        return false;
      }
      if (e.key === 'Backspace') {
        term.input('\x15', true);
        e.preventDefault();
        return false;
      }
      return true;
    });
    // xterm's helper textarea captures every Tab once focused, making the terminal a focus trap during page Tab navigation; demote it out of the Tab order (clicking still focuses it).
    const helperTextarea = container.querySelector<HTMLTextAreaElement>(
      '.xterm-container textarea.xterm-helper-textarea',
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
    if (term.cols < MIN_COMMIT_COLS || term.rows < MIN_COMMIT_ROWS) {
      dlog('XtermView', 'mount DEFERRED post-fit degenerate', {
        cols: term.cols,
        rows: term.rows,
      });
      lastFailedMountSizeRef.current = {
        w: container.offsetWidth,
        h: container.offsetHeight,
      };
      term.dispose();
      removeTestDumpHook();
      if (termRef.current === term) termRef.current = null;
      return deferUntilUsableGeometry();
    }
    lastFailedMountSizeRef.current = null;

    let automaticAllowed = true;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    let retryDelay = 500;
    const permitted = () => recovery !== null ? recovery.read().phase === 'connected' : !__NC_BUNDLED__;
    const connect = () => {
      setStatus(permitted() ? 'connecting' : 'closed');
      setCloseInfo(null);
      setProtocolError(null);
      setExitInfo(null);
      exitInfoRef.current = null;
      onExitChangeRef.current?.(null);
      term.options.disableStdin = true;
      if (!permitted()) return () => {};
      const generation = recovery?.read().generation;
      let live = true;
      const current = () => live && permitted() && generation === recovery?.read().generation;
      let connectionReady = false;
      let awaitingOwner = false;
      const wsUrl = `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${
        location.host
      }/api/terminals/${encodeURIComponent(terminalId)}`;
      const ws = new WebSocket(wsUrl);
      let handshakeTimedOut = false;
      const handshakeTimer = __NC_BUNDLED__ ? setTimeout(() => {
        if (current()) { handshakeTimedOut = true; ws.close(); }
      }, 15_000) : null;

      // Frames produced between `new WebSocket(…)` and `ws.onopen` are queued and flushed after the ClientHello.
      const pendingFrames: ClientMsg[] = [];
      const send = (msg: ClientMsg) => {
        if (!current()) { pendingFrames.length = 0; return; }
        if (ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify(msg));
        } else if (ws.readyState === WebSocket.CONNECTING && typeof msg === 'object' && 'TerminalThemeUpdate' in msg) {
          pendingFrames.push(msg);
        }
      };
      sendRef.current = send;
      // Drain a `TerminalThemeUpdate` buffered before this effect ran.
      if (pendingThemeRef.current) {
        send(pendingThemeRef.current);
        pendingThemeRef.current = null;
      }

      // Per-connection client id; the daemon's `OwnerRegistry` keys on it so a tab survives WS reconnects without losing ownership.
      const clientId = makeUuid();
      // Monotonic resize epoch so a `ResizeApplied` echo can be matched and stale applies ignored.
      let resizeEpoch = 0;
      // Kept separate from `term.cols/rows`: ServerHello may resize the local xterm before first-attach sync is decided.
      const mountDesired = { cols: term.cols, rows: term.rows };
      let lastCols = term.cols;
      let lastRows = term.rows;
      let renderRev = 0;
      let ptySeq = 0;

      // Liveness detection is server-side (10s ping / 30s pong timeout → 1011); a client-side timer cannot see browser auto-pongs and false-positives on an idle PTY.

      ws.onopen = () => {
        if (!current()) { ws.close(); return; }
        setStatus('handshaking');
        send({
          ClientHello: {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminalId,
            client_id: clientId,
            desired_size: {
              cols: mountDesired.cols,
              rows: mountDesired.rows,
              pixel_width: null,
              pixel_height: null,
            },
            cell_size: null,
            // 'All' restores daemon-retained scrollback on remount; bounded by the server's SCROLLBACK_MAX_LINES.
            initial_scrollback: 'All',
            resume_from: null,
            // The daemon may still hand us Observer if another client owns the session.
            role_hint: 'Owner',
            capabilities: {
              render_encodings: ['Vt'],
              supports_scrollback: true,
              supports_sixel: false,
              supports_images: false,
              // The WS bridge force-strips this to false for browser ingress regardless; declared false to match.
              kernel_originated_input: false,
            },
          },
        });
        // Flush frames queued before the handshake; we are OPEN inside `onopen`.
        while (pendingFrames.length > 0) {
          const queued = pendingFrames.shift()!;
          send(queued);
        }
      };

      ws.onmessage = (e) => {
        if (!current()) return;
        let msg: DaemonMsg;
        try {
          msg = JSON.parse(typeof e.data === 'string' ? e.data : '') as DaemonMsg;
        } catch {
          return;
        }
        if ('ProtocolError' in msg || 'TerminalExited' in msg) automaticAllowed = false;
        if ('ServerHello' in msg) {
          if (handshakeTimer !== null) clearTimeout(handshakeTimer);
          const sh = msg.ServerHello;
          onRoleChangeRef.current?.(sh.client_role);
          setStatus('connected');
          awaitingOwner = false;
          connectionReady = true;
          retryDelay = 500;
          term.options.disableStdin = false;
          // A full replay replaces the retained view; do not append it twice.
          term.reset();
          if (wantedOwnerRef.current && sh.client_role === 'Observer') {
            // Previous owner's pump may not have released yet; claim eagerly
            // instead of waiting for the first owner-gated frame to fail.
            send('OwnerClaim');
          }
          // Resize the local terminal to the snapshot before writing the replay so the cursor lines up.
          if (sh.snapshot.cols !== term.cols || sh.snapshot.rows !== term.rows) {
            term.resize(sh.snapshot.cols, sh.snapshot.rows);
            lastCols = sh.snapshot.cols;
            lastRows = sh.snapshot.rows;
          }
          if (sh.snapshot.scrollback) {
            term.write(Uint8Array.from(sh.snapshot.scrollback));
            // snapshot.data leads with ED 2 (`\x1b[2J`); flush the viewport into the scrollback ring first or it erases the replayed tail.
            term.write('\r\n'.repeat(term.rows));
          }
          term.write(Uint8Array.from(sh.snapshot.data), () => {
            if (current() && termRef.current === term) term.scrollToBottom();
          });
          // A pure expansion cannot clip the recovery model; a shrink or mixed-axis change can destroy history and must wait for stable ResizeObserver intent.
          const mountIsPureExpansion =
            mountDesired.cols >= sh.pty_size.cols &&
            mountDesired.rows >= sh.pty_size.rows &&
            (mountDesired.cols > sh.pty_size.cols ||
              mountDesired.rows > sh.pty_size.rows);
          if (sh.client_role === 'Owner' && mountIsPureExpansion) {
            resizeEpoch += 1;
            send({
              ResizeCommit: {
                epoch: resizeEpoch,
                cols: mountDesired.cols,
                rows: mountDesired.rows,
              },
            });
          }
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
          // Standalone snapshot: the daemon decided we need a hard re-sync.
          const s = msg.RenderSnapshot;
          if (s.cols !== term.cols || s.rows !== term.rows) {
            term.resize(s.cols, s.rows);
            lastCols = s.cols;
            lastRows = s.rows;
          }
          if (s.scrollback) {
            term.clear();
            // `clear()` keeps the cursor column; restored history must start at column 0.
            term.write('\x1b[H');
            term.write(Uint8Array.from(s.scrollback));
            // Same ED 2 erasure guard as ServerHello.
            term.write('\r\n'.repeat(term.rows));
          }
          term.write(Uint8Array.from(s.data), () => {
            if (current() && termRef.current === term) term.scrollToBottom();
          });
          renderRev = s.render_rev;
          ptySeq = s.pty_seq;
          return;
        }
        if ('ResizeApplied' in msg) {
          const r = msg.ResizeApplied;
          // Stale-epoch guard: an out-of-order `ResizeApplied` must not clobber newer local geometry.
          if (r.epoch < resizeEpoch) return;
          lastCols = r.cols;
          lastRows = r.rows;
          renderRev = r.render_rev;
          ptySeq = r.pty_seq;
          return;
        }
        if ('SnapshotRequired' in msg) {
          term.clear();
          return;
        }
        if ('TerminalExited' in msg) {
          const t = msg.TerminalExited;
          exitInfoRef.current = { code: t.code };
          setExitInfo({ code: t.code });
          setStatus('exited');
          awaitingOwner = false;
          connectionReady = false;
          term.options.disableStdin = true;
          // The JSON frame carries no signal flag (`code` is 128+sig for a signal-killed child); the reliable `signal_killed` arrives via the REST seed. Idempotent against the duplicate `onclose` fire.
          onExitChangeRef.current?.({
            exit_code: t.code,
            signal_killed: false,
          });
          term.writeln(
            `\r\n\x1b[2m[process exited${
              t.code != null ? ` (code ${t.code})` : ''
            }]\x1b[0m`,
          );
          return;
        }
        if ('ProtocolError' in msg) {
          // NotOwner rejects an operation without closing the established
          // connection. Only an owner acknowledgement may restore its input.
          awaitingOwner = msg.ProtocolError.code === 'NotOwner'
            && (connectionReady || awaitingOwner) && ws.readyState === WebSocket.OPEN;
          setProtocolError({
            code: msg.ProtocolError.code,
            message: msg.ProtocolError.message,
          });
          setStatus('protocol-error');
          connectionReady = false;
          term.options.disableStdin = true;
          return;
        }
        if ('OwnerChanged' in msg) {
          // Late ownership events cannot recover fatal errors, exits or a
          // closed transport, even when they name this client.
          if ((!connectionReady && !awaitingOwner) || ws.readyState !== WebSocket.OPEN) return;
          const { owner_client_id: newOwnerId } = msg.OwnerChanged;
          dlog('XtermView', 'OwnerChanged', msg.OwnerChanged);
          if (newOwnerId === null) {
            if (wantedOwnerRef.current) {
              send('OwnerClaim');
            }
            return;
          }
          if (newOwnerId === clientId) {
            onRoleChangeRef.current?.('Owner');
            if (awaitingOwner) {
              awaitingOwner = false;
              connectionReady = true;
              automaticAllowed = true;
              retryDelay = 500;
              term.options.disableStdin = false;
              setStatus('connected');
              setProtocolError(null);
            }
            // A ResizeCommit sent while Observer may have been rejected; resend the fitted geometry on ownership transfer.
            if (term.cols >= MIN_COMMIT_COLS && term.rows >= MIN_COMMIT_ROWS) {
              resizeEpoch += 1;
              send({
                ResizeCommit: {
                  epoch: resizeEpoch,
                  cols: term.cols,
                  rows: term.rows,
                },
              });
            } else {
              dlog(
                'XtermView',
                'OwnerChanged ResizeCommit suppressed: degenerate geometry',
                {
                  cols: term.cols,
                  rows: term.rows,
                },
              );
            }
          } else {
            onRoleChangeRef.current?.('Observer');
          }
          return;
        }
        if ('Backpressure' in msg) {
          // The daemon never emits this yet; logged for debugging.
          dlog('XtermView', 'Backpressure', msg.Backpressure);
          return;
        }
      };

      ws.onclose = (e) => {
        if (handshakeTimer !== null) clearTimeout(handshakeTimer);
        const wasAwaitingOwner = awaitingOwner;
        awaitingOwner = false;
        connectionReady = false;
        term.options.disableStdin = true;
        // 1000 + `child-exited` = daemon's clean child-exit close (mapped to `exited` even if the `TerminalExited` frame was dropped); 1006 abnormal; 1011 server heartbeat trip; 1001 going away.
        setCloseInfo({ code: e.code, reason: e.reason || '' });
        dlog('XtermView', 'WS close', {
          code: e.code,
          reason: e.reason,
          wasClean: e.wasClean,
        });
        const isChildExitClose =
          e.code === 1000 && e.reason === 'child-exited';
        const transientClose = [1001, 1006, 1011].includes(e.code)
          || (handshakeTimedOut && [1000, 1005].includes(e.code));
        if (isChildExitClose || !transientClose) automaticAllowed = false;
        if (__NC_BUNDLED__ && automaticAllowed && current() && retryTimer === null) {
          retryTimer = setTimeout(() => { retryTimer = null; if (automaticAllowed && permitted()) reconnect(); }, retryDelay * (0.75 + Math.random() * 0.5));
          retryDelay = Math.min(8000, retryDelay * 2);
        }
        // Don't clobber a more-specific state (`exited`, `protocol-error`); a `child-exited` close promotes to `exited`.
        setStatus((prev) => {
          if (prev === 'exited' || (prev === 'protocol-error' && !wasAwaitingOwner)) return prev;
          if (isChildExitClose) return 'exited';
          return 'closed';
        });
        // Backstop for the parent's exit badge, only when no `TerminalExited` frame delivered a code: the parent's callback is a plain setState and would be clobbered back to `null`.
        if (isChildExitClose && exitInfoRef.current === null) {
          onExitChangeRef.current?.({
            exit_code: null,
            signal_killed: false,
          });
        }
        // Role is undefined once the WS is gone — parent clears any pill.
        onRoleChangeRef.current?.(null);
      };
      ws.onerror = (e) => {
        const wasAwaitingOwner = awaitingOwner;
        awaitingOwner = false;
        connectionReady = false;
        term.options.disableStdin = true;
        dlog('XtermView', 'WS error', e);
        setStatus((prev) =>
          prev === 'exited' || (prev === 'protocol-error' && !wasAwaitingOwner) ? prev : 'closed',
        );
        onRoleChangeRef.current?.(null);
      };

      const dataSub = term.onData((d) => {
        if (!connectionReady) return;
        const bytes = Array.from(new TextEncoder().encode(d));
        // `input_seq: 0` means no ack requested; only kernel-originated transient clients use non-zero seqs.
        send({ Input: { data: bytes, input_seq: 0 } });
      });

      // One fit per animation frame: RGL's resize handle fires the ResizeObserver on every mousemove, which shows as a 1-2px shake.
      let pending = false;
      let resizeFrame: number | null = null;
      let sawInitialResizeObservation = false;
      const onResize = () => {
        // ResizeObserver always delivers an initial observation; treating it as user intent would narrow the PTY that ServerHello just restored.
        if (!sawInitialResizeObservation) {
          sawInitialResizeObservation = true;
          return;
        }
        if (pending) return;
        pending = true;
        resizeFrame = requestAnimationFrame(() => {
          resizeFrame = null;
          pending = false;
          if (!connectionReady) return;
          if (!visibleRef.current) return;
          // fit() mutates the local xterm in place, so a collapsed container would leave the buffer at e.g. 2x1 interpreting RenderPatch bytes at the wrong geometry.
          if (
            !isNonDegenerateMountSize(container.offsetWidth, container.offsetHeight)
          ) {
            dlog('XtermView', 'resize → skip fit (container degenerate)', {
              containerW: container.offsetWidth,
              containerH: container.offsetHeight,
              lastCols,
              lastRows,
            });
            return;
          }
          try {
            fit.fit();
          } catch {
            return;
          }
          // fit() can still land on a marginal grid; restore last-known-good so RenderPatch bytes don't render against it.
          if (term.cols < MIN_COMMIT_COLS || term.rows < MIN_COMMIT_ROWS) {
            dlog('XtermView', 'resize → fit DEGENERATE — restore last good', {
              cols: term.cols,
              rows: term.rows,
              lastCols,
              lastRows,
            });
            if (term.cols !== lastCols || term.rows !== lastRows) {
              term.resize(lastCols, lastRows);
            }
            return;
          }
          if (term.cols !== lastCols || term.rows !== lastRows) {
            dlog('XtermView', 'resize → fit', {
              from: { cols: lastCols, rows: lastRows },
              to: { cols: term.cols, rows: term.rows },
              containerW: container.offsetWidth,
              containerH: container.offsetHeight,
            });
            // `lastCols/Rows` stay at their previous value until `ResizeApplied` confirms, else a coalesce could swallow a resize back to the same size.
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
      flushResizeRef.current = onResize;
      const ro = new ResizeObserver(onResize);
      ro.observe(container);

      void renderRev;
      void ptySeq;

      return () => {
        live = false;
        if (handshakeTimer !== null) clearTimeout(handshakeTimer);
        if (flushResizeRef.current === onResize) flushResizeRef.current = null;
        ro.disconnect();
        if (resizeFrame !== null) cancelAnimationFrame(resizeFrame);
        dataSub.dispose();
        ws.onopen = null;
        ws.onmessage = null;
        ws.onclose = null;
        ws.onerror = null;
        try {
          ws.close();
        } catch {
          /* already closed */
        }
        // Strict-mode double-invoke: a teardown that runs after the next mount installed its own `send` must not null it out.
        if (sendRef.current === send) {
          sendRef.current = null;
        }
        // Sync here rather than via onclose so a strict-mode unmount or `terminalId` change clears the parent even with no close frame.
        onRoleChangeRef.current?.(null);
        // Revoking the transport does not revoke authoritative exit/error facts.
        // A new attach resets them in connect(); disposal clears the parent below.
      };
    };
    let disconnect = connect();
    const reconnect = () => {
      if (!permitted()) return;
      disconnect(); disconnect = connect();
    };
    let wasPermitted = permitted();
    const unsubscribeRecovery = recovery?.subscribe(() => {
      const now = permitted();
      if (!now) {
        if (retryTimer !== null) clearTimeout(retryTimer);
        retryTimer = null; disconnect(); disconnect = () => {};
        term.options.disableStdin = true;
        setStatus(previous => previous === 'exited' || previous === 'protocol-error' ? previous : 'closed');
      } else if (!wasPermitted && automaticAllowed) reconnect();
      wasPermitted = now;
    });
    const manualReconnect = () => {
      if (!permitted()) return;
      automaticAllowed = true; retryDelay = 500;
      if (retryTimer !== null) clearTimeout(retryTimer);
      retryTimer = null; reconnect();
    };
    reconnectRef.current = manualReconnect;
    return () => {
      if (reconnectRef.current === manualReconnect) reconnectRef.current = null;
      unsubscribeRecovery?.();
      if (retryTimer !== null) clearTimeout(retryTimer);
      disconnect();
      exitInfoRef.current = null;
      onExitChangeRef.current?.(null);
      term.dispose();
      removeTestDumpHook();
      if (termRef.current === term) termRef.current = null;
    };
    // Theme changes and connection status must never tear down the buffer.
  }, [terminalId, layoutRetryKey, recovery]);

  return (
    <div ref={rootRef} className="xterm-view" data-nc-terminal-id={terminalId}>
      {/* xterm's per-cell spans are presentational, not navigable text; `aria-hidden` scopes axe and AT to the helper textarea, which carries its own ARIA wiring. */}
      <div
        ref={containerRef}
        className="xterm-container"
        aria-hidden="true"
        role="presentation"
      />
      {/* While waiting for usable layout, keep only a neutral loading state;
       *  the terminal bridge has not opened a WebSocket yet. */}
      {geometryDeferred && (
        <div className="xterm-status">Loading terminal…</div>
      )}
      {!geometryDeferred && status === 'connecting' && (
        <div className="xterm-status">connecting…</div>
      )}
      {!geometryDeferred && status === 'handshaking' && (
        <div className="xterm-status">handshaking…</div>
      )}
      {status === 'closed' && (
        <ErrorBox
          message="Connection lost."
          description="Reconnect to the same terminal."
          actionLabel="Reconnect"
          onRetry={() => reconnectRef.current?.()}
          details={closeInfo === null ? undefined : `WebSocket ${closeInfo.code}${closeInfo.reason ? `: ${closeInfo.reason}` : ''}`}
          floating
        />
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
              ? ' (refresh required for protocol v4)'
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
    </div>
  );
});
