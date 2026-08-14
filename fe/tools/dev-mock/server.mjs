// @ts-nocheck -- Reason: dev-only Vite middleware; it models untyped kernel wire
// payloads directly and ships in no build. Same treatment as repository-check.mjs.
/**
 * Dev-only in-memory kernel, for looking at the design system without running
 * the `make dev` stack. Enabled by `FE_DEV_MOCK=1`; it is a Vite middleware, so
 * nothing here ships in a build and nothing under `web/src` knows it exists.
 *
 * The scenario is chosen to exercise the states the spec actually legislates —
 * waves that are waiting, waves that are running, several coves so the eight
 * identity slots are visible, and enough history that RECENT is not empty. A
 * fixture where everything is `done` would render a page with no hierarchy at
 * all and prove nothing.
 */

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const now = Date.now();

export const devMockCoves = [
  { id: 'cove-atlas', name: 'atlas', color: '#5B8DEF', sort: 0, kind: 'user' },
  { id: 'cove-ledger', name: 'ledger-service', color: '#8B7FE8', sort: 1, kind: 'user' },
  { id: 'cove-notes', name: 'field-notes', color: '#7FC8A9', sort: 2, kind: 'user' },
].map((cove) => ({ ...cove, created_at: now - 40 * DAY, updated_at: now - 2 * HOUR }));

const cwdOf = { 'cove-atlas': '/srv/atlas', 'cove-ledger': '/srv/ledger', 'cove-notes': '/home/kenji/notes' };

const waveSeed = [
  ['w-1', 'cove-atlas', 'Reference resolver: this round of fixes', 'blocked', 40 * 60_000, true],
  ['w-2', 'cove-atlas', 'Referenced side: valuation conclusion', 'reviewing', 2 * HOUR, false],
  ['w-3', 'cove-atlas', 'Backfill the projection table', 'working', 11 * 60_000, false],
  ['w-4', 'cove-atlas', 'Drop the legacy sort column', 'done', 3 * DAY, false],
  ['w-5', 'cove-ledger', 'Reconcile the August close', 'failed', 5 * HOUR, false],
  ['w-6', 'cove-ledger', 'Split the settlement adapter', 'working', 3 * 60_000, false],
  ['w-7', 'cove-ledger', 'Retire the v1 webhook', 'draft', 9 * DAY, false],
  ['w-8', 'cove-notes', 'Weekly review: what shipped', 'done', 26 * HOUR, false],
  ['w-9', 'cove-notes', 'Reading list triage', 'canceled', 12 * DAY, false],
  ['w-10', 'cove-notes', '', 'planning', 90_000, false],
];

export const devMockWaves = waveSeed.map(([id, coveId, title, lifecycle, age, pinned], index) => ({
  id, cove_id: coveId, title, sort: index, lifecycle,
  cwd: cwdOf[coveId],
  archived_at: null,
  pinned_at: pinned ? now - 3 * DAY : null,
  terminal_at: null,
  created_at: now - 20 * DAY,
  updated_at: now - age,
}));

// `any_card_needs_input` is the one overlay the kernel really writes, so it is
// the only one seeded: `progress` / `eta` / `now` have no writer in production
// and the design forbids reserving space for them (§6.3).
export const devMockOverlays = [{
  id: 'ov-1', plugin_id: 'card-fsm', entity_kind: 'wave', entity_id: 'w-6',
  kind: 'any_card_needs_input', payload: { value: true }, updated_at: now - 60_000,
}];

// A deterministic candle series. Dev fixtures must look the same on every
// reload — a chart that redraws differently each time is impossible to review
// a layout against, and impossible to screenshot twice.
function candleSeries(startMs, days, startPrice) {
  const candles = [];
  let price = startPrice;
  for (let day = 0; day < days; day += 1) {
    const drift = Math.sin(day / 9) * 1.6 + Math.sin(day / 2.3) * 0.7 + (day % 7 === 0 ? -1.2 : 0.25);
    const open = price;
    const close = Math.max(1, open + drift);
    const high = Math.max(open, close) + Math.abs(drift) * 0.6;
    const low = Math.min(open, close) - Math.abs(drift) * 0.5;
    const volume = 900_000 + Math.abs(drift) * 620_000 + (day % 5) * 90_000;
    candles.push([
      startMs + day * DAY,
      Number(open.toFixed(2)), Number(high.toFixed(2)),
      Number(low.toFixed(2)), Number(close.toFixed(2)), Math.round(volume),
    ]);
    price = close;
  }
  return candles;
}

