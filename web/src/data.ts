import type { Cove, Wave, TermLine, GitCommit, DiffHunk } from './types';

// Card factories.
const term = (title: string, lines: TermLine[]) =>
  ({ type: 'terminal' as const, title, lines });
const doc = (title: string, body: string) =>
  ({ type: 'doc' as const, title, body });
const git = (branch: string, commits: GitCommit[]) =>
  ({ type: 'git' as const, branch, commits });
const diff = (file: string, added: number, removed: number, hunks: DiffHunk[]) =>
  ({ type: 'diff' as const, file, added, removed, hunks });

export const coves: Cove[] = [
  { id: 'atlas',   name: 'Atlas',     subtitle: 'Personal site',         color: 'oklch(58% 0.16 235)' },
  { id: 'compass', name: 'Compass',   subtitle: 'Habit tracker iOS app', color: 'oklch(62% 0.14 195)' },
  { id: 'reef',    name: 'Reef',      subtitle: 'Client · e-commerce',   color: 'oklch(60% 0.15 25)'  },
  { id: 'beacon',  name: 'Beacon',    subtitle: 'Side project',          color: 'oklch(70% 0.12 80)'  },
  { id: 'tide',    name: 'Tide Pool', subtitle: 'OSS CLI',               color: 'oklch(64% 0.13 170)' },
];

