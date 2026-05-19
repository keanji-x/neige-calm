// PluginIframeCard — the host-side renderer for kernel cards whose kind is
// a plugin view URI (`ui://<plugin_id>/<view_id>`).
//
// M5 (m3-mcp-apps full integration): we mount the
// `@modelcontextprotocol/ext-apps` AppBridge instead of rendering a
// placeholder. The AppBridge owns the iframe ↔ host postMessage channel
// (JSON-RPC 2.0 with `ui/*` and `tools/call` framing); we hand it a manual
// `oncalltool` handler that routes outbound iframe tool calls to the kernel
// via `POST /api/plugins/:id/tool-call`.
//
// Per migration doc §7.6 row 5, iframes can ONLY call `neige.*`
// kernel-namespace tools — the kernel route's `forbidden_tool` 403 enforces
// this server-side; we mirror the gate in `oncalltool` so AppBridge sees a
// clean MCP-style error instead of a network rejection.
//
// Per §3.2: AppBridge accepts `null` as its MCP client when the host wants
// to handle every request manually. We do that — Neige isn't proxying for a
// remote MCP server, the iframe's only writes are `neige.*` callbacks.
// `oncalltool` is the single entry point; the other forwarding hooks
// (`onreadresource`, `onlistresources`, …) stay default-empty so AppBridge
// returns `MethodNotFound` for them, which is the right answer.

import { useEffect, useRef, useState } from 'react';

import type { CardEntry } from './registry';
import type { PluginCardData } from '../types';
import type { KernelCard } from '../api/wire';
import {
  CalmApiError,
  toolCallFromIframe,
} from '../api/calm';
import {
  AppBridge,
  PostMessageTransport,
  type McpUiHostCapabilities,
} from '@modelcontextprotocol/ext-apps/app-bridge';
import type { Implementation } from '@modelcontextprotocol/sdk/types.js';

/**
 * Parse a `ui://` resource URI into the (plugin_id, view_id) pair.
 *
 * URI shape: scheme = `ui`, authority = plugin_id, path's first segment =
 * view_id (additional path segments are allowed per migration doc §7.6 row 1
 * for multi-asset views, but we only surface the first one as the view_id;
 * deeper segments stay folded into `resource_uri`).
 *
 * Returns `null` for anything that doesn't match — including the legacy
 * `plugin:<plugin>:<view>` form, which was deleted in M4. The registry's
 * `adaptKernelCard` falls through to the next entry when we return null.
 */
export function parsePluginCardKind(
  kind: string,
): { plugin_id: string; view_id: string; resource_uri: string } | null {
  if (!kind.startsWith('ui://')) return null;
  // ui://<authority>/<path>; authority is the plugin id by convention.
  // Strip the scheme then split on the first `/` to separate authority
  // from the (potentially multi-segment) view path.
  const rest = kind.slice('ui://'.length);
  const slash = rest.indexOf('/');
  if (slash <= 0 || slash === rest.length - 1) return null;
  const plugin_id = rest.slice(0, slash);
  const view_id = rest.slice(slash + 1);
  return { plugin_id, view_id, resource_uri: kind };
}

/** True when `kind` looks like a plugin card URI. Single source of truth for
 *  any URI-prefix dispatch — keep checks aligned with `parsePluginCardKind`. */
export function isPluginCardKind(kind: string): boolean {
  return kind.startsWith('ui://');
}

const HOST_INFO: Implementation = {
  name: 'neige-calm',
  version: '0.1.0',
};

const HOST_CAPABILITIES: McpUiHostCapabilities = {
  // We do proxy server tools — that's the whole point of `tool-call` — but
  // we don't expose every MCP-server feature. The AppBridge inspects this
  // object during the `ui/initialize` handshake to advertise host shape.
  serverTools: {},
  // The host can open external links via the user's default browser. Real
  // wiring lives below in `bridge.onopenlink`.
  openLinks: {},
  // Logging is cheap to surface in the console and useful for plugin
  // authors during development.
  logging: {},
};

