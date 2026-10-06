import { createServer as createHttpServer } from 'node:http';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import { createServer, build, preview } from 'vite';
import react from '@vitejs/plugin-react';
import { previewConfig } from './config.mjs';
const config = previewConfig(process.argv[2] ?? '5198', process.argv[3] ?? 'motion', process.argv[4] ?? 'development');
const session = process.argv[5] ?? 'unmanaged';
if (session !== 'unmanaged' && !/^[a-f0-9-]{36}$/.test(session)) throw new Error('Invalid preview session identity');
const base = '/next/';
const plugins = [react()];
/** @type {import('vite').InlineConfig} */
const shared = { configFile: false, root: resolve(config.fe, 'web'), base, plugins, define: { __NC_BUNDLED__: 'false', __NC_VERSION__: JSON.stringify('preview'), __NC_BUILD__: JSON.stringify('preview') } };
/** @param {string} css @param {string} entry */
const html = (css, entry) => `<!doctype html><html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Neige · 交互预览</title><meta name="nc-preview-session" content="${session}">${css}</head><body><div id="root"></div><script type="module" src="${entry}"></script></body></html>`;
/** @type {() => Promise<void>} */
let close;
if (config.mode === 'production') {
  const outDir = resolve(config.fe, `node_modules/.cache/preview-${config.kind}-${config.port}-${session}`); const virtual = '\0neige-preview-entry';
  await mkdir(outDir, { recursive: true });
  await build({ ...shared, plugins: [...plugins, { name: 'neige-preview-entry',
    resolveId(id) { if (id === 'neige-preview-entry') return virtual; },
    load(id) { if (id === virtual) return `import ${JSON.stringify(resolve(config.fe, 'web/src/styles/entry.css'))}; import ${JSON.stringify(config.entry)};`; },
  }], build: { outDir, emptyOutDir: true, manifest: true, rollupOptions: { input: 'neige-preview-entry' } } });
/** @type {import('vite').Manifest} */
  const manifest = JSON.parse(await readFile(resolve(outDir, '.vite/manifest.json'), 'utf8')); const entries = Object.values(manifest).filter(value => value.isEntry);
  if (entries.length !== 1) throw new Error('Preview build must have one entry'); const entry = entries[0];
  const css = (entry.css ?? []).map(file => `<link rel="stylesheet" href="${base}${file}">`).join('');
  await writeFile(resolve(outDir, 'index.html'), html(css, `${base}${entry.file}`));
  const server = await preview({ ...shared, build: { outDir }, preview: { host: '127.0.0.1', port: config.port, strictPort: true } });
  close = () => new Promise((resolve, reject) => server.httpServer.close(error => error ? reject(error) : resolve(undefined)));
} else {
  const server = createHttpServer(async (request, response) => {
    if (request.url?.split('?')[0] === config.route) { try { const page = await vite.transformIndexHtml(config.route, html('<link rel="stylesheet" href="/src/styles/entry.css">', `/@fs${config.entry}`)); response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' }); response.end(page); } catch (error) { response.writeHead(500); response.end(String(error)); } }
    else vite.middlewares(request, response, () => { response.writeHead(404); response.end(); });
  });
  const vite = await createServer({ ...shared, cacheDir: resolve(config.fe, `node_modules/.cache/${config.kind}-preview`), optimizeDeps: { entries: [config.entry] }, server: { middlewareMode: true, hmr: { server }, fs: { allow: [config.fe] } }, appType: 'custom' });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(config.port, '127.0.0.1', () => resolve(undefined)); });
  close = async () => { await vite.close(); await new Promise(resolve => server.close(resolve)); };
}
process.stdout.write(`Neige preview: ${config.url} (${config.mode})\n`);
let closing = false; const stop = async () => { if (closing) return; closing = true; await close(); };
process.once('SIGINT', stop); process.once('SIGTERM', stop);