// A written report, in the shape the kernel persists (`WaveReportPayload`).
//
// Since schema v2 the authoritative layout is `blocks[]` and `body` is its
// flat projection; both are seeded, and they agree, because that is the
// invariant the kernel maintains and the frontend reads `blocks` first. The
// fixture carries **one block of every kind this build knows plus one it does
// not**, because the states worth being able to look at are exactly the ones
// the spec legislates — including the degraded one.
const w1Body = [
    '# What this wave is for',
    '',
    'Reference resolution was walking the card graph on every read. On a workspace',
    'with a few thousand cards that is a hundred milliseconds a keystroke, and it',
    'got worse the longer you used it. The fix is a frozen reverse index built once',
    'per projection and invalidated by the same events that invalidate the cards.',
    '',
    '# What changed',
    '',
    '- `resolve_reference` reads the index instead of walking; the walk is gone, not',
    '  kept as a fallback, because a fallback that never runs is a fallback nobody',
    '  notices has broken.',
    '- The index is built inside the projection transaction, so it cannot observe a',
    '  half-applied batch.',
    '- Invalidation is keyed on `card.updated_at`, not on a manual dirty flag.',
    '',
    'The measured read path on the 4k-card fixture:',
    '',
    '| step | before | after |',
    '| --- | --- | --- |',
    '| resolve one reference | 84ms | 0.3ms |',
    '| rebuild after one edit | — | 11ms |',
    '| cold projection | 260ms | 271ms |',
    '',
    'The cold path pays 11ms for the index. That is the whole cost.',
    '',
    '# Still open',
    '',
    'Two call sites reach the graph directly and do not go through the resolver:',
    '',
    '```rust',
    '// crates/calm-truth/src/projection.rs:412',
    'let target = cards.iter().find(|c| c.id == reference.target_id);',
    '```',
    '',
    'Both are on the ingest path, where the index does not exist yet. Neither is',
    'wrong today, but the moment ingest starts reading references they become the',
    'two places the frozen index is silently not in force.',
    '',
    '> Left alone deliberately. Making them go through the resolver means building',
    '> the index during ingest, which is a different wave.',
    '',
    '- [x] Frozen reverse index',
    '- [x] Invalidation keyed on the projection',
    '- [ ] Ingest-path call sites',
  ].join('\n');

const w1Report = {
  schemaVersion: 3,
  docRev: 7,
  summary: 'Reference resolution now runs off the frozen index; two call sites still bypass it.',
  body: w1Body,
  blocks: [
    // The prose block stops where the benchmark table starts: in the block
    // model that table is a `table` block, and leaving the Markdown copy in the
    // prose would render it twice. `body` keeps both, because `body` is the
    // flat projection of the blocks, not a second document.
    { id: 'b-goal', kind: 'prose', rev: 4, payload: { markdown: w1Body.split('\nThe measured read path')[0] } },
    {
      id: 'b-bench', kind: 'table', rev: 2,
      payload: {
        caption: 'Read path on the 4k-card fixture',
        highlight: 'resolve one reference',
        columns: [
          { key: 'step', label: 'Step' },
          { key: 'before', label: 'Before', align: 'right' },
          { key: 'after', label: 'After', align: 'right' },
        ],
        rows: [
          { step: 'resolve one reference', before: '84.0', after: '0.3' },
          { step: 'rebuild after one edit', before: null, after: '11.0' },
          { step: 'cold projection', before: '260.0', after: '271.0' },
        ],
      },
    },
    {
      id: 'b-open', kind: 'prose', rev: 5,
      payload: {
        markdown: [
          '# Still open',
          '',
          'Two call sites reach the graph directly and do not go through the resolver.',
          'The valuation this hangs off is in [the referenced side](neige://wave/w-2#b-thesis),',
          'and its comparables table is [here](neige://wave/w-2#b-comps).',
          '',
          '```rust',
          '// crates/calm-truth/src/projection.rs:412',
          'let target = cards.iter().find(|c| c.id == reference.target_id);',
          '```',
          '',
          '> Left alone deliberately. Making them go through the resolver means building',
          '> the index during ingest, which is a different wave.',
        ].join('\n'),
      },
    },
    {
      id: 'b-task-ingest', kind: 'task', rev: 1,
      payload: {
        key: 'ingest-resolver', kind: 'codex', declared_by: 'spec', ready: true,
        goal: 'Route the two ingest-path call sites through the resolver, building the index during ingest.',
        acceptance: 'No direct cards.iter().find on a reference id anywhere under crates/calm-truth.',
        gate: {
          steps: [
            { name: 'fmt', cmd: 'cargo fmt --check' },
            { name: 'clippy', cmd: 'cargo clippy --all-targets -- -D warnings' },
            { name: 'test', cmd: 'cargo test -p calm-truth' },
          ],
        },
      },
    },
    {
      id: 'b-task-dropped', kind: 'task', rev: 2,
      payload: {
        key: 'walk-fallback', declared_by: 'spec', tombstoned_by: 'user',
        tombstone: { reason: 'A fallback that never runs is a fallback nobody notices has broken.' },
      },
    },
    {
      id: 'b-app', kind: 'app', rev: 1,
      payload: { src: '/dev-mock/app.html', title: 'Index rebuild timeline', height: 168 },
    },
    // The block this build cannot draw. It is in the fixture on purpose: the
    // degraded state is the one that decides whether the block model is safe to
    // ship, and it is the only state that cannot be produced on demand later.
    { id: 'b-sankey', kind: 'chart.sankey', rev: 1, payload: { nodes: [], links: [] } },
  ],
};