function detectTheme(): 'light' | 'dark' {
  if (typeof window === 'undefined') return 'light';
  try {
    const mql = window.matchMedia('(prefers-color-scheme: dark)');
    return mql.matches ? 'dark' : 'light';
  } catch {
    return 'light';
  }
}

/**
 * The actual card body. Mounts an iframe pointing at the kernel's HTML
 * route, then wires an AppBridge to the iframe's content window once it
 * loads. Errors during mount surface as an in-card error state — the rest
 * of the wave keeps rendering.
 */
function PluginIframeCard({ card }: { card: PluginCardData }) {
  const parsed = parsePluginCardKind(card.resource_uri);
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!parsed) {
      setError(`malformed resource_uri: ${card.resource_uri}`);
      return;
    }
    const iframe = iframeRef.current;
    if (!iframe) return;

    // Closure-scoped flag so the async setup respects an unmount-before-
    // connect race. AppBridge.connect awaits the iframe's `ui/initialize`,
    // and React's strict-mode double-invoke can fire teardown before
    // connect resolves.
    let cancelled = false;
    let bridge: AppBridge | null = null;
    let transport: PostMessageTransport | null = null;

    const { plugin_id } = parsed;

    const setup = async () => {
      // Wait for the iframe to actually load the HTML body before we hand
      // its contentWindow to the transport — otherwise the transport
      // listens to a `null` source for the first round of frames.
      await new Promise<void>((resolve, reject) => {
        if (iframe.contentWindow && iframe.contentDocument?.readyState === 'complete') {
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
          reject(new Error('iframe failed to load plugin HTML'));
        };
        iframe.addEventListener('load', onLoad);
        iframe.addEventListener('error', onError);
      });
      if (cancelled) return;

      // §3.2 of the migration doc: pass `null` for the MCP client and
      // register an `oncalltool` that routes to the kernel REST endpoint.
      // `toolInfo` in the spec carries a `tool: Tool` definition (the spec
      // assumes a chat host where an LLM picked the tool). Neige doesn't
      // have a Tool object handy at mount time, so we stash the card id
      // under the host-context's open index signature instead — plugin
      // iframes that need it can read it as `hostContext.neige.cardId`.
      bridge = new AppBridge(null, HOST_INFO, HOST_CAPABILITIES, {
        hostContext: {
          theme: detectTheme(),
          platform: 'web',
          availableDisplayModes: ['inline'],
          displayMode: 'inline',
          neige: card.id ? { cardId: card.id } : undefined,
        },
      });

      // The iframe → kernel write path. §7.6 row 5: gate `neige.*` only.
      // The server gates this too (the route returns 403 `forbidden_tool`)
      // but mirroring it client-side avoids a wasted network round-trip
      // and gives the iframe a clean, in-protocol error frame.
      bridge.oncalltool = async (params) => {
        if (!params.name.startsWith('neige.')) {
          return {
            content: [
              {
                type: 'text',
                text: `tool "${params.name}" is not callable from a plugin iframe — only neige.* kernel tools are allowed`,
              },
            ],
            isError: true,
          };
        }
        try {
          const result = (await toolCallFromIframe(plugin_id, {
            name: params.name,
            arguments: (params.arguments ?? {}) as Record<string, unknown>,
          })) as Record<string, unknown> | null;
          // The kernel dispatcher returns the inner `neige.*` handler's
          // value (e.g. `{ ok: true }`). Surface it under `structuredContent`
          // so the iframe sees a spec-shaped CallToolResult.
          return {
            content: [],
            structuredContent: result ?? {},
          };
        } catch (err) {
          const message =
            err instanceof CalmApiError ? `${err.code}: ${err.message}` : String(err);
          return {
            content: [{ type: 'text', text: message }],
            isError: true,
          };
        }
      };

      // Forward iframe-initiated logging to the browser console — handy
      // for plugin development; cheap if no plugin uses it.
      bridge.onloggingmessage = ({ level, logger, data }) => {
        // eslint-disable-next-line no-console
        const fn = level === 'error' ? console.error : console.log;
        fn(`[plugin ${plugin_id}${logger ? ' / ' + logger : ''}]`, data);
      };

      transport = new PostMessageTransport(
        iframe.contentWindow!,
        iframe.contentWindow!,
      );

      try {
        await bridge.connect(transport);
      } catch (e) {
        if (!cancelled) {
          setError(`AppBridge connect failed: ${e instanceof Error ? e.message : String(e)}`);
        }
      }
    };

    setup().catch((e) => {
      if (!cancelled) setError(e instanceof Error ? e.message : String(e));
    });

    return () => {
      cancelled = true;
      // Best-effort teardown. `close()` on the bridge cascades into the
      // transport; the transport closes its postMessage listener so a
      // late frame can't fire a stale handler.
      void bridge?.close().catch(() => {
        /* already closed / never connected */
      });
      void transport?.close().catch(() => {});
    };
    // Re-mount the bridge if the underlying URI changes — usually only on
    // initial mount, but a card whose `resource_uri` was mutated server-side
    // should get a fresh iframe.
  }, [card.id, card.resource_uri, parsed?.plugin_id, parsed?.view_id]);

  if (!parsed) {
    return (
      <div
        className="plugin-iframe-card plugin-iframe-error"
        style={{
          border: '1px solid var(--card-border, #ddd)',
          padding: 8,
          height: '100%',
          boxSizing: 'border-box',
          fontSize: 13,
          opacity: 0.7,
        }}
      >
        Plugin card: malformed resource URI
      </div>
    );
  }

  const iframeSrc = `/api/plugins/${encodeURIComponent(parsed.plugin_id)}/resources/${encodeURIComponent(parsed.view_id)}`;

  return (
    <div
      className="plugin-iframe-card"
      style={{
        border: '1px solid var(--card-border, #ddd)',
        display: 'flex',
        flexDirection: 'column',
        height: '100%',
        boxSizing: 'border-box',
      }}
    >
      <div
        className="plugin-iframe-head card-drag-handle"
        style={{
          fontSize: 12,
          opacity: 0.7,
          padding: '4px 8px',
          userSelect: 'none',
          borderBottom: '1px solid var(--card-border, #eee)',
        }}
      >
        Plugin: {parsed.plugin_id}:{parsed.view_id}
      </div>
      {error ? (
        <div
          className="plugin-iframe-error-body"
          style={{ padding: 8, fontSize: 13, color: 'var(--error-fg, #b00)' }}
        >
          {error}
        </div>
      ) : (
        <iframe
          ref={iframeRef}
          title={`plugin ${parsed.plugin_id}/${parsed.view_id}`}
          src={iframeSrc}
          // AppBridge runs its own sandbox-proxy when the HTML carries the
          // `text/html;profile=mcp-app` MIME, which the kernel emits via the
          // M5 view-html route. We do NOT set `srcdoc` — the HTML is fetched
          // via the iframe's own GET so the kernel's CSP header on the
          // response applies to the document.
          sandbox="allow-scripts allow-same-origin"
          style={{
            flex: 1,
            border: 'none',
            width: '100%',
            background: 'transparent',
          }}
        />
      )}
    </div>
  );
}

export const PluginIframeEntry: CardEntry<PluginCardData> = {
  // Sentinel discriminator — the registry uses `card.type === 'plugin'` to
  // find this entry. Kernel cards carry the full `ui://` kind in
  // `KernelCard.kind`; `fromKernel` is what bridges the two.
  type: 'plugin',
  Component: PluginIframeCard,
  defaultSize: { w: 4, h: 6, minW: 3, minH: 3 },
  fromKernel: (k: KernelCard) => {
    if (!isPluginCardKind(k.kind)) return null;
    const parsed = parsePluginCardKind(k.kind);
    if (!parsed) return null;
    return {
      type: 'plugin',
      id: k.id,
      resource_uri: parsed.resource_uri,
    };
  },
  // No addPanel entry yet — Slice G drives discoverability from the
  // /api/plugins/views catalog, not from the static registry.
};
