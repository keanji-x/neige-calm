import { describe, expect, it } from 'vitest';
import { parseMcpConfig } from './mcp-config.js';

describe('MCP JSON configuration', () => {
  it('imports named HTTP servers and preserves headers without browser storage', () => {
    const result = parseMcpConfig(JSON.stringify({ mcpServers: { 'Docs & Search': {
      type: 'http', url: 'https://example.com/mcp',
      headers: { Authorization: 'Bearer sk-private-value', 'X-Tenant': 'tenant-team' },
    } } }));
    expect(result.kind).toBe('ready');
    if (result.kind !== 'ready') throw new Error('expected ready');
    expect(result.draft).toMatchObject({ display_name: 'Docs & Search', tool_mode: 'all',
      headers: { Authorization: 'Bearer sk-private-value', 'X-Tenant': 'tenant-team' } });
    expect(result.draft.id).toMatch(/^[a-z0-9][a-z0-9._-]*$/);
    expect(parseMcpConfig(JSON.stringify({ mcpServers: { 'Docs & Search': {
      headers: { Authorization: 'Bearer another-credential', 'X-Tenant': 'other' },
      url: 'https://example.com/mcp', type: 'http',
    } } }))).toMatchObject({ kind: 'ready', draft: { id: result.draft.id } });
  });
  it('requires an explicit choice for multiple servers', () => {
    const raw = JSON.stringify({ servers: { a: { url: 'https://a.test/mcp' }, b: { url: 'https://b.test/mcp' } } });
    expect(parseMcpConfig(raw)).toEqual({ kind: 'choose', names: ['a', 'b'] });
    expect(parseMcpConfig(raw, 'b')).toMatchObject({ kind: 'ready', draft: { display_name: 'b', url: 'https://b.test/mcp' } });
    expect(parseMcpConfig(raw, 'missing').kind).toBe('invalid');
  });
  it('accepts a direct URL object and streamable-http', () => {
    expect(parseMcpConfig('{"type":"streamable-http","url":"https://docs.example/mcp"}'))
      .toMatchObject({ kind: 'ready', draft: { display_name: 'docs.example', tools: '', tool_mode: 'all' } });
  });
  it.each([
    '', '{bad secret', '{}', 'null', '[]', '{"mcpServers":{}}',
    '{"command":"sh","args":["-c","secret"]}',
    '{"url":"https://a.test","type":"sse"}',
    '{"url":"https://a.test","oauth":{}}',
    '{"url":"https://a.test","headersHelper":"run-secret"}',
    '{"url":"https://a.test","headers":{"Authorization":"${TOKEN}"}}',
    '{"url":"https://a.test","headers":{"Authorization":1}}',
    '{"url":"https://a.test","headers":{"X-Key":"a","x-key":"b"}}',
    '{"url":"https://a.test","env":{"TOKEN":"x"}}',
    '{"url":"https://a.test","unknownOption":true}',
  ])('refuses unsupported or ambiguous config without echoing it: %s', (raw) => {
    const result = parseMcpConfig(raw);
    expect(result.kind).toBe('invalid');
    if (result.kind === 'invalid') expect(result.error).not.toContain('secret');
  });
});
