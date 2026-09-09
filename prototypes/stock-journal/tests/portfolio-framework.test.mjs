import assert from 'node:assert/strict';
import { readFile, writeFile, mkdtemp, rm, access } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { applyQuoteUpdates, portfolioReportBlocks, portfolioSnapshotSchema, portfolioView } from '../src/portfolio-framework.ts';

const raw = JSON.parse(await readFile(new URL('../src/portfolio-snapshot.json', import.meta.url), 'utf8'));
const fixture = () => portfolioSnapshotSchema.parse(raw);
const markdown = snapshot => portfolioReportBlocks(snapshot, '/next/portfolio-demo.html')
  .filter(block => block.kind === 'prose').map(block => block.payload.markdown).join('\n');

test('one quote update changes both the chart weights and native report values', () => {
  const before = fixture();
  const after = applyQuoteUpdates(before, [{ symbol: 'DEMO:PINE', quote: { ...before.holdings[0].quote, price: 50 } }]);
  const view = portfolioView(after);
  assert.equal(view.total, 204000);
  assert.equal(view.rows[0].value, 40000);
  assert.match(markdown(after), /50\.00 CNY/);
  assert.match(markdown(after), /19\.6%/);
  assert.equal(before.holdings[0].quote.price, 45);
});

test('missing quotes or FX cannot silently produce complete weights', () => {
  for (const missing of ['quote', 'fx']) {
    const snapshot = fixture();
    if (missing === 'quote') snapshot.holdings[0].quote = { ...snapshot.holdings[0].quote, price: null, asOf: null, status: 'unavailable' };
    else snapshot.holdings[0].quote.fxToBase = null;
    const view = portfolioView(snapshot);
    assert.equal(view.total, null);
    assert.ok(view.rows.every(row => row.weight === null));
    assert.match(markdown(snapshot), /部分行情或汇率不可用/);
  }
});

test('quote identity and ordering are explicit', () => {
  const snapshot = fixture();
  const quote = snapshot.holdings[0].quote;
  assert.throws(() => applyQuoteUpdates(snapshot, [{ symbol: 'UNRELATED.US', quote }]), /Unknown holding symbol/);
  assert.throws(() => applyQuoteUpdates(snapshot, [{ symbol: 'DEMO:PINE', quote }, { symbol: 'DEMO:PINE', quote }]), /Duplicate quote/);
  assert.throws(() => applyQuoteUpdates(snapshot, [{ symbol: 'DEMO:PINE', quote: { ...quote, asOf: '2020-01-01' } }]), /Out-of-order/);
});

test('stale data, missing events and unlinked stocks are visible', () => {
  const snapshot = fixture();
  snapshot.holdings[0].quote.status = 'stale';
  snapshot.holdings[0].trackId = null;
  snapshot.holdings[0].nextEvent = null;
  const report = markdown(snapshot);
  assert.match(report, /含过期行情/);
  assert.match(report, /旧行情/);
  assert.match(report, /暂无事件/);
  const holdings = portfolioReportBlocks(snapshot, '/next/portfolio-demo.html').find(block => block.id === 'holdings-heading');
  assert.doesNotMatch(holdings.payload.markdown, /neige:\/\/wave\/pine/);
});

test('empty snapshots have empty states and never fabricated transactions', () => {
  const snapshot = { ...fixture(), holdings: [], trades: [], history: [], cash: 0 };
  assert.equal(portfolioView(snapshot).total, 0);
  const report = markdown(snapshot);
  assert.match(report, /暂无持仓/);
  assert.match(report, /暂无交易记录/);
  assert.doesNotMatch(report, /Barra|因子暴露/);
});

test('snapshot validation rejects missing required fields and duplicate identities', () => {
  const snapshot = fixture();
  delete snapshot.cash;
  assert.equal(portfolioSnapshotSchema.safeParse(snapshot).success, false);
  const duplicate = fixture(); duplicate.holdings.push(duplicate.holdings[0]);
  assert.equal(portfolioSnapshotSchema.safeParse(duplicate).success, false);
});

test('fractional trades and escaped text survive report composition', () => {
  const snapshot = fixture();
  snapshot.trades[0].quantity = 2.5;
  snapshot.trades[0].reason = '[link](javascript:alert(1)) | note\nnext';
  assert.match(markdown(snapshot), /2\.5 股/);
  assert.match(markdown(snapshot), /\\\[link\\\]/);
  assert.match(markdown(snapshot), /\\\| note next/);
});

test('quote import persists a valid update and rejects duplicates without changing data', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'neige-portfolio-test-'));
  try {
    const snapshot = join(directory, 'snapshot.json');
    const quotes = join(directory, 'quotes.json');
    await writeFile(snapshot, JSON.stringify(raw));
    const update = { symbol: 'DEMO:PINE', quote: { ...raw.holdings[0].quote, price: 50 } };
    await writeFile(quotes, JSON.stringify([update]));
    const script = new URL('../scripts/update-quotes.mjs', import.meta.url);
    execFileSync(process.execPath, ['--experimental-strip-types', script.pathname, quotes, snapshot]);
    const saved = await readFile(snapshot, 'utf8');
    assert.equal(JSON.parse(saved).holdings[0].quote.price, 50);
    await writeFile(quotes, JSON.stringify([update, update]));
    assert.throws(() => execFileSync(process.execPath, ['--experimental-strip-types', script.pathname, quotes, snapshot], { stdio: 'pipe' }));
    assert.equal(await readFile(snapshot, 'utf8'), saved);
    await assert.rejects(access(`${snapshot}.lock`));
  } finally { await rm(directory, { recursive: true, force: true }); }
});
