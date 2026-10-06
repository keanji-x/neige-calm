import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

// Compose the app icon from the canonical mark without changing in-app branding.
const mark = await readFile(new URL('../../web/src/ui/brand/neige-mark.svg', import.meta.url), 'utf8');
const snowflake = mark
  .replace('<svg ', '<svg x="116" y="116" width="280" height="280" ')
  .replace('stroke="#3C528D"', 'stroke="#ffffff"');
const icon = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" role="img" aria-label="Neige Calm">
  <defs>
    <linearGradient id="app-background" x1="0" y1="0" x2="1" y2="1">
      <stop stop-color="#3c528d" />
      <stop offset="1" stop-color="#6260aa" />
    </linearGradient>
  </defs>
  <path d="M128 0H384C472 0 512 40 512 128V384C512 472 472 512 384 512H128C40 512 0 472 0 384V128C0 40 40 0 128 0Z" fill="#ffffff" />
  <circle cx="256" cy="256" r="190" fill="url(#app-background)" />
  ${snowflake}
</svg>
`;
const output = new URL('../../web/public/icons/', import.meta.url);
await mkdir(output, { recursive: true });
await writeFile(new URL('../../web/src/ui/brand/neige-app.svg', import.meta.url), icon);
await writeFile(new URL('../../../mobile/www/neige-app.svg', import.meta.url), icon);
// Android uses a 108dp layer with a central 72dp viewport: inset the whole icon.
const androidForeground = icon.replace('viewBox="0 0 512 512"', 'viewBox="-128 -128 768 768"');
await writeFile(new URL('../../../mobile/app-icon-foreground.svg', import.meta.url), androidForeground);
const browser = await chromium.launch();
try {
  for (const size of [192, 512]) {
    const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
    await page.setContent(`<style>body { margin: 0; } body > svg { width: 100vw; height: 100vh; }</style>${icon}`);
    await page.screenshot({ path: fileURLToPath(new URL(`neige-${size}.png`, output)), omitBackground: true });
    await page.close();
  }
} finally {
  await browser.close();
}
