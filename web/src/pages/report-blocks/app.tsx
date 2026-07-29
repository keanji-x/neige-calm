// `app` report block — an embedded same-origin page inside a sandboxed
// iframe. This is the ONE block that keeps a border: the frame marks it as
// an outsider held in a sandbox, unlike charts/tables which are native
// document furniture.
//
// The iframe wiring mirrors `cards/plugin-iframe.tsx`: same sandbox
// attributes, and a best-effort AppBridge (MCP ext-apps) mounted against
// the frame so MCP-app content gets `ui/initialize` + live theme pushes.
// Differences from the card host, deliberate:
//   * `src` is a validated same-origin path from the block payload, not a
//     `ui://` plugin URI — there is no plugin_id, so `oncalltool` rejects
//     every call instead of routing to the kernel tool-call endpoint.
//   * Non-MCP pages never complete the handshake; `connect()` simply stays
//     pending until unmount and the plain page renders fine regardless.

import { useEffect, useMemo, useRef } from 'react';
import {
  AppBridge,
  PostMessageTransport,
  type McpUiHostCapabilities,
} from '@modelcontextprotocol/ext-apps/app-bridge';
import type { Implementation } from '@modelcontextprotocol/sdk/types.js';
import { useTheme } from '../../app/theme';
import type { AppBlockPayload } from '../../cards/builtins/wave-report';

const HOST_INFO: Implementation = {
  name: 'neige-calm',
  version: '0.1.0',
};

const HOST_CAPABILITIES: McpUiHostCapabilities = {
  logging: {},
};

const MIN_HEIGHT = 120;
const MAX_HEIGHT = 2000;
const DEFAULT_HEIGHT = 360;

function clampHeight(height: number | undefined): number {
  if (typeof height !== 'number' || Number.isNaN(height)) return DEFAULT_HEIGHT;
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height));
}

export function ReportAppBlock({ payload }: { payload: AppBlockPayload }) {
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const bridgeRef = useRef<AppBridge | null>(null);
  const { resolved: theme } = useTheme();
  const latestThemeRef = useRef<'light' | 'dark'>(theme);
  latestThemeRef.current = theme;

  const height = clampHeight(payload.height);
  const title = payload.title?.trim() ? payload.title : payload.src;

  // Defense in depth on top of the zod regex: resolve the src the way the
  // browser will and assert it stays on our origin. URL normalization (e.g.
  // `\` → `/`, or a smuggled scheme) that would escape same-origin renders
  // the placeholder instead of mounting an iframe.
  const sameOrigin = useMemo(() => {
    try {
      return (
        new URL(payload.src, window.location.origin).origin ===
        window.location.origin
      );
    } catch {
      return false;
    }
  }, [payload.src]);

  useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe) return;
    let cancelled = false;
    let bridge: AppBridge | null = null;
    let transport: PostMessageTransport | null = null;

    const setup = async () => {
      await new Promise<void>((resolve, reject) => {
        if (
          iframe.contentWindow &&
          iframe.contentDocument?.readyState === 'complete'
        ) {
          resolve();
          return;
        }
        const onLoad = () => {
          iframe.removeEventListener('load', onLoad);
          iframe.removeEventListener('error', onError);
          resolve();
        };
        const onError = () => {
          iframe.removeEventListener('load', onLoad);
          iframe.removeEventListener('error', onError);
          reject(new Error('report app iframe failed to load'));
        };
        iframe.addEventListener('load', onLoad);
        iframe.addEventListener('error', onError);
      });
      if (cancelled || !iframe.contentWindow) return;

      bridge = new AppBridge(null, HOST_INFO, HOST_CAPABILITIES, {
        hostContext: {
          theme: latestThemeRef.current,
          platform: 'web',
          availableDisplayModes: ['inline'],
          displayMode: 'inline',
        },
      });
      // Report app blocks have no plugin identity, so there is no kernel
      // tool-call route to forward to — reject every call in-protocol.
      bridge.oncalltool = async (params) => ({
        content: [
          {
            type: 'text',
            text: `tool "${params.name}" is not callable from a report app block`,
          },
        ],
        isError: true,
      });
      transport = new PostMessageTransport(
        iframe.contentWindow,
        iframe.contentWindow,
      );
      await bridge.connect(transport);
      if (!cancelled) {
        bridgeRef.current = bridge;
        try {
          bridge.setHostContext({ theme: latestThemeRef.current });
        } catch {
          // Post-connect theme re-push is best-effort.
        }
      }
    };

    // Best-effort: a non-MCP page (or a load error) must not take the block
    // down — the sandboxed iframe itself is the deliverable.
    setup().catch(() => {});

    return () => {
      cancelled = true;
      bridgeRef.current = null;
      void bridge?.close().catch(() => {});
      void transport?.close().catch(() => {});
    };
  }, [payload.src]);

  useEffect(() => {
    const bridge = bridgeRef.current;
    if (!bridge) return;
    try {
      bridge.setHostContext({ theme });
    } catch {
      // Theme push is best-effort; the block keeps rendering.
    }
  }, [theme]);

  if (!sameOrigin) {
    return (
      <div className="rb-unsupported" role="note">
        unsupported block kind app
      </div>
    );
  }

  return (
    <div className="rb-app">
      <div className="rb-appbar">
        <svg className="rb-appbar-lock" viewBox="0 0 24 24" aria-hidden="true">
          <rect x="5" y="11" width="14" height="9" rx="2" />
          <path d="M8 11V8a4 4 0 0 1 8 0v3" />
        </svg>
        <span className="rb-appbar-title">{title}</span>
        <span className="rb-appbar-badge">SANDBOXED</span>
      </div>
      <iframe
        ref={iframeRef}
        className="rb-app-frame"
        title={title}
        src={payload.src}
        sandbox="allow-scripts allow-same-origin"
        style={{ height }}
      />
    </div>
  );
}
