import { createServer } from 'node:http';
import { networkInterfaces } from 'node:os';
import { expect, test } from '@playwright/test';

test('checks an unsaved MCP configuration, installs it, and enables every catalog page', async ({ page, request }) => {
  const seen: { method: string; auth: string; tenant: string }[] = [];
  const upstream = createServer((req, res) => {
    let body = '';
    req.on('data', (chunk: Buffer) => { body += chunk.toString(); });
    req.on('end', () => {
      const rpc = JSON.parse(body) as { id: number; method: string; params: { cursor?: string } };
      seen.push({ method: rpc.method, auth: String(req.headers.authorization ?? ''), tenant: String(req.headers['x-tenant'] ?? '') });
      const result = rpc.method === 'initialize'
        ? { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' } }
        : rpc.params.cursor === 'second'
          ? { tools: [{ name: 'fetch', inputSchema: { type: 'object' } }] }
          : { tools: [{ name: 'search', inputSchema: { type: 'object' } }], nextCursor: 'second' };
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify({ jsonrpc: '2.0', id: rpc.id, result }));
    });
  });
  await new Promise<void>((resolve) => upstream.listen(0, '0.0.0.0', resolve));
  const address = upstream.address();
  if (address === null || typeof address === 'string') throw new Error('Missing fixture port');
  // CI's backend is in Docker; loopback would point at the container itself.
  // The fixture carries only synthetic credentials and closes in finally.
  const fixtureHost = Object.values(networkInterfaces()).flat()
    .find((address) => address?.family === 'IPv4' && !address.internal)?.address ?? '127.0.0.1';
  const name = `MCP browser ${Date.now()}`;
  let pluginId: string | undefined;
  try {
    const before = await (await request.get('/api/plugins')).json() as unknown;
    await page.goto('/next/settings/plugins');
    await page.getByText('Add a plugin', { exact: true }).click();
    await page.getByRole('textbox', { name: 'MCP configuration' }).fill(JSON.stringify({ mcpServers: {
      [name]: { type: 'http', url: `http://${fixtureHost}:${address.port}/mcp`, headers: { Authorization: 'Bearer fixture-private-token', 'X-Tenant': 'team' } },
    } }, null, 2));
    await page.getByRole('button', { name: 'Check connection' }).click();
    await expect(page.getByRole('status', { name: 'Connection check' })).toContainText('2 tools discovered');
    expect(await (await request.get('/api/plugins')).json()).toEqual(before);
    expect(seen.map((entry) => entry.method)).toEqual(['initialize', 'tools/list', 'tools/list']);
    await page.getByText('View tool names', { exact: true }).click();
    await expect(page.getByRole('listitem').filter({ hasText: /^fetch$/ })).toBeVisible();
    await page.screenshot({ path: 'test-results/plugin-setup-integrated.png', fullPage: true });
    const installed = page.waitForResponse((response) => response.url().endsWith('/api/plugins/install') && response.request().method() === 'POST');
    await page.getByRole('button', { name: 'Add plugin' }).click();
    const response = await installed;
    expect(response.status()).toBe(201);
    const detail = await response.json() as { id: string; enabled: boolean };
    pluginId = detail.id;
    expect(detail.enabled).toBe(false);
    expect(JSON.stringify(detail)).not.toContain('fixture-private-token');
    await page.getByRole('switch', { name: `Enable ${name}` }).click();
    await expect.poll(async () => {
      const result = await (await request.get(`/api/plugins/${pluginId}`)).json() as { state: string };
      return result.state;
    }).toBe('running');
    expect(seen.every((entry) => entry.auth === 'Bearer fixture-private-token' && entry.tenant === 'team')).toBe(true);
    expect(seen.some((entry) => entry.method === 'tools/call')).toBe(false);
  } finally {
    if (pluginId !== undefined) await request.delete(`/api/plugins/${pluginId}`);
    await new Promise<void>((resolve, reject) => upstream.close((error) => error ? reject(error) : resolve()));
  }
});