const w2Body = [
  '# Valuation conclusion',
  '',
  'We hold the discount rate at 9.4%. It is not copied from a screen: it is the',
  'cost of equity implied by the comparables below, rounded up by 40bp for the',
  'concentration in a single distribution channel.',
  '',
  '# How the rate is taken',
  '',
  'Three years of daily closes, and the volatility that comes out of them.',
].join('\n');

const w2Report = {
  schemaVersion: 3,
  docRev: 3,
  summary: 'Discount rate held at 9.4%; the concentration premium is the only judgement call.',
  body: w2Body,
  blocks: [
    { id: 'b-thesis', kind: 'prose', rev: 3, payload: { markdown: w2Body.split('\n# How the rate')[0] } },
    {
      id: 'b-comps', kind: 'table', rev: 2,
      payload: {
        caption: 'Comparables, trailing twelve months',
        highlight: '600519.SH',
        columns: [
          { key: 'name', label: 'Company' },
          { key: 'pe', label: 'P/E', align: 'right' },
          { key: 'margin', label: 'Op margin', align: 'right' },
          { key: 'beta', label: 'Beta', align: 'right' },
        ],
        rows: [
          { name: '600519.SH', pe: '28.4', margin: '67.1%', beta: '0.82' },
          { name: '000858.SZ', pe: '21.9', margin: '52.4%', beta: '0.94' },
          { name: '600809.SH', pe: '18.2', margin: '48.0%', beta: '1.05' },
          { name: '000568.SZ', pe: '19.7', margin: '50.6%', beta: '0.98' },
        ],
      },
    },
    { id: 'b-method', kind: 'prose', rev: 1, payload: { markdown: '# How the rate is taken\n\nThree years of daily closes, and the volatility that comes out of them.' } },
    {
      id: 'b-chart', kind: 'chart.candles', rev: 6,
      payload: {
        symbol: '600519.SH', period: 'day', overlays: ['ma20', 'ma60'],
        caption: 'Daily closes. The gap in March is the ex-dividend date, not a data hole.',
        candles: candleSeries(now - 220 * DAY, 220, 168),
      },
    },
  ],
};

export const devMockCards = {
  'w-1': [
    { id: 'c-1', wave_id: 'w-1', kind: 'wave-report', sort: 0, payload: w1Report, deletable: false },
    { id: 'c-2', wave_id: 'w-1', kind: 'codex', title: 'agent', sort: 1, payload: {}, deletable: true },
  ],
  'w-2': [{ id: 'c-3', wave_id: 'w-2', kind: 'wave-report', sort: 0, payload: w2Report, deletable: false }],
};

// Backlinks are what *other* waves wrote, so the fixture derives them from the
// links actually present in w-1's report rather than declaring them separately:
// a fixture whose backlinks disagree with its documents would be demonstrating
// a state the kernel cannot produce.
const backlinks = {
  'w-2': {
    truncated: false,
    skipped_sources: 0,
    backlinks: [
      {
        src_wave_id: 'w-1', src_wave_title: 'Reference resolver: this round of fixes',
        src_block_id: 'b-open', dst_block_id: 'b-thesis', label: 'the referenced side',
        quote: {
          before: 'The valuation this hangs off is in ', label: 'the referenced side',
          after: ', and its comparables table is here.', head_elided: true, tail_elided: false,
        },
        updated_at: now - HOUR,
      },
      {
        src_wave_id: 'w-1', src_wave_title: 'Reference resolver: this round of fixes',
        src_block_id: 'b-open', dst_block_id: 'b-comps', label: 'here',
        quote: {
          before: '…and its comparables table is ', label: 'here',
          after: '.', head_elided: true, tail_elided: false,
        },
        updated_at: now - HOUR,
      },
      // A second citing wave, mentioning this one from two different blocks —
      // the case the row's count exists for. One wave citing you twice from
      // *one* block (above) is one mention; twice from two blocks is two.
      {
        src_wave_id: 'w-3', src_wave_title: 'Backfill the projection table',
        src_block_id: 'b-plan', dst_block_id: 'b-comps', label: 'the comparables',
        quote: {
          before: 'Numbers come from ', label: 'the comparables',
          after: ' rather than from the screen.', head_elided: false, tail_elided: true,
        },
        updated_at: now - 3 * HOUR,
      },
      {
        src_wave_id: 'w-3', src_wave_title: 'Backfill the projection table',
        src_block_id: 'b-risks', dst_block_id: null, label: 'the valuation wave',
        quote: {
          before: 'If ', label: 'the valuation wave',
          after: ' moves the rate, this backfill has to run again.', head_elided: true, tail_elided: false,
        },
        updated_at: now - 3 * HOUR,
      },
    ],
  },
};