export const waves: Wave[] = [
  {
    id: 'w-001', coveId: 'atlas', title: 'Migrate the site to Astro',
    status: 'running', progress: 0.62, eta: '~ 40 min left',
    now: 'Migrating the OG image generator to read directly from the content collection.',
    plan: [
      { label: 'Move /pages to content collections', done: true, when: '3h ago' },
      { label: 'Type schema for blog frontmatter',   done: true, when: '2h ago' },
      { label: 'Migrate the OG image generator',     cur:  true, when: 'now'   },
      { label: 'Sweep through links + redirects' },
      { label: 'Update sitemap + RSS' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log',  text: 'Refactored generateOG() to accept a CollectionEntry.' },
        { kind: 'edit', text: 'src/lib/og.ts  +24 −41' },
        { kind: 'log',  text: 'Hit a circular import — moving schema into the integration hook.' },
        { kind: 'edit', text: 'astro.config.mjs  +18 −9' },
        { kind: 'cmd',  text: '$ rm public/pages.json' },
      ]),
      term('npm run build', [
        { kind: 'cmd',  text: '$ npm run build' },
        { kind: 'out',  text: 'astro v4.5.6 starting up…' },
        { kind: 'out',  text: 'generating 47 OG images…' },
        { kind: 'pass', text: '✓  47 OG images regenerated (12.3s)' },
        { kind: 'pass', text: '✓  build complete (18.4s)' },
      ]),
      diff('src/lib/og.ts', 24, 41, [
        {
          header: '@@ -8,9 +8,5 @@ export async function generateOG(',
          lines: [
            { kind: 'ctx', text: "  const fontPath = path.join(root, 'fonts/Inter.woff')" },
            { kind: 'rm',  text: '  const entries = await readPagesManifest()' },
            { kind: 'rm',  text: '  for (const e of entries) {' },
            { kind: 'rm',  text: '    if (!e.frontmatter.og) continue' },
            { kind: 'add', text: '  if (!entry.data.og) return' },
            { kind: 'ctx', text: '  const buf = await renderImage(entry, fontPath)' },
          ],
        },
      ]),
    ],
  },

  {
    id: 'w-010', coveId: 'compass', title: 'Onboarding flow v2',
    status: 'running', progress: 0.81, eta: 'almost done',
    now: 'Cleaning copy on the permissions screen — last sweep before review.',
    plan: [
      { label: 'Audit screen copy',            done: true, when: 'yesterday' },
      { label: 'Tighten permissions language', cur:  true, when: 'now' },
      { label: 'Stage for design review' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log',  text: 'Three permission-copy variants drafted.' },
        { kind: 'log',  text: 'Picking v2 — calmest read.' },
        { kind: 'edit', text: 'Onboarding/Permissions.swift  +12 −18' },
      ]),
      doc('Permissions copy · v2', [
        '**Notifications.** "We\'ll nudge you when a streak\'s at risk — nothing else."',
        '**Health data.** "For steps + active minutes. Stays on your device."',
        '**Location.** "Only if you want the weather-aware reminders. You can skip this."',
      ].join('\n\n')),
    ],
  },

  {
    id: 'w-011', coveId: 'compass', title: 'HealthKit step sync',
    status: 'waiting', progress: 0.34, eta: 'waiting 8 min',
    now: 'Wants to choose between background-fetch and silent push.',
    plan: [
      { label: 'Read HealthKit docs',             done: true, when: '1h ago' },
      { label: 'Prototype background-fetch path', done: true, when: '40m ago' },
      { label: 'Decide background-fetch vs push', cur:  true, when: 'waiting on you' },
      { label: 'Wire it up' },
      { label: 'Test on cold-start + low battery' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log',  text: "Read HealthKit's step-sync surface." },
        { kind: 'log',  text: 'Prototyped both paths.' },
        { kind: 'out',  text: 'Background-fetch — simpler, ~15 min stale, battery-friendly.' },
        { kind: 'out',  text: 'Silent push — near-real-time via APNs, small battery cost, subtler failure modes.' },
        { kind: 'ask',  text: 'Background-fetch or silent push?' },
        { kind: 'hint', text: 'Streaks update once a day. Background-fetch is probably enough.' },
      ]),
    ],
  },

  {
    id: 'w-020', coveId: 'reef', title: 'Multi-currency support',
    status: 'running', progress: 0.45, eta: '~ 3 hours left',
    now: 'Drafting the conversion service. Will run integration tests next.',
    plan: [
      { label: 'Map currency table',       done: true, when: 'earlier' },
      { label: 'Draft conversion service', cur:  true, when: 'now' },
      { label: 'Plug into cart + checkout' },
      { label: 'Run integration tests' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log',  text: 'Pulling rates from openexchangerates daily.' },
        { kind: 'edit', text: 'src/lib/currency.ts  +84 −0' },
      ]),
      term('npm test -- conversion', [
        { kind: 'cmd',  text: '$ npm test -- conversion' },
        { kind: 'pass', text: '✓  converts USD→EUR' },
        { kind: 'pass', text: '✓  converts EUR→JPY via USD' },
        { kind: 'pass', text: '✓  handles missing rate gracefully' },
        { kind: 'out',  text: 'Tests: 12 passed, 0 failed   (1.4s)' },
      ]),
      git('feat/multi-currency', [
        { sha: 'c41a8e', msg: 'Wire conversion into Cart aggregate',   when: '8m ago'  },
        { sha: '9b27dd', msg: 'Add openexchangerates daily refresh',   when: '32m ago' },
        { sha: '1f04ac', msg: 'Currency reference table + ISO codes',  when: '1h ago'  },
      ]),
    ],
  },

  {
    id: 'w-030', coveId: 'beacon', title: 'Editor block-style toolbar',
    status: 'running', progress: 0.72, eta: '~ 25 min left',
    now: 'Wiring the cmd+/ shortcut.',
    plan: [
      { label: 'Spec the block model',   done: true, when: 'earlier' },
      { label: 'Floating toolbar shell', done: true, when: 'earlier' },
      { label: 'Wire cmd+/ shortcut',    cur:  true, when: 'now' },
      { label: 'Polish hover + focus states' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log',  text: 'Toolbar shell renders — hooking up cmd+/.' },
        { kind: 'edit', text: 'editor/Toolbar.tsx  +47 −12' },
        { kind: 'edit', text: 'editor/keymap.ts  +6 −0' },
      ]),
    ],
  },

  {
    id: 'w-031', coveId: 'beacon', title: 'Image upload pipeline',
    status: 'waiting', progress: 0.40, eta: 'waiting 1h',
    now: 'Wants to confirm S3 vs Cloudflare R2 for blob storage.',
    plan: [
      { label: 'Sketch the upload flow', done: true, when: 'earlier' },
      { label: 'Pick blob storage',      cur:  true, when: 'waiting on you' },
      { label: 'Implement upload + signed URLs' },
      { label: 'Image resizing pipeline' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log',  text: 'Sketched the upload flow.' },
        { kind: 'out',  text: 'R2 — no egress fees, great for a public CDN.' },
        { kind: 'out',  text: 'S3 — deeper ecosystem, existing IAM patterns.' },
        { kind: 'ask',  text: 'R2 or S3 for blob storage?' },
        { kind: 'hint', text: 'Images are public + read-heavy. R2 is the cheaper fit.' },
      ]),
    ],
  },

  {
    id: 'w-021', coveId: 'reef', title: 'Refactor checkout state machine',
    status: 'waiting', progress: 0.60, eta: 'waiting 22 min',
    now: 'Two failing tests — wants to know if expected behavior changed.',
    plan: [
      { label: 'Capture failing test outputs', done: true, when: '30m ago' },
      { label: 'Decide on expected behavior',  cur:  true, when: 'waiting on you' },
      { label: 'Fix the state machine' },
      { label: 'Re-run the suite' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log',  text: 'Two failing tests in `calc total tax`.' },
        { kind: 'out',  text: "Both fail because I switched rounding to banker's." },
        { kind: 'out',  text: 'The tests expect half-up.' },
        { kind: 'ask',  text: "Switch back to half-up, or keep banker's and fix the tests?" },
        { kind: 'hint', text: 'Half-up is what every other line in the codebase uses.' },
      ]),
      term('npm test -- tax', [
        { kind: 'cmd',  text: '$ npm test -- tax' },
        { kind: 'pass', text: '✓  rounds positive amounts' },
        { kind: 'pass', text: '✓  rounds zero' },
        { kind: 'fail', text: "✗  totalTax · banker's vs half-up" },
        { kind: 'out',  text: '    expected 1.18, got 1.17' },
        { kind: 'fail', text: '✗  totalTax · sums correctly' },
        { kind: 'out',  text: '    expected 12.50, got 12.49' },
        { kind: 'err',  text: 'Tests: 2 failed, 2 passed   (0.9s)' },
      ]),
      diff('src/checkout/tax.ts', 1, 1, [
        {
          header: '@@ -42,3 +42,3 @@ export function totalTax(',
          lines: [
            { kind: 'ctx', text: '  const subtotal = sum(lines.map(l => l.tax))' },
            { kind: 'rm',  text: '  return roundBankers(subtotal, 2)' },
            { kind: 'add', text: '  return roundHalfUp(subtotal, 2)' },
          ],
        },
      ]),
    ],
  },

  {
    id: 'w-012', coveId: 'compass', title: 'Migrate persistence to SwiftData',
    status: 'waiting', progress: 0.50, eta: 'paused yesterday',
    now: 'Paused — schema decision still open.',
    plan: [
      { label: 'Inventory all Core Data models', done: true, when: 'Mon' },
      { label: 'Map to SwiftData schema',        cur:  true, when: 'on hold' },
      { label: 'Write migration plan' },
      { label: 'Stage the cut-over' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log', text: 'Paused at your request.' },
        { kind: 'log', text: 'Resume any time — plan + history preserved.' },
      ]),
      doc('Migration plan · draft', [
        '**Phase 1.** Add SwiftData alongside Core Data behind a feature flag. Both write; SwiftData is the read source in DEBUG.',
        '**Phase 2.** Background migration on first launch of 2.5. Verify schema parity. Surface a one-line `Cleaning up…` in onboarding if needed.',
        '**Phase 3.** Strip Core Data after one stable release. Keep the importer code path for users on older builds.',
      ].join('\n\n')),
    ],
  },

  {
    id: 'w-013', coveId: 'compass', title: 'App icon refresh',
    status: 'waiting', progress: 1.0, eta: 'done 2 days ago',
    now: 'Shipped the soft-blue direction.',
    plan: [
      { label: 'Three icon directions',   done: true },
      { label: 'Pick the soft-blue path', done: true },
      { label: 'Export at all sizes',     done: true },
      { label: 'Ship',                    done: true, when: '2d ago' },
    ],
    cards: [
      term('claude code', [
        { kind: 'log',  text: 'Shipped the soft-blue direction.' },
        { kind: 'log',  text: 'Icon lives in Assets.xcassets/AppIcon — all 14 sizes generated.' },
        { kind: 'log',  text: 'Build 2.4.1 went out Friday.' },
      ]),
      git('main', [
        { sha: 'fe2c41', msg: 'Ship build 2.4.1 — soft-blue icon', when: '2d ago' },
        { sha: 'a91d77', msg: 'Export AppIcon at all 14 sizes',    when: '2d ago' },
      ]),
    ],
  },
];
