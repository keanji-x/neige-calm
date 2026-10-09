import { chromium } from '@playwright/test';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { build, preview } from 'vite';
import react from '@vitejs/plugin-react';
// Cold first-open comparison against exact production source from Git.
// The baseline/bundle use a temporary directory; raw results use ignored test-results.
const root = resolve(import.meta.dirname, '..');
const baselineRef = process.argv[2] ?? '4e96394e4';
const directory = await mkdtemp(resolve(tmpdir(), 'neige-code-benchmark-'));
await symlink(resolve(root, 'node_modules'), resolve(directory, 'node_modules'), 'dir');
const baseline = execFileSync('git', ['show', `${baselineRef}:fe/web/src/systems/fs-viewers/code-pane.tsx`], { cwd: root, encoding: 'utf8' });
if (/from ['"]\./.test(baseline)) throw new Error('Baseline must use external-package imports only');
await writeFile(resolve(directory, 'baseline.tsx'), baseline);
await writeFile(resolve(directory, 'index.html'), `<!doctype html><html data-theme="dark"><head><meta charset="utf-8"><title>Code first-open benchmark</title></head><body><div id="root" style="width:600px;height:440px"></div><script type="module">
import React from 'react';
import { createRoot } from 'react-dom/client';
import ${JSON.stringify(resolve(root, 'web/src/styles/entry.css'))};
const query = new URLSearchParams(location.search);
const text = ('def main():\\n    print("' + 'x'.repeat(80) + '")\\n').repeat(Number(query.get('rows')) / 2);
const start = performance.now();
const mode = query.get('mode');
let component;
if (mode === 'baseline') component = React.createElement((await import('./baseline.tsx')).CodePane, {path:'main.py',text,theme:'dark'});
else if (mode === 'full') component = React.createElement((await import(${JSON.stringify(resolve(root, 'web/src/systems/fs-viewers/code-pane.tsx'))})).CodePane, {path:'main.py',text,theme:'dark'});
else component = React.createElement((await import(${JSON.stringify(resolve(root, 'web/src/ui/code/public.tsx'))})).ReadOnlyCode, {source:{kind:'filename',value:'main.py'},text,theme:'dark'});
createRoot(document.getElementById('root')).render(component);
const check = () => {
  const content = document.querySelector('[role="textbox"]');
  const ready = mode === 'readonly' ? document.querySelector('[data-nc-code-status="ready"], [data-nc-code-status="limited"]') : content?.querySelector('span');
  if (content && ready) window.result = {firstOpenMs:performance.now()-start, resources:performance.getEntriesByType('resource').map(r=>({name:r.name.split('/').pop(),bytes:r.transferSize}))};
  else requestAnimationFrame(check);
};
requestAnimationFrame(check);
</script></body></html>`);
const outDir = resolve(directory, 'dist');
await build({ configFile: false, root: directory, plugins: [react()], resolve: { dedupe: ['react', 'react-dom', '@codemirror/state', '@codemirror/view', '@codemirror/language'] }, build: { outDir, emptyOutDir: true,
  rolldownOptions: { input: resolve(directory, 'index.html') } } });
const server = await preview({ configFile: false, root: directory, build: { outDir }, preview: { host: '127.0.0.1', port: 0 } });
const address = server.httpServer.address();
if (address === null || typeof address === 'string') throw new Error('No benchmark TCP address');
let browser;
/** @type {{ rows: number; mode: string; sample: number; firstOpenMs: number; resources: { name: string; bytes: number }[] }[]} */
const records = [];
try {
  browser = await chromium.launch({ headless: true });
  for (const rows of [200, 4000]) {
    for (const mode of ['baseline', 'readonly', 'full']) {
      for (let sample = 0; sample < 5; sample++) {
        const context = await browser.newContext();
        const page = await context.newPage();
        /** @type {string[]} */
        const errors = [];
        page.on('pageerror', error => errors.push(error.message));
        await page.goto(`http://127.0.0.1:${address.port}/index.html?mode=${mode}&rows=${rows}`);
        await page.waitForFunction(() => Reflect.has(globalThis, 'result'), undefined, { timeout: 30000 });
        const result = await page.evaluate(() => /** @type {{ firstOpenMs: number; resources: { name: string; bytes: number }[] }} */ (Reflect.get(globalThis, 'result')));
        if (errors.length) throw new Error(errors.join('\n'));
        records.push({ rows, mode, sample, ...result });
        await context.close();
      }
    }
  }
} finally {
  await browser?.close();
  await new Promise((resolve, reject) => server.httpServer.close(error => error ? reject(error) : resolve(undefined)));
}
await mkdir(resolve(root, 'test-results/code-benchmark'), { recursive: true });
await writeFile(resolve(root, 'test-results/code-benchmark/results.json'), JSON.stringify({ baselineRef, records }, null, 2) + '\n');
await rm(directory, { recursive: true, force: true });
for (const rows of [200, 4000]) {
  for (const mode of ['baseline', 'readonly', 'full']) {
    const samples = records.filter(r => r.rows === rows && r.mode === mode);
    const times = samples.map(s => s.firstOpenMs).sort((a,b)=>a-b);
    console.log(JSON.stringify({ rows, mode, medianMs: times[2], minMs: times[0], maxMs: times[4], transferredBytes: samples[0].resources.reduce((s,r)=>s+r.bytes,0), resources: samples[0].resources.length, baselineRef }));
  }
}