const EMPTY_BACKLINKS = { backlinks: [], truncated: false, skipped_sources: 0 };

// A same-origin page for the `app` block to embed. It is served by this
// middleware rather than dropped into `web/public` so the demo adds no tracked
// asset to the app itself.
//
// **It is styled like the document that embeds it**, and that is not cheating:
// an app authored for this product reads the host's theme (that is what the
// legacy card host's `ui/initialize` + theme push is for). A fixture in a
// different type stack would have been demonstrating a page nobody wrote.
const DEV_MOCK_APP_HTML = `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<style>
  /* html as well as body: a transparent body still leaves the frame's own
     canvas painted the UA default white, which is a white patch inside a
     tinted frame, and a glaring one in dark mode. */
  html, body { background: transparent; }
  :root { color-scheme: light dark; }
  body {
    margin: 0; padding: 18px 20px;
    font: 15px/1.5 ui-serif, "New York", Georgia, serif;
    color: light-dark(oklch(38% 0.01 250), oklch(80% 0.01 245));
  }
  ol { margin: 0; padding-left: 20px; }
  li { margin-block: 6px; }
  b { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; font-weight: 400;
      color: light-dark(oklch(45% 0.10 267), oklch(80% 0.11 267)); }
</style></head>
<body>
  <ol>
    <li><b>t+0ms</b> projection transaction opens</li>
    <li><b>t+11ms</b> frozen reverse index built, in-transaction</li>
    <li><b>t+11ms</b> commit; readers see index and cards together</li>
    <li><b>on edit</b> invalidation keyed on <b>card.updated_at</b></li>
  </ol>
</body></html>`;

const settings = { http_proxy: 'http://127.0.0.1:2080', https_proxy: 'http://127.0.0.1:2080' };

const coves = devMockCoves;
const waves = devMockWaves;
const overlays = devMockOverlays;
const cards = devMockCards;

export const DEV_MOCK_ROUTES = Object.freeze([
  ['GET', '/api/version'], ['GET', '/api/settings'], ['PUT', '/api/settings'],
  ['GET', '/api/overlays'], ['GET', '/api/coves'], ['POST', '/api/coves'],
  ['GET', '/api/coves/{cove_id}/waves'], ['PATCH', '/api/coves/{id}'], ['DELETE', '/api/coves/{id}'],
  ['GET', '/api/waves'], ['POST', '/api/waves'], ['GET', '/api/waves/{id}'], ['PATCH', '/api/waves/{id}'], ['DELETE', '/api/waves/{id}'],
  ['GET', '/api/waves/{id}/backlinks'],
].map((route) => Object.freeze(route)));

function send(res, status, body) {
  res.statusCode = status;
  res.setHeader('content-type', 'application/json');
  res.end(body === undefined ? '' : JSON.stringify(body));
}

function readBody(req) {
  return new Promise((resolve) => {
    let raw = '';
    req.on('data', (chunk) => { raw += chunk; });
    req.on('end', () => { try { resolve(JSON.parse(raw || '{}')); } catch { resolve({}); } });
  });
}

export function devMockApi() {
  return {
    name: 'neige-dev-mock-api',
    configureServer(server) {
      server.middlewares.use(handleDevMockRequest);
    },
  };
}

