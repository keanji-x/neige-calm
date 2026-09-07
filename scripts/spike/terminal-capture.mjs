#!/usr/bin/env node
// Developer-only S0 probe. Attach to an explicitly supplied managed browser;
// capture one existing terminal without navigating, resizing, or sending input.
import { createRequire } from 'node:module';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const require = createRequire(new URL('../../fe/package.json', import.meta.url));

/** Capture pixels only. This does NOT claim atomic screen-state observation. */
export async function captureTerminal(page, terminalId) {
  if (!/^[a-zA-Z0-9_-]{1,128}$/.test(terminalId)) throw new Error('Invalid terminal ID');
  const terminal = page.locator(`[data-nc-terminal-id="${terminalId}"]`);
  if (await terminal.count() !== 1) throw new Error('Expected exactly one bound terminal');
  // Reject offscreen targets before screenshot() can auto-scroll the page.
  const before = await terminal.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    const visible = document.visibilityState === 'visible'
      && rect.width > 0 && rect.height > 0
      && rect.top >= 0 && rect.left >= 0
      && rect.bottom <= window.innerHeight && rect.right <= window.innerWidth;
    const screen = element.querySelector('.xterm-screen');
    const status = element.querySelector('.xterm-status');
    return { visible, mounted: screen !== null, status: status?.textContent ?? null,
      bounds: { x: rect.x, y: rect.y, width: rect.width, height: rect.height } };
  });
  if (!before.visible || !before.mounted || before.status !== null) {
    throw new Error('Terminal surface unavailable or not fully visible');
  }
  // Browser-native clipping also captures the canvas/WebGL renderer. It does
  // not read canvas.toDataURL or recreate terminal glyphs from a text dump.
  const png = await page.screenshot({ type: 'png', clip: before.bounds, timeout: 5000 });
  const after = await terminal.boundingBox();
  if (JSON.stringify(after) !== JSON.stringify(before.bounds)) {
    throw new Error('Terminal moved during capture; discard the image');
  }
  return { png, manifest: {
    terminal_id: terminalId, captured_at: new Date().toISOString(),
    bounds: before.bounds, mime_type: 'image/png', bytes: png.length,
    observation_consistency: 'pixels_only_unverified',
  } };
}

async function main() {
  const { values } = parseArgs({ options: {
    'browser-url': { type: 'string' }, 'page-url': { type: 'string' },
    'terminal-id': { type: 'string' }, 'output-dir': { type: 'string' },
  } });
  for (const key of ['browser-url', 'page-url', 'terminal-id', 'output-dir']) {
    if (!values[key]) throw new Error(`Missing --${key}`);
  }
  const { chromium } = require('playwright');
  const browser = await chromium.connectOverCDP(values['browser-url'], { timeout: 5000 });
  try {
    const pages = browser.contexts().flatMap((context) => context.pages())
      .filter((page) => page.url() === values['page-url']);
    if (pages.length !== 1) throw new Error('Expected exactly one page matching --page-url');
    const capture = await captureTerminal(pages[0], values['terminal-id']);
    const dir = resolve(values['output-dir']);
    // Refuse existing directories so repeated probes never overwrite evidence.
    await mkdir(dir, { mode: 0o700 });
    await writeFile(resolve(dir, 'terminal.png'), capture.png, { flag: 'wx', mode: 0o600 });
    await writeFile(resolve(dir, 'capture.json'), `${JSON.stringify(capture.manifest, null, 2)}\n`,
      { flag: 'wx', mode: 0o600 });
    process.stdout.write(`${dir}\n`);
  } finally {
    // close() disconnects a connected browser client; it does not stop the
    // browser that the developer supplied via connectOverCDP.
    await browser.close();
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
}
