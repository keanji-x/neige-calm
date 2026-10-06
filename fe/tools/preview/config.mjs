import { resolve } from 'node:path';
const kinds = Object.freeze({ motion: 'motion', links: 'link' });
/** @param {string | undefined} port @param {string} kind @param {string} mode */
export function previewConfig(port, kind, mode) {
  const number = Number(port);
  if (!/^\d+$/.test(String(port)) || !Number.isInteger(number) || number < 1 || number > 65535) throw new Error('Expected port 1..65535');
  const name = Object.entries(kinds).find(([key]) => key === kind)?.[1];
  if (name === undefined) throw new Error('Expected motion or links preview');
  if (mode !== 'development' && mode !== 'production') throw new Error('Expected development or production mode');
  const fe = resolve(import.meta.dirname, '../..');
  return Object.freeze({ port: number, kind, mode, fe, route: `/next/${name}-preview`, entry: resolve(fe, `web/src/app/shell/${name}-preview.tsx`), unit: `neige-preview-${kind}-${number}.service`, url: `http://127.0.0.1:${number}/next/${name}-preview` });
}
/** Probe each declared initial asset; an HTML fallback is not a loaded script. */
/** @param {ReturnType<typeof previewConfig>} config @param {string} session */
export async function previewReadiness(config, session) {
  const page = await fetch(config.url, { signal: AbortSignal.timeout(2000), redirect: 'error' });
  if (!page.ok || !page.headers.get('content-type')?.includes('text/html')) throw new Error('Preview page is not ready');
  const html = await page.text(); if (!html.includes('id="root"')) throw new Error('Preview page has no root');
  if (!html.includes(`<meta name="nc-preview-session" content="${session}">`)) throw new Error('Preview session identity does not match');
  const assets = [...html.matchAll(/<(?:script|link)\b[^>]*?(?:src|href)="([^"]+)"/g)].map(match => match[1]);
  if (!assets.some(path => path.endsWith('.js') || path.includes('/@fs'))) throw new Error('Preview entry is missing');
  for (const path of assets) {
    const url = new URL(path, config.url); if (url.origin !== new URL(config.url).origin) throw new Error('Preview asset must be local');
    const asset = await fetch(url, { signal: AbortSignal.timeout(2000), redirect: 'error' }); const type = asset.headers.get('content-type') ?? '';
    if (!asset.ok || type.includes('text/html') || !(await asset.text()).trim()) throw new Error(`Preview asset is not ready: ${path}`);
  }
  return { service: 'ready', url: config.url, forwarding: 'not-verified' };
}
