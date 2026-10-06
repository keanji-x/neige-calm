import { createServer } from 'node:http';
import { afterEach, expect, it } from 'vitest';
import { previewConfig, previewReadiness } from './config.mjs';
const servers: ReturnType<typeof createServer>[] = [];
afterEach(async () => { await Promise.all(servers.map(server => new Promise<void>(resolve => server.close(() => resolve())))); servers.length = 0; });
it.each(['', '0', '-1', '65536', '12;stop', 'NaN'])('rejects invalid port %s', port => { expect(() => previewConfig(port, 'motion', 'production')).toThrow('Expected port'); });
it('rejects unknown kind and mode before starting a process', () => {
  expect(() => previewConfig('5224', 'unknown', 'production')).toThrow('Expected motion');
  expect(() => previewConfig('5224', 'motion', 'unknown')).toThrow('Expected development');
});
it.each(['correct', 'foreign', 'broken', 'wrong-type', 'redirect'])('validates the real HTTP page/asset contract: %s', async mode => {
  const server = createServer((request, response) => {
    if (request.url === '/next/motion-preview') {
      response.setHeader('content-type', 'text/html');
      response.end(`<meta name="nc-preview-session" content="${mode === 'foreign' ? 'other' : 'owned'}"><div id="root"></div><script src="/next/entry.js"></script>`);
    } else if (mode === 'redirect') { response.writeHead(302, { location: 'https://example.invalid/asset' }); response.end(); }
    else { response.setHeader('content-type', mode === 'broken' ? 'text/html' : mode === 'wrong-type' ? 'application/json' : 'text/javascript'); response.end(mode === 'broken' ? '<html>fallback</html>' : 'export const ready = true;'); }
  });
  servers.push(server); await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address(); if (address === null || typeof address === 'string') throw new Error('No port');
  const config = previewConfig(String(address.port), 'motion', 'production');
  if (mode === 'correct') await expect(previewReadiness(config, 'owned')).resolves.toMatchObject({ service: 'ready', forwarding: 'not-verified' });
  else await expect(previewReadiness(config, 'owned')).rejects.toBeDefined();
});

it.each(['text/css', 'text/plain'])('checks a declared stylesheet MIME: %s', async type => {
  const server = createServer((request, response) => {
    if (request.url === '/next/motion-preview') { response.setHeader('content-type', 'text/html'); response.end('<meta name="nc-preview-session" content="owned"><div id="root"></div><link rel="stylesheet" href="/style.css"><script src="/entry.js"></script>'); }
    else { response.setHeader('content-type', request.url === '/style.css' ? type : 'text/javascript'); response.end(request.url === '/style.css' ? 'body{color:black}' : 'export const ready=true;'); }
  });
  servers.push(server); await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address(); if (address === null || typeof address === 'string') throw new Error('No port');
  const config = previewConfig(String(address.port), 'motion', 'production');
  if (type === 'text/css') await expect(previewReadiness(config, 'owned')).resolves.toMatchObject({ service: 'ready' });
  else await expect(previewReadiness(config, 'owned')).rejects.toThrow('Preview asset is not ready');
});
