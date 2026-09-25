import { mkdir, readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

// Rasterize the canonical vector mark; no separate drawing to drift from the brand.
const mark = await readFile(new URL('../../web/src/ui/brand/neige-mark.svg', import.meta.url), 'utf8');
const output = new URL('../../web/public/icons/', import.meta.url);
await mkdir(output, { recursive: true });
const browser = await chromium.launch();
try {
  for (const size of [192, 512]) {
    const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
    await page.setContent(`<style>body { margin: 0; background: white; } svg { width: 100vw; height: 100vh; }</style>${mark}`);
    await page.screenshot({ path: fileURLToPath(new URL(`neige-${size}.png`, output)) });
    await page.close();
  }
} finally {
  await browser.close();
}
