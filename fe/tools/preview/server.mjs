import { createServer as createHttpServer } from 'node:http';
import { resolve } from 'node:path';
import { createServer } from 'vite';

// A development-only surface composed from production components; no API or user data.
const port = Number(process.argv[2] ?? 5198);
const vite = await createServer({ server: { middlewareMode: true }, appType: 'custom' });
const entry = resolve(import.meta.dirname, 'edit-motion.tsx');
const server = createHttpServer(async (request, response) => {
  if (request.url?.split('?')[0] === '/next/motion-preview') {
    try {
      const html = await vite.transformIndexHtml('/next/motion-preview', `<!doctype html><html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Neige · 动画预览</title></head><body><div id="root"></div><script type="module" src="/@fs${entry}"></script></body></html>`);
      response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      response.end(html);
    } catch (error) {
      vite.ssrFixStacktrace(error);
      response.writeHead(500); response.end(String(error));
    }
  } else vite.middlewares(request, response, () => { response.writeHead(404); response.end(); });
});
server.listen(port, '127.0.0.1', () => process.stdout.write(`Motion preview: http://127.0.0.1:${port}/next/motion-preview\n`));
const close = async () => { server.close(); await vite.close(); };
process.once('SIGINT', close); process.once('SIGTERM', close);
