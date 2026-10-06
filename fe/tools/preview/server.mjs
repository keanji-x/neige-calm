import { createServer as createHttpServer } from 'node:http';
import { resolve } from 'node:path';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';

// A development-only surface composed from production components; no API or user data.
const port = Number(process.argv[2] ?? 5198);
const kind = process.argv[3] ?? 'motion';
if (kind !== 'motion' && kind !== 'links') throw new Error('Expected motion or links preview');
const route = `/next/${kind === 'links' ? 'link' : 'motion'}-preview`;
const entry = resolve(import.meta.dirname, `../../web/src/app/shell/${kind === 'links' ? 'link' : 'motion'}-preview.tsx`);
const server = createHttpServer(async (request, response) => {
  if (request.url?.split('?')[0] === route) {
    try {
      const html = await vite.transformIndexHtml(route, `<!doctype html><html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Neige · 交互预览</title><link rel="stylesheet" href="/src/styles/entry.css"></head><body><div id="root"></div><script type="module" src="/@fs${entry}"></script></body></html>`);
      response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      response.end(html);
    } catch (error) {
      if (error instanceof Error) vite.ssrFixStacktrace(error);
      response.writeHead(500); response.end(String(error));
    }
  } else vite.middlewares(request, response, () => { response.writeHead(404); response.end(); });
});
const vite = await createServer({
  configFile: false,
  root: resolve(import.meta.dirname, '../../web'),
  base: '/next/',
  cacheDir: resolve(import.meta.dirname, `../../node_modules/.cache/${kind}-preview`),
  plugins: [react()],
  define: { __NC_BUNDLED__: 'false', __NC_VERSION__: JSON.stringify('preview'), __NC_BUILD__: JSON.stringify('preview') },
  optimizeDeps: { entries: [entry] },
  server: { middlewareMode: true, hmr: { server }, fs: { allow: [resolve(import.meta.dirname, '../..')] } },
  appType: 'custom',
});
server.listen(port, '127.0.0.1', () => process.stdout.write(`Neige preview: http://127.0.0.1:${port}${route}\n`));
const close = async () => { server.close(); await vite.close(); };
process.once('SIGINT', close); process.once('SIGTERM', close);
