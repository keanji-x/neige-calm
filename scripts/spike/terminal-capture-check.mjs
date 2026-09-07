#!/usr/bin/env node
// Real production TerminalCardView + xterm, with controlled WS output. No PTY
// or model is launched. Run against this checkout's Vite development server.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { resolve } from 'node:path';
import { captureTerminal } from './terminal-capture.mjs';

const require = createRequire(new URL('../../fe/package.json', import.meta.url));
const { chromium } = require('playwright');
const { values } = parseArgs({ options: {
  'base-url': { type: 'string' }, 'output-dir': { type: 'string' },
} });
if (!values['base-url'] || !values['output-dir']) {
  throw new Error('Usage: node scripts/spike/terminal-capture-check.mjs --base-url URL --output-dir NEW_DIR');
}
const output = resolve(values['output-dir']);
await mkdir(output, { mode: 0o700 });
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1000, height: 700 } });
  page.setDefaultTimeout(5000);
  const errors = [];
  const inputs = [];
  let terminalSocket;
  page.on('pageerror', (error) => errors.push(error.message));
  await page.route('**/api/**', (route) => route.fulfill({ json: {} }));
  await page.routeWebSocket('**/api/terminals/capture-terminal', (socket) => {
    terminalSocket = socket;
    socket.onMessage((raw) => {
      const message = JSON.parse(String(raw));
      if (message.Input) inputs.push(message.Input);
      if (!message.ClientHello) return;
      const hello = message.ClientHello;
      const size = hello.desired_size;
      socket.send(JSON.stringify({ ServerHello: {
        protocol_version: 4, terminal_id: 'capture-terminal', session_id: 'capture-session',
        client_role: 'Owner', owner_client_id: hello.client_id, pty_size: size,
        pty_seq_head: 0, pty_seq_tail: 1, render_rev: 1,
        snapshot: { render_rev: 1, pty_seq: 1, cols: size.cols, rows: size.rows,
          encoding: 'Vt', scrollback: null,
          data: Array.from(Buffer.from('\x1b[2J\x1b[H\x1b[?25lPreview terminal — 中文\r\n\x1b[7m /help \x1b[0m\r\n /status\r\n> /')) },
        history_gap: null, is_child_ready: true,
      } }));
    });
  });
  await page.goto(new URL('/next/', values['base-url']).href);
  // Load the production card, not a copy of its markup. These browser imports
  // are served by Vite, whose optimized dependencies live outside fe/web.
  const dependency = (name) => `/next/@fs${fileURLToPath(new URL(`../../fe/node_modules/.vite/deps/${name}.js`, import.meta.url))}`;
  await page.evaluate(async ({ react, reactDom }) => {
    const { default: { createElement } } = await import(react);
    const { default: { createRoot } } = await import(reactDom);
    const { TerminalCardView } = await import('/next/src/systems/cards/builtins/terminal-card.tsx');
    // The API is intentionally stubbed, so establish the theme that the
    // authenticated app shell normally supplies to both CSS and xterm.
    document.documentElement.dataset.theme = 'dark';
    const host = document.createElement('div');
    host.className = 'track-card';
    host.style.cssText = 'position:fixed;left:20px;top:20px;width:700px;height:420px;z-index:10000;background:#0f1418';
    document.body.append(host);
    createRoot(host).render(createElement(TerminalCardView, {
      card: { id: 'card-1', title: 'Probe', terminalId: 'capture-terminal',
        sessionState: 'running', cwd: null, gateCwd: null },
      host: { lifecycle: { getSnapshot: () => ({ visible: true }), subscribe: () => () => {} } },
    }));
  }, { react: dependency('react'), reactDom: dependency('react-dom_client') });
  await page.locator('.xterm-view .xterm-screen').waitFor();
  await page.getByRole('img', { name: 'status Working' }).waitFor();
  // Allow the controlled one-shot frame to render. This is only a probe/test
  // settle delay, not the eventual production atomic observation protocol.
  await page.waitForTimeout(300);
  assert.equal(await page.locator('[data-nc-terminal-id="capture-terminal"]').count(), 2);
  const capture = await captureTerminal(page, 'capture-terminal');
  assert.equal(capture.png.subarray(1, 4).toString(), 'PNG');
  assert.ok(capture.png.length > 1000, 'capture contains rendered terminal pixels');
  const darkBackground = await page.evaluate(async (bytes) => {
    const image = await createImageBitmap(new Blob([Uint8Array.from(bytes)], { type: 'image/png' }));
    const canvas = document.createElement('canvas');
    canvas.width = image.width;
    canvas.height = image.height;
    const context = canvas.getContext('2d');
    context.drawImage(image, 0, 0);
    const pixel = context.getImageData(image.width - 10, image.height - 10, 1, 1).data;
    image.close();
    return pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80;
  }, Array.from(capture.png));
  assert.ok(darkBackground, 'PNG must capture the dark terminal background, not a white page');
  await writeFile(resolve(output, 'terminal.png'), capture.png, { mode: 0o600 });
  await writeFile(resolve(output, 'capture.json'), JSON.stringify(capture.manifest, null, 2), { mode: 0o600 });
  assert.ok(terminalSocket);
  // Force a real connection-state transition between pixels and the final
  // availability check, while still using the actual browser screenshot.
  await assert.rejects(captureTerminal({
    locator: page.locator.bind(page),
    screenshot: async (options) => {
      const png = await page.screenshot(options);
      await terminalSocket.close({ code: 1000, reason: 'probe-disconnect' });
      await page.locator('.xterm-view [data-nc-error-box]').waitFor();
      return png;
    },
  }, 'capture-terminal'), /unavailable/);
  await assert.rejects(captureTerminal(page, 'capture-terminal'), /unavailable/);
  assert.deepEqual(inputs, [], 'capture never sends terminal input');
  assert.deepEqual(errors, []);
  process.stdout.write('PASS: production card capture; disconnect before/during capture rejected; no input\n');
} finally {
  await browser.close();
}
