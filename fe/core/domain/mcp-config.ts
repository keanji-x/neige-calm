/** Pure import of supported remote MCP configuration. Errors never echo input.
 * No environment expansion, helper execution, browser storage or network IO.
 */
import { EMPTY_CONNECTOR_DRAFT, type ConnectorInstallDraft } from './plugins.js';

export type McpConfigResult =
  | Readonly<{ kind: 'invalid'; error: string }>
  | Readonly<{ kind: 'choose'; names: readonly string[] }>
  | Readonly<{ kind: 'ready'; draft: ConnectorInstallDraft }>;

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function invalid(error: string): McpConfigResult { return { kind: 'invalid', error }; }
function unresolved(value: string): boolean { return /\$\{|\{\{/.test(value); }

/** Identity excludes header values: rotating a key must not rename a plugin. */
function idOf(name: string, endpoint: string): string {
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 40) || 'server';
  let hash = 2166136261;
  for (const char of `${name}\n${endpoint}`) hash = Math.imul(hash ^ char.codePointAt(0)!, 16777619);
  return `mcp-${slug}-${(hash >>> 0).toString(16).padStart(8, '0')}`;
}

export function parseMcpConfig(raw: string, selectedName?: string): McpConfigResult {
  if (raw.trim() === '') return invalid('Paste the MCP configuration JSON.');
  if (raw.length > 65536) return invalid('The configuration is too large (maximum 64 KiB).');
  let parsed: unknown;
  try { parsed = JSON.parse(raw); } catch { return invalid('Invalid JSON. Check commas, quotes and brackets.'); }
  if (!object(parsed)) return invalid('Use a JSON object containing a remote server configuration.');
  let name: string | undefined;
  let entry: unknown = parsed;
  if ('mcpServers' in parsed || 'servers' in parsed) {
    if ('mcpServers' in parsed && 'servers' in parsed) return invalid('Use one server wrapper: mcpServers or servers.');
    if (Object.keys(parsed).some((key) => key !== 'servers' && key !== 'mcpServers')) {
      return invalid('Wrapper options and variable inputs are not supported. Paste only the server configuration.');
    }
    const servers = parsed.mcpServers ?? parsed.servers;
    if (!object(servers) || Object.keys(servers).length === 0) return invalid('The configuration contains no servers.');
    const names = Object.keys(servers);
    if (names.some((key) => key.trim() === '' || key.length > 128 || unresolved(key))) {
      return invalid('Server names must be non-empty text of at most 128 characters without variables.');
    }
    if (names.length > 1 && selectedName === undefined) return { kind: 'choose', names };
    name = selectedName ?? names[0];
    if (name === undefined || !Object.hasOwn(servers, name)) return invalid('Choose a server from this configuration.');
    entry = servers[name];
  }
  if (!object(entry)) return invalid('The selected server must be a JSON object.');
  if ('command' in entry || 'args' in entry || entry.type === 'stdio') return invalid('Local command (stdio) servers are not supported here. Use a remote HTTP server.');
  if ('headersHelper' in entry || 'env' in entry || 'envFile' in entry) return invalid('Helpers and environment expansion are not supported. Supply literal HTTP headers.');
  if ('oauth' in entry || 'auth' in entry) return invalid('OAuth configuration is not supported. Supply an API key in HTTP headers if the server supports it.');
  if (entry.type !== undefined && entry.type !== 'http' && entry.type !== 'streamable-http') {
    return invalid('Only HTTP / streamable-http servers are supported here. SSE and other transports are not supported.');
  }
  if (Object.keys(entry).some((key) => !['type', 'url', 'headers', 'name'].includes(key))) return invalid('This server contains unsupported options. Use only type, url, headers and name.');
  if (typeof entry.url !== 'string' || unresolved(entry.url)) return invalid('Provide a literal HTTP URL; unresolved variables are not supported.');
  // This only extracts a display label. The backend validates the complete
  // URL once for both Check and Add; core does not use browser URL globals.
  const endpoint = /^https?:\/\/([^/?#]+)(?:[/?][^\s#]*)?$/.exec(entry.url);
  const authority = endpoint?.[1];
  if (authority === undefined || /[@\\\s]/.test(authority)) return invalid('Use an HTTP URL without user credentials or a fragment.');
  if (entry.name !== undefined && (typeof entry.name !== 'string' || entry.name.trim() === '' || unresolved(entry.name))) return invalid('The server name must be non-empty text without variables.');
  const headers: Record<string, string> = {};
  if (entry.headers !== undefined) {
    if (!object(entry.headers)) return invalid('Headers must be an object with string values.');
    const seen = new Set<string>();
    for (const [key, value] of Object.entries(entry.headers)) {
      if (typeof value !== 'string' || unresolved(value) || unresolved(key)) return invalid('HTTP headers must contain literal strings; unresolved variables are not supported.');
      if (seen.has(key.toLowerCase())) return invalid('Header names must be unique regardless of letter case.');
      seen.add(key.toLowerCase());
      Object.defineProperty(headers, key, { value, enumerable: true, configurable: true, writable: true });
    }
  }
  const displayName = name ?? (typeof entry.name === 'string' ? entry.name.trim() : authority);
  return { kind: 'ready', draft: {
    ...EMPTY_CONNECTOR_DRAFT, id: idOf(displayName, entry.url), display_name: displayName,
    url: entry.url, headers,
  } };
}
