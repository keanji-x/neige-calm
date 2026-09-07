#!/usr/bin/env node
// Developer acceptance: actual RMUX + fzf, immutable recovery capture + xterm.
// No model, application credentials, database, or production Terminal creation.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { closePreview, deadline } from './terminal-preview-cleanup.mjs';

const require = createRequire(new URL('../../fe/package.json', import.meta.url));
const { chromium } = require('playwright');
const { values } = parseArgs({ options: {
  driver: { type: 'string' }, runtime: { type: 'string' }, output: { type: 'string' },
} });
assert.ok(values.driver && values.runtime && values.output, '--driver BIN --runtime BIN --output NEW_DIR required');
const output = resolve(values.output);
await mkdir(output, { mode: 0o700 });
const xterm = require.resolve('@xterm/xterm');
const assets = new Map([
  ['/xterm.js', ['application/javascript', await readFile(xterm)]],
  ['/xterm.css', ['text/css', await readFile(resolve(dirname(xterm), '../css/xterm.css'))]],
  ['/', ['text/html', Buffer.from(`<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="/xterm.css">
  <style>body{margin:16px;background:#0f1418}#terminal{display:inline-block;padding:8px}</style>
  <div id="terminal"></div><script src="/xterm.js"></script><script>
  window.term = new Terminal({cols:80,rows:24,fontSize:16,fontFamily:'monospace',scrollback:2000,
    cursorBlink:false,theme:{background:'#0f1418',foreground:'#d4d4d4'}});
  term.open(document.querySelector('#terminal'));
  </script>`)]],
]);
const server = createServer((req, res) => {
  const asset = assets.get(req.url);
  if (!asset) { res.writeHead(404).end(); return; }
  res.writeHead(200, { 'Content-Type': asset[0] }).end(asset[1]);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const program = `i=1; while [ "$i" -le 80 ]; do printf 'history %02d\\r\\n' "$i"; i=$((i+1)); done
printf '\\033[38;5;1mPALETTE\\033[38;2;0;0;1mRGB\\033[0m\\r\\n'
choice=$({ printf '/help 帮助\\n/status 状态\\n/settings 设置\\n'; i=1; while [ "$i" -le 40 ]; do printf '/command-%02d\\n' "$i"; i=$((i+1)); done; } | /usr/bin/fzf --reverse --no-sort --no-info --prompt='> ')
printf '\\r\\nRESULT:%s\\r\\n' "$choice"
read hold`;
const driver = spawn(resolve(values.driver), ['--runtime-executable', resolve(values.runtime),
  '--cwd', output, '--program', '/bin/sh', '--', '-c', program], {
  stdio: ['pipe', 'pipe', 'pipe'], env: { PATH: '/usr/bin:/bin', LANG: 'C.UTF-8' },
});
let stderr = '';
driver.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-8192); });
// Individual writes reject through their callback; prevent an unhandled stream error.
driver.stdin.on('error', () => {});
const lines = createInterface({ input: driver.stdout })[Symbol.asyncIterator]();
const ended = once(driver, 'exit');
void ended.catch(() => {});
let browser;
async function receive() {
  const next = await deadline(lines.next(), 'driver reply timeout');
  assert.ok(!next.done, `driver closed: ${stderr}`);
  return JSON.parse(next.value);
}
let pending = Promise.resolve();
const inputs = [];
let closing = false;
function request(action) {
  const result = pending.then(async () => {
    assert.ok(!closing, 'preview is closing');
    if (action.action === 'text' || action.action === 'key') inputs.push(action);
    await new Promise((resolve, reject) => driver.stdin.write(JSON.stringify(action) + '\n',
      error => error ? reject(error) : resolve()));
    const response = await receive();
    assert.ok(!response.error, response.error);
    return response.result;
  });
  pending = result.catch(() => {});
  return result;
}

