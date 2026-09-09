import test from 'node:test';
import assert from 'node:assert/strict';
import { readMarketPortfolio } from '../src/market-adapter.ts';
import { portfolioView, portfolioReportBlocks } from '../src/portfolio-framework.ts';
import { marketFixture } from './market-fixture.mjs';
import { metadataIsMissing } from '../src/portfolio-metadata.ts';

const metadata = { assets: { 'US:AAA': { name: '美股研究', trackId: 'research-us', nextEvent: { date: '2026-10-01', title: '季度复盘' } } }, trades: [] };
const read = fixture => readMarketPortfolio('market-track', fixture.overlays, metadata);

test('uses producer-converted values, metadata and native Track links', () => {
  const fixture = marketFixture();
  fixture.holdings.rows[0].value = 700.01;
  const result = read(fixture);
  assert.equal(result.kind, 'ready');
  const view = portfolioView(result.snapshot);
  assert.equal(view.rows[0].value, 700.01);
  assert.equal(view.total, 880);
  assert.equal(view.rows[0].quote.currency, 'USD');
  assert.equal(view.rows[0].dayChange, null);
  assert.equal(view.rows[1].trackId, null);
  assert.equal(result.snapshot.cash, 0);
  const block = portfolioReportBlocks(result.snapshot, '/next/portfolio-demo.html').find(block => block.id === 'holdings-heading');
  assert.match(block.payload.markdown, /neige:\/\/wave\/research-us/);
  assert.match(block.payload.markdown, /季度复盘/);
  assert.match(block.payload.markdown, /Converted at/);
});

test('partial valuations never become a full allocation chart', () => {
  const fixture = marketFixture();
  fixture.holdings.rows[1].value = null;
  fixture.holdings.rows[1].rate = null;
  fixture.holdings.rows[2].value = 700;
  const result = read(fixture);
  assert.equal(result.kind, 'ready');
  assert.equal(portfolioView(result.snapshot).total, null);
  assert.ok(portfolioView(result.snapshot).rows.every(item => item.weight === null));
});

test('foreign and unknown currency history points remain gaps', () => {
  const result = read(marketFixture());
  assert.equal(result.kind, 'ready');
  assert.deepEqual(portfolioView(result.snapshot).history.map(item => item.value), [null, 800, null, 850, 880]);
  assert.ok(result.snapshot.notes.some(note => note.includes('2 个历史点')));
});

test('only the requested Track and exact market plugin can supply data', () => {
  const fixture = marketFixture('another-track');
  assert.equal(read(fixture).kind, 'waiting');
  fixture.overlays.forEach(item => { item.entity_id = 'market-track'; item.plugin_id = 'unrelated'; });
  assert.equal(read(fixture).kind, 'waiting');
});

test('duplicate identities, unknown total units and invalid rows are refused', () => {
  const duplicate = marketFixture(); duplicate.holdings.rows.splice(1, 0, { ...duplicate.holdings.rows[0] });
  assert.equal(read(duplicate).kind, 'invalid');
  const currency = marketFixture(); currency.holdings.rows[2].currency = null;
  assert.equal(read(currency).kind, 'invalid');
  const invalid = marketFixture(); invalid.holdings.rows[0].currency = null;
  assert.equal(read(invalid).kind, 'invalid');
  const inconsistent = marketFixture(); inconsistent.holdings.rows[2].value = 10;
  assert.equal(read(inconsistent).kind, 'invalid');
});

test('old overlays remain labeled stale and missing history stays empty', () => {
  const fixture = marketFixture('market-track', 1000); fixture.overlays.pop();
  const result = readMarketPortfolio('market-track', fixture.overlays, metadata, 200000);
  assert.equal(result.kind, 'ready');
  assert.equal(portfolioView(result.snapshot).stale, true);
  assert.deepEqual(result.snapshot.history, []);
});

test('an empty plugin portfolio is an empty state rather than an invented currency', () => {
  const fixture = marketFixture();
  fixture.holdings.rows = [{ ...fixture.holdings.rows[2], currency: null, value: 0 }];
  const result = read(fixture);
  assert.equal(result.kind, 'waiting');
  assert.match(result.message, /暂未登记持仓/);
});

test('metadata absence follows the real file-route contract, not generic read failures', () => {
  const missing = { status: 400, statusText: 'Bad Request', body: { code: 'bad_request', error: 'path /workspace/.neige-portfolio/metadata.json not found' } };
  assert.equal(metadataIsMissing(missing, '/workspace'), true);
  for (const status of [401, 403, 404, 500]) assert.equal(metadataIsMissing({ ...missing, status }, '/workspace'), false);
  assert.equal(metadataIsMissing(missing, '/another-workspace'), false);
  assert.equal(metadataIsMissing({ ...missing, body: { code: 'bad_request', error: 'path /workspace not found' } }, '/workspace'), false);
  assert.equal(metadataIsMissing({ ...missing, body: { code: 'bad_request', error: 'path resolves outside track workspace' } }, '/workspace'), false);
});