export async function handleDevMockRequest(req, res, next) {
        const url = new URL(req.url ?? '/', 'http://mock');
        const path = url.pathname;
        if (path === '/dev-mock/app.html') {
          res.statusCode = 200;
          res.setHeader('content-type', 'text/html; charset=utf-8');
          return res.end(DEV_MOCK_APP_HTML);
        }
        if (!path.startsWith('/api/')) return next();
        const method = req.method ?? 'GET';

        if (path === '/api/auth/whoami') {
          return send(res, 200, { userId: 'u-dev', displayName: 'Kenji Xie', role: 'owner', sessionId: 's-dev' });
        }
        if (path === '/api/auth/logout') return send(res, 204, undefined);
        if (path === '/api/version' && method === 'GET') {
          return send(res, 200, {
            webCompatVersion: 1, minWebCompatVersion: 1, syncEventVersion: 1, dbInstanceId: 'mock',
          });
        }
        if (path === '/api/settings' && (method === 'GET' || method === 'PUT')) {
          if (method === 'PUT') {
            const body = await readBody(req);
            for (const [key, value] of Object.entries(body.settings ?? {})) {
              if (value === null || value === '') delete settings[key];
              else settings[key] = value;
            }
          }
          return send(res, 200, { settings });
        }
        if (path === '/api/overlays' && method === 'GET') return send(res, 200, overlays);
        if (path === '/api/coves' && method === 'GET') return send(res, 200, coves);
        if (path === '/api/coves' && method === 'POST') {
          const body = await readBody(req);
          if (typeof body.name !== 'string' || typeof body.color !== 'string') return send(res, 400, { message: 'name and color are required' });
          const cove = {
            id: `cove-${Math.random().toString(36).slice(2, 8)}`, name: body.name, color: body.color,
            sort: coves.length, kind: 'user', created_at: Date.now(), updated_at: Date.now(),
          };
          coves.push(cove);
          return send(res, 201, cove);
        }

        const coveWaves = /^\/api\/coves\/([^/]+)\/waves$/.exec(path);
        if (coveWaves && method === 'GET') {
          return send(res, 200, waves.filter((wave) => wave.cove_id === decodeURIComponent(coveWaves[1])));
        }
        const coveId = /^\/api\/coves\/([^/]+)$/.exec(path);
        if (coveId && (method === 'PATCH' || method === 'DELETE')) {
          const id = decodeURIComponent(coveId[1]);
          const index = coves.findIndex((cove) => cove.id === id);
          if (index < 0) return send(res, 404, { message: 'no such cove' });
          if (method === 'DELETE') {
            coves.splice(index, 1);
            for (let i = waves.length - 1; i >= 0; i -= 1) if (waves[i].cove_id === id) waves.splice(i, 1);
            return send(res, 204, undefined);
          }
          const body = await readBody(req);
          Object.assign(coves[index], body, { updated_at: Date.now() });
          return send(res, 200, coves[index]);
        }

        if (path === '/api/waves' && method === 'GET') return send(res, 200, waves);
        if (path === '/api/waves' && method === 'POST') {
          const body = await readBody(req);
          if (typeof body.cove_id !== 'string' || typeof body.title !== 'string') return send(res, 400, { message: 'cove_id and title are required' });
          const wave = {
            id: `w-${Math.random().toString(36).slice(2, 8)}`, cove_id: body.cove_id, title: body.title,
            sort: waves.length, lifecycle: 'draft', cwd: body.cwd ?? '', archived_at: null, pinned_at: null,
            terminal_at: null, created_at: Date.now(), updated_at: Date.now(),
          };
          waves.push(wave);
          return send(res, 201, wave);
        }
        const waveBacklinks = /^\/api\/waves\/([^/]+)\/backlinks$/.exec(path);
        if (waveBacklinks && method === 'GET') {
          const id = decodeURIComponent(waveBacklinks[1]);
          return send(res, 200, backlinks[id] ?? EMPTY_BACKLINKS);
        }
        const waveId = /^\/api\/waves\/([^/]+)$/.exec(path);
        if (waveId && (method === 'GET' || method === 'PATCH' || method === 'DELETE')) {
          const id = decodeURIComponent(waveId[1]);
          const index = waves.findIndex((wave) => wave.id === id);
          if (index < 0) return send(res, 404, { message: 'no such wave' });
          if (method === 'DELETE') { waves.splice(index, 1); return send(res, 204, undefined); }
          if (method === 'PATCH') {
            const body = await readBody(req);
            Object.assign(waves[index], body, { updated_at: Date.now() });
            return send(res, 200, waves[index]);
          }
          return send(res, 200, {
            wave: waves[index],
            cards: (cards[id] ?? []).map((card) => ({ ...card, created_at: now - DAY, updated_at: now - HOUR })),
            overlays: overlays.filter((overlay) => overlay.entity_id === id),
          });
        }

        return send(res, 404, { message: `dev mock has no route for ${method} ${path}` });
}