const text = capture => {
  const { cols, rows, cells } = capture.snapshot;
  return Array.from({ length: rows }, (_, row) => cells.slice(row * cols, (row + 1) * cols)
    .map(cell => cell.glyph.padding ? '' : cell.glyph.text).join('')).join('\n');
};
async function until(predicate) {
  const until = Date.now() + 5000;
  let last;
  do {
    const capture = await request({ action: 'observe' });
    last = capture;
    if (predicate(capture)) return capture;
    await new Promise(resolve => setTimeout(resolve, 25));
  } while (Date.now() < until);
  throw new Error(`terminal did not reach expected state:\n${text(last)}`);
}
try {
  assert.deepEqual(await receive(), { ready: true });
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1000, height: 650 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.exposeFunction('previewInput', data => closing ? undefined : request({ action: 'text', text: data }));
  await page.evaluate(() => term.onData(data => { void window.previewInput(data); }));
  async function render(capture, name) {
    await page.evaluate(async frame => {
      term.reset(); term.resize(frame.snapshot.cols, frame.snapshot.rows);
      await new Promise(resolve => term.write(new Uint8Array(frame.keyframe), resolve));
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    }, capture);
    const actual = await page.evaluate(() => ({
      cursor: { row: term.buffer.active.cursorY, col: term.buffer.active.cursorX },
      cells: Array.from({ length: term.rows }, (_, row) => Array.from({ length: term.cols }, (_, col) => {
        const cell = term.buffer.active.getLine(term.buffer.active.baseY + row).getCell(col);
        return { text: cell.getChars(), width: cell.getWidth(),
          bold: !!cell.isBold(), inverse: !!cell.isInverse(),
          foreground: { mode: cell.isFgDefault() ? 'default' : cell.isFgRGB() ? 'rgb' : 'palette', value: cell.isFgDefault() ? null : cell.getFgColor() },
          background: { mode: cell.isBgDefault() ? 'default' : cell.isBgRGB() ? 'rgb' : 'palette', value: cell.isBgDefault() ? null : cell.getBgColor() } };
      })).flat(),
    }));
    assert.deepEqual(actual.cursor, { row: capture.snapshot.cursor.row, col: capture.snapshot.cursor.col });
    capture.snapshot.cells.forEach((cell, index) => {
      const expected = cell.glyph;
      assert.equal(actual.cells[index].text || ' ', expected.text || ' ', `cell ${index} text`);
      assert.equal(actual.cells[index].width, expected.width, `cell ${index} width`);
      assert.equal(actual.cells[index].bold, !!(cell.attributes.bits & 1), `cell ${index} bold`);
      assert.equal(actual.cells[index].inverse, !!(cell.attributes.bits & 16), `cell ${index} inverse`);
      const color = value => {
        if (value === 'Default') return { mode: 'default', value: null };
        if (value.Indexed) return { mode: 'palette', value: value.Indexed.index };
        if (value.Ansi) return { mode: 'palette', value: value.Ansi.index };
        if (value.BrightAnsi) return { mode: 'palette', value: value.BrightAnsi.index + 8 };
        if (value.Rgb) return { mode: 'rgb', value: (value.Rgb.red << 16) | (value.Rgb.green << 8) | value.Rgb.blue };
        throw new Error(`unhandled fixture color ${JSON.stringify(value)}`);
      };
      assert.deepEqual(actual.cells[index].foreground, color(cell.foreground), `cell ${index} foreground`);
      assert.deepEqual(actual.cells[index].background, color(cell.background), `cell ${index} background`);
    });
    await page.locator('#terminal').screenshot({ path: resolve(output, `${name}.png`) });
    await writeFile(resolve(output, `${name}.json`), JSON.stringify(capture, null, 2));
  }
  const menu = await until(capture => capture.alternate && text(capture).includes('/status 状态'));
  await render(menu, '01-menu');
  await request({ action: 'key', key: 'PageDown' });
  await request({ action: 'key', key: 'PageDown' });
  const applicationScrolled = await until(capture => !text(capture).includes('/help') && text(capture).includes('/command-'));
  await render(applicationScrolled, '01a-application-scroll');
  await request({ action: 'key', key: 'PageUp' });
  await request({ action: 'key', key: 'PageUp' });
  await render(await until(capture => text(capture).includes('/help')), '01a-return-to-top');
  const screen = await page.locator('.xterm-screen').boundingBox();
  await page.mouse.click(screen.x + screen.width / 80 * 5, screen.y + screen.height / 24 * 3.5);
  const clicked = await until(capture => text(capture).includes('> /settings'));
  await render(clicked, '01b-mouse-selection');
  await request({ action: 'key', key: 'Up' });
  await until(capture => text(capture).includes('> /status'));
  await request({ action: 'text', text: '/s' });
  const filtered = await until(capture => text(capture).includes('> /s') && !text(capture).includes('/help'));
  await render(filtered, '02-slash-filter');
  await request({ action: 'key', key: 'Down' });
  await until(capture => text(capture).includes('> /settings'));
  await request({ action: 'key', key: 'Up' });
  await until(capture => text(capture).includes('> /status'));
  await request({ action: 'key', key: 'Enter' });
  const selected = await until(capture => !capture.alternate && text(capture).includes('RESULT:/status 状态'));
  await render(selected, '03-selected');
  await pending;
  const inputsBeforeScroll = inputs.length;
  const before = await page.evaluate(() => term.buffer.active.viewportY);
  await page.evaluate(() => term.scrollLines(-10));
  const after = await page.evaluate(() => term.buffer.active.viewportY);
  assert.ok(after < before, 'viewport scroll reveals captured terminal history');
  await page.locator('#terminal').screenshot({ path: resolve(output, '04-history-scroll.png') });
  const visibleText = await page.evaluate(() => Array.from({ length: term.rows }, (_, row) =>
    term.buffer.active.getLine(term.buffer.active.viewportY + row)?.translateToString(true) ?? ''));
  await writeFile(resolve(output, '04-history-scroll.json'), JSON.stringify({
    source: selected,
    view: { viewport_top: after, source_history_start: selected.history_rows_total - selected.history_rows_included,
      visible_text: visibleText },
  }, null, 2));
  await pending;
  assert.equal(inputs.length, inputsBeforeScroll, 'viewport scroll must send no text/key request');
  assert.deepEqual(errors, []);
  await writeFile(resolve(output, 'result.json'), JSON.stringify({
    backend: 'rmux-0.10.0', real_tui: 'fzf', model_used: false,
    checks: ['atomic recovery grid/cursor equals xterm', 'application PageDown/PageUp', 'mouse selection through xterm input', 'slash filtering', 'arrow selection', 'enter result', 'viewport history scroll'],
    viewport_before: before, viewport_after: after, input_requests: inputs,
  }, null, 2));
  console.log(`PASS: real RMUX/fzf observation and interaction; evidence ${output}`);
} finally {
  closing = true;
  try { await closePreview({ browser, driver, exited: ended, pending, stderr: () => stderr }); }
  finally {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
}
