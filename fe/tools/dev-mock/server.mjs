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

const coves = [
  { id: 'cove-atlas', name: 'atlas', color: '#5B8DEF', sort: 0, kind: 'user' },
  { id: 'cove-ledger', name: 'ledger-service', color: '#8B7FE8', sort: 1, kind: 'user' },
  { id: 'cove-notes', name: 'field-notes', color: '#7FC8A9', sort: 2, kind: 'user' },
].map((cove) => ({ ...cove, created_at: now - 40 * DAY, updated_at: now - 2 * HOUR }));

const cwdOf = { 'cove-atlas': '/srv/atlas', 'cove-ledger': '/srv/ledger', 'cove-notes': '/home/kenji/notes' };

// The folders each cove has claimed, which is what the New wave dialog keys its
// shape on. Two for atlas (so the `Folder` select is reachable), one for ledger
// (so the form is just the task), none for field-notes (so the claim-the-first-
// folder branch is reachable too).
const folderSeed = {
  'cove-atlas': ['/srv/atlas', '/srv/atlas-docs'],
  'cove-ledger': ['/srv/ledger'],
  'cove-notes': [],
};

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

const waves = waveSeed.map(([id, coveId, title, lifecycle, age, pinned], index) => ({
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
const overlays = [{
  id: 'ov-1', plugin_id: 'card-fsm', entity_kind: 'wave', entity_id: 'w-6',
  kind: 'any_card_needs_input', payload: { value: true }, updated_at: now - 60_000,
}];

// A written report, in the shape the kernel persists (`WaveReportPayload`:
// schemaVersion / docRev / summary / body, body being Markdown whose H1s the
// frontend splits into sections). w-2 keeps an empty payload on purpose — the
// empty state is a state worth being able to look at.
const w1Report = {
  schemaVersion: 3,
  docRev: 7,
  summary: 'Reference resolution now runs off the frozen index; two call sites still bypass it.',
  body: [
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
  ].join('\n'),
};

const cards = {
  'w-1': [
    { id: 'c-1', wave_id: 'w-1', kind: 'wave-report', title: null, sort: 0, payload: w1Report, deletable: false },
    { id: 'c-2', wave_id: 'w-1', kind: 'codex', title: 'agent', sort: 1, payload: {}, deletable: true },
  ],
  'w-2': [{ id: 'c-3', wave_id: 'w-2', kind: 'wave-report', title: null, sort: 0, payload: {}, deletable: false }],
};

const settings = { http_proxy: 'http://127.0.0.1:2080', https_proxy: 'http://127.0.0.1:2080' };

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
      server.middlewares.use(async (req, res, next) => {
        const url = new URL(req.url ?? '/', 'http://mock');
        const path = url.pathname;
        if (!path.startsWith('/api/')) return next();
        const method = req.method ?? 'GET';

        if (path === '/api/auth/whoami') {
          return send(res, 200, { userId: 'u-dev', displayName: 'Kenji Xie', role: 'owner', sessionId: 's-dev' });
        }
        if (path === '/api/auth/logout') return send(res, 204, undefined);
        if (path === '/api/version') {
          return send(res, 200, {
            webCompatVersion: 1, minWebCompatVersion: 1, syncEventVersion: 1, dbInstanceId: 'mock',
          });
        }
        if (path === '/api/settings') {
          if (method === 'PUT') {
            const body = await readBody(req);
            for (const [key, value] of Object.entries(body.settings ?? {})) {
              if (value === null || value === '') delete settings[key];
              else settings[key] = value;
            }
          }
          return send(res, 200, { settings });
        }
        if (path === '/api/overlays') return send(res, 200, overlays);
        if (path === '/api/coves' && method === 'GET') return send(res, 200, coves);
        if (path === '/api/coves' && method === 'POST') {
          const body = await readBody(req);
          const cove = {
            id: `cove-${Math.random().toString(36).slice(2, 8)}`, name: body.name, color: body.color,
            sort: coves.length, kind: 'user', created_at: Date.now(), updated_at: Date.now(),
          };
          coves.push(cove);
          return send(res, 200, cove);
        }

        // The New wave dialog reads this to decide whether it has to ask for a
        // path at all. `cove-notes` is deliberately left with no folder so the
        // zero-folder branch (claim the cove's first folder) is reachable in the
        // mock, and `cove-atlas` has two so the `Folder` select is too.
        const coveFolders = /^\/api\/coves\/([^/]+)\/folders$/.exec(path);
        if (coveFolders && method === 'GET') {
          const id = decodeURIComponent(coveFolders[1]);
          const paths = folderSeed[id] ?? [];
          return send(res, 200, paths.map((folderPath, index) => ({
            id: index + 1, cove_id: id, path: folderPath,
            repo_identity: null, repo_identity_probed_at: null, created_at: now - 30 * DAY,
          })));
        }

        const coveWaves = /^\/api\/coves\/([^/]+)\/waves$/.exec(path);
        if (coveWaves) {
          return send(res, 200, waves.filter((wave) => wave.cove_id === decodeURIComponent(coveWaves[1])));
        }
        const coveId = /^\/api\/coves\/([^/]+)$/.exec(path);
        if (coveId) {
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

        if (path === '/api/waves' && method === 'POST') {
          const body = await readBody(req);
          const wave = {
            id: `w-${Math.random().toString(36).slice(2, 8)}`, cove_id: body.cove_id, title: body.title,
            sort: waves.length, lifecycle: 'draft', cwd: body.cwd ?? '', archived_at: null, pinned_at: null,
            terminal_at: null, created_at: Date.now(), updated_at: Date.now(),
          };
          waves.push(wave);
          return send(res, 200, wave);
        }
        const waveId = /^\/api\/waves\/([^/]+)$/.exec(path);
        if (waveId) {
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
      });
    },
  };
}
