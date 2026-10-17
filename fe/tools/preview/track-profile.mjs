import { chromium } from '@playwright/test';
import { createHash, randomUUID } from 'node:crypto';
import { lstat, mkdir, readFile, readdir, readlink, realpath, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { parseArgs } from 'node:util';
import { preview } from 'vite';

/** @typedef {import('./track-profile-types.d.ts').BrowserScope} BrowserScope */
/** @typedef {import('./track-profile-types.d.ts').Probe} Probe */
/** @typedef {import('./track-profile-types.d.ts').Capture} Capture */

/** @param {unknown} error */
function errorCode(error) {
  return typeof error === 'object' && error !== null && 'code' in error ? error.code : undefined;
}
/** @param {unknown} error @returns {import('./track-profile-types.d.ts').Failure} */
function failureOf(error) { return { message: String(error), stack: error instanceof Error ? error.stack ?? null : null }; }

const { values } = parseArgs({ strict: true, options: {
  build: { type: 'string' }, output: { type: 'string' }, label: { type: 'string', default: 'track' },
  count: { type: 'string', default: '300' }, width: { type: 'string', default: '1400' },
  'detail-delay-ms': { type: 'string', default: '0' }, scenario: { type: 'string', default: 'returns' },
  'reduced-motion': { type: 'boolean', default: false }, cpu: { type: 'boolean', default: false },
  help: { type: 'boolean', default: false },
} });
if (values.help) {
  process.stdout.write('Usage: node tools/preview/track-profile.mjs --build DIR [--count 300] [--width 1400] [--label LABEL] [--detail-delay-ms 1000] [--scenario returns|rapid] [--reduced-motion] [--cpu] [--output NEW_DIR]\n');
  process.exit(0);
}
/** @param {string} name @param {string} value @param {number} minimum @param {number} maximum */
function integer(name, value, minimum, maximum) {
  if (!/^\d+$/.test(value) || !Number.isSafeInteger(Number(value)) || Number(value) < minimum || Number(value) > maximum) {
    throw new Error(`${name} must be an integer from ${minimum} to ${maximum}`);
  }
  return Number(value);
}
if (values.build === undefined) throw new Error('--build must identify a profile-build output directory');
if (!/^[a-zA-Z0-9._-]{1,64}$/.test(values.label)) throw new Error('--label must be 1–64 letters, numbers, dots, underscores or hyphens');
if (!['returns', 'rapid'].includes(values.scenario)) throw new Error('--scenario must be returns or rapid');
const count = integer('--count', values.count, 1, 10000);
const width = integer('--width', values.width, 320, 3840);
const detailDelayMs = integer('--detail-delay-ms', values['detail-delay-ms'], 0, 30000);
const fe = resolve(import.meta.dirname, '../..');
const outDir = await realpath(resolve(values.build));
/** @param {string} path @returns {Promise<string>} */
async function canonicalDestination(path) {
  let ancestor = resolve(path);
  const segments = [];
  for (;;) {
    try { return resolve(await realpath(ancestor), ...segments); }
    catch (error) {
      if (errorCode(error) !== 'ENOENT') throw error;
      const entry = await lstat(ancestor).catch(cause => {
        if (errorCode(cause) === 'ENOENT') return null;
        throw cause;
      });
      if (entry?.isSymbolicLink()) {
        ancestor = resolve(dirname(ancestor), await readlink(ancestor));
        continue;
      }
      const parent = dirname(ancestor);
      if (parent === ancestor) throw error;
      segments.unshift(basename(ancestor));
      ancestor = parent;
    }
  }
}
const output = await canonicalDestination(values.output ?? join(fe, 'test-results/track-presentation', `${values.label}-${Date.now()}-${randomUUID().slice(0, 8)}`));
const buildRelativeOutput = relative(outDir, output);
if (buildRelativeOutput !== '..' && !buildRelativeOutput.startsWith(`..${sep}`) && !isAbsolute(buildRelativeOutput)) {
  throw new Error('--output must be outside the immutable build directory');
}
await mkdir(dirname(output), { recursive: true });
await mkdir(output); // Refuse overwriting any previous successful or failed capture.
/** @param {string} directory @returns {Promise<import('./track-profile-types.d.ts').BuildInput>} */
async function readBuild(directory) {
  const metadata = /** @type {import('./track-profile-types.d.ts').BuildMetadata} */ (JSON.parse(await readFile(join(directory, 'profile-build.json'), 'utf8')));
  if (!/^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(metadata.commit)
    || typeof metadata.workingTreeDirty !== 'boolean' || typeof metadata.reactProfiling !== 'boolean'
    || metadata.sourceMaps !== true || typeof metadata.outDir !== 'string'
    || await realpath(metadata.outDir) !== directory) throw new Error('Invalid or mismatched profile-build.json');
  const hashes = [];
  const inputs = [];
  for (const path of await buildFiles(directory)) {
    const bytes = await readFile(path);
    hashes.push({ path: relative(directory, path), bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') });
    if (!path.endsWith('.map')) continue;
    const map = /** @type {import('./track-profile-types.d.ts').SourceMapInput} */ (JSON.parse(bytes.toString()));
    const sources = map.sources ?? [];
    for (let index = 0; index < sources.length; index++) {
      const source = sources[index].replaceAll('\\', '/');
      if (!source.endsWith('/web/src/app/providers/public.tsx') && source !== 'web/src/app/providers/public.tsx') continue;
      const text = map.sourcesContent?.[index];
      const match = typeof text === 'string' && /^export const WEB_COMPAT_VERSION = (\d+);$/m.exec(text);
      if (!match) throw new Error('Build source map does not declare WEB_COMPAT_VERSION');
      inputs.push({ map: relative(directory, path), source, version: Number(match[1]), sha256: createHash('sha256').update(text).digest('hex') });
    }
  }
  if (inputs.length === 0 || new Set(inputs.map(input => `${input.version}:${input.sha256}`)).size !== 1) {
    throw new Error('Build maps must contain one coherent providers/public.tsx source input');
  }
  return { metadata, hashes, inputs, compatVersion: inputs[0].version };
}
/** @param {string} directory @returns {Promise<string[]>} */
async function buildFiles(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...await buildFiles(path));
    else if (entry.isFile()) files.push(path);
  }
  return files.sort();
}
/** @type {import('./track-profile-types.d.ts').BuildInput} */
let buildInput;
try { buildInput = await readBuild(outDir); }
catch (error) {
  await writeFile(join(output, 'report.json'), JSON.stringify({ complete: false, label: values.label,
    build: outDir, failures: [{ message: String(error) }] }, null, 2) + '\n');
  throw error;
}
const { metadata, hashes, inputs, compatVersion } = buildInput;
async function verifyBuild() {
  if ((await buildFiles(outDir)).length !== hashes.length) throw new Error('Build files changed during capture');
  for (const file of hashes) {
    if (createHash('sha256').update(await readFile(join(outDir, file.path))).digest('hex') !== file.sha256) {
      throw new Error(`Build file changed: ${file.path}`);
    }
  }
}
const area = { id: 'profile-area', name: 'Profile area', color: '#6574cd', sort: 0, kind: 'user', created_at: 1, updated_at: 1 };
const tracks = [
  { id: 'small', area_id: area.id, title: 'Small track', sort: 0, cwd: '/workspace', pinned_at: null, closed_at: null, admin_granted_at: null, created_at: 1, updated_at: 2 },
  { id: 'large', area_id: area.id, title: 'Large track', sort: 1, cwd: '/workspace', pinned_at: null, closed_at: null, admin_granted_at: null, created_at: 1, updated_at: 2 },
];
const fixture = tracks.map(track => ({ id: track.id, title: track.title, count: track.id === 'large' ? count : 2 }));
/** @param {string} id @param {number} size */
function reportCard(id, size) {
  return { id: `report-${id}`, track_id: id, kind: 'track-report', title: null, sort: 0,
    payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: Array.from({ length: size }, (_, index) => ({
      id: `block-${index}`, kind: 'prose', rev: 1, payload: { markdown: `# ${index === 0 ? `Report ${id}` : `Section ${index}`}\n\n${`Evidence for item ${index}: **result ${index * 17}**, a [reference](https://example.invalid/${index}) and independent report text for case ${index}. `.repeat(5)}` },
    })) }, deletable: false, created_at: 1, updated_at: 2 };
}
const details = new Map(tracks.map(track => {
  const item = fixture.find(item => item.id === track.id);
  if (item === undefined) throw new Error(`Missing fixture for ${track.id}`);
  return [track.id, { track, can_reopen: false, can_admin: true, can_close: true,
    cards: [reportCard(track.id, item.count)], overlays: [] }];
}));
/** @type {import('./track-profile-types.d.ts').RecordedRequest[]} */
const requests = [];
/** @type {import('./track-profile-types.d.ts').Failure[]} */
const pageErrors = [];
/** @type {{ url: string; status: number }[]} */
const assetErrors = [];
/** @type {import('./track-profile-types.d.ts').Failure[]} */
const failures = [];
/** @type {import('./track-profile-types.d.ts').Step[]} */
const steps = [];
/** @type {import('@playwright/test').Browser | undefined} */
let browser;
/** @type {import('@playwright/test').BrowserContext | undefined} */
let context;
/** @type {import('@playwright/test').Page | undefined} */
let page;
/** @type {import('vite').PreviewServer | undefined} */
let server;
/** @type {import('@playwright/test').CDPSession | undefined} */
let cpu;
/** @type {string | undefined} */
let baseUrl;
/** @type {string | undefined} */
let browserVersion;
/** @type {Capture | undefined} */
let captured;
let closing = false;
/** @type {Map<ReturnType<typeof setTimeout>, () => void>} */
const delayedReads = new Map();
let complete = false;
/** @type {Error | null} */
let interrupted = null;
/** @param {string} signal */
const interrupt = signal => {
  interrupted = new Error(`Capture interrupted by ${signal}`);
  void browser?.close().catch(error => { failures.push({ message: String(error) }); });
};
const onInterrupt = () => interrupt('SIGINT');
const onTerminate = () => interrupt('SIGTERM');
process.once('SIGINT', onInterrupt);
process.once('SIGTERM', onTerminate);
try {
  server = await preview({ configFile: false, root: join(fe, 'web'), base: '/next/', logLevel: 'warn',
    build: { outDir }, preview: { host: '127.0.0.1', port: 0, strictPort: true } });
  const address = server.httpServer.address();
  if (address === null || typeof address === 'string') throw new Error('Preview has no loopback TCP address');
  baseUrl = `http://127.0.0.1:${address.port}`;
  browser = await chromium.launch({ headless: true });
  browserVersion = browser.version();
  context = await browser.newContext({ viewport: { width, height: 900 }, serviceWorkers: 'block',
    reducedMotion: values['reduced-motion'] ? 'reduce' : 'no-preference' });
  page = await context.newPage();
  page.setDefaultTimeout(15000 + detailDelayMs);
  page.on('pageerror', error => { pageErrors.push({ message: error.message, stack: error.stack }); });
  page.on('response', response => {
    if (response.status() >= 400 && !new URL(response.url()).pathname.startsWith('/api/')) {
      assetErrors.push({ url: response.url(), status: response.status() });
    }
  });
  await page.route('**/api/**', async route => {
    const request = route.request(); const url = new URL(request.url()); const path = url.pathname;
    const recorded = { method: request.method(), path: path + url.search, hostAt: Date.now(), handled: true };
    requests.push(recorded);
    /** @type {unknown} */
    let body;
    if (path === '/api/auth/whoami') body = { userId: 'profile-user', displayName: 'Profile', role: 'owner', sessionId: 'profile-session' };
    else if (path === '/api/version') body = { assistantProviders: true, conversationCreateModel: true, conversationSide: true,
      webCompatVersion: compatVersion, minWebCompatVersion: compatVersion, syncEventVersion: 0,
      dbInstanceId: 'profile-boot', databaseId: 'profile-database' };
    else if (path === '/api/areas') body = [area];
    else if (path === `/api/areas/${area.id}/tracks`) body = tracks;
    else if (path === '/api/tracks/small' || path === '/api/tracks/large') {
      const id = path.slice('/api/tracks/'.length);
      if (id === 'large' && detailDelayMs > 0) await new Promise(/** @param {(value?: undefined) => void} resolve */ resolve => {
        const timer = setTimeout(() => { delayedReads.delete(timer); resolve(); }, detailDelayMs);
        delayedReads.set(timer, resolve);
      });
      body = details.get(id);
    } else if (/^\/api\/tracks\/(small|large)\/conversations$/.test(path)) body = [];
    else if (/^\/api\/tracks\/(small|large)\/backlinks$/.test(path)) body = { backlinks: [], truncated: false, skipped_sources: 0 };
    else if (/^\/api\/tracks\/(small|large)\/report$/.test(path)) body = { taskDiagnostics: [] };
    else if (path === '/api/read-receipts') body = { baselineMs: Number.MAX_SAFE_INTEGER, receipts: [] };
    else if (path.startsWith('/api/read-receipts/') && path.endsWith('/read')) {
      const parts = path.split('/');
      body = { kind: parts[3], targetId: parts[4], ...request.postDataJSON(), manualUnread: false };
    } else if (path === '/api/settings' || path === '/api/git/status') body = {};
    else if (['/api/plugins', '/api/track-templates', '/api/agent-providers', '/api/agent-providers/managed', '/api/overlays', '/api/areas/closed'].includes(path)) body = [];
    else if (path === '/api/models') body = { models: [], default: { model: null, reasoning_effort: null, supported_reasoning_efforts: null },
      default_source: 'unknown', source: 'live', fetched_at_ms: 1 };
    else if (path.startsWith('/api/today/daily')) body = null;
    else {
      recorded.handled = false;
      await route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: 'Unhandled profile endpoint', code: 'not_found' }) });
      return;
    }
    if (!closing) await route.fulfill({ contentType: 'application/json', body: JSON.stringify(body) });
  });
  await page.routeWebSocket('**/api/events', () => {});
  await page.addInitScript(({ fixture }) => {
    const scope = /** @type {BrowserScope} */ (globalThis);
    const { document, performance, requestAnimationFrame, setTimeout, MutationObserver, PerformanceObserver, Worker } = scope;
    /** @type {Probe} */
    const probe = { recording: false, clicks: [], snapshots: [], frames: [], tasks: [], workers: [], pulses: [], state: null };
    scope.__ncTrackProfile = probe;
    const at = () => performance.now();
    const active = () => fixture.find(item => scope.location.pathname.endsWith(`/track/${item.id}`));
    const inspect = () => {
      const target = active();
      const trackPage = document.querySelector('[data-nc-track-page]');
      const report = document.querySelector('[data-nc-report]');
      const title = trackPage?.querySelector('h1')?.textContent
        || document.querySelector('[data-nc-workspace-header] [data-nc-page-title]')?.textContent || null;
      const statuses = Array.from(document.querySelectorAll('[role="status"]')).map(node => node.textContent);
      const neutral = statuses.includes('Loading track…');
      const firstHeading = report?.querySelector('h2')?.textContent ?? null;
      const headings = report?.querySelectorAll('h2').length ?? 0;
      const region = document.querySelector('section[aria-label="Track report"]');
      const result = { route: scope.location.pathname, target: target?.id ?? null,
        shell: target !== undefined && (neutral || title?.includes(target.title) === true),
        title, neutral, statuses, report: report !== null, firstHeading, headings,
        busy: region?.getAttribute('aria-busy') ?? null,
        full: target !== undefined && firstHeading === `Report ${target.id}` && headings === target.count && region?.getAttribute('aria-busy') !== 'true' };
      if (JSON.stringify(result) === JSON.stringify(probe.state)) return;
      probe.state = result;
      if (probe.recording) probe.snapshots.push({ at: at(), ...result });
    };
    new MutationObserver(inspect).observe(document, { subtree: true, childList: true, characterData: true, attributes: true,
      attributeFilter: ['aria-busy', 'aria-current'] });
    new PerformanceObserver(list => {
      if (probe.recording) probe.tasks.push(...list.getEntries().map(entry => ({ start: entry.startTime, duration: entry.duration })));
    }).observe({ type: 'longtask', buffered: false });
    document.addEventListener('click', event => {
      const button = event.target instanceof scope.Element ? event.target.closest('button') : null;
      const label = button?.getAttribute('aria-label');
      const selected = fixture.find(item => label === `Track ${item.title}` || label === item.title);
      if (selected === undefined || !probe.recording) return;
      const clicked = at();
      probe.clicks.push({ target: selected.id, at: clicked, trusted: event.isTrusted, eventAt: event.timeStamp, handlingDelay: clicked - event.timeStamp });
      setTimeout(() => { probe.pulses.push({ clickAt: clicked, at: at() }); }, 0);
      requestAnimationFrame(time => { probe.pulses.push({ clickAt: clicked, frameAt: time, at: at() }); });
    }, true);
    /** @type {number | null} */
    let previous = null;
    /** @type {number | null} */
    let previousObserved = null;
    /** @param {number} time */
    const frame = time => {
      const observedAt = at();
      if (probe.recording) probe.frames.push({ at: observedAt, frameAt: time, gap: previous === null ? null : time - previous,
        observedGap: previousObserved === null ? null : observedAt - previousObserved,
        target: probe.state?.target ?? null, shell: probe.state?.shell ?? false, report: probe.state?.report ?? false,
        firstHeading: probe.state?.firstHeading ?? null, full: probe.state?.full ?? false });
      previous = time; previousObserved = observedAt;
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
    let workerSequence = 0;
    /** @param {unknown} value */
    const messageType = value => typeof value === 'object' && value !== null && 'type' in value && typeof value.type === 'string' ? value.type : null;
    scope.Worker = class extends Worker {
      /** @param {string | URL} url @param {WorkerOptions} [options] */
      constructor(url, options) {
        super(url, options);
        this.profileId = ++workerSequence;
        if (probe.recording) probe.workers.push({ kind: 'create', id: this.profileId, at: at(), url: String(url) });
        /** @param {MessageEvent<unknown>} event */
        this.profileReply = event => {
          if (probe.recording) probe.workers.push({ kind: 'reply', id: this.profileId, at: at(), type: messageType(event.data) });
        };
        /** @param {ErrorEvent} event */
        this.profileError = event => {
          if (probe.recording) probe.workers.push({ kind: 'error', id: this.profileId, at: at(), message: event.message });
        };
        this.addEventListener('message', this.profileReply);
        this.addEventListener('error', this.profileError);
      }
      /** @param {unknown} message @param {Transferable[] | StructuredSerializeOptions} [options] */
      postMessage(message, options) {
        if (probe.recording) probe.workers.push({ kind: 'post', id: this.profileId, at: at(), type: messageType(message) });
        if (Array.isArray(options)) super.postMessage(message, options);
        else super.postMessage(message, options);
      }
      terminate() {
        if (probe.recording) probe.workers.push({ kind: 'terminate', id: this.profileId, at: at() });
        this.removeEventListener('message', this.profileReply);
        this.removeEventListener('error', this.profileError);
        super.terminate();
      }
    };
  }, { fixture });
  await page.goto(`${baseUrl}/next/track/small`);
  await page.waitForFunction(() => {
    const scope = /** @type {BrowserScope} */ (globalThis);
    return scope.__ncTrackProfile.state?.full === true;
  });
  await page.waitForTimeout(500);
  if (values.cpu) {
    cpu = await context.newCDPSession(page);
    await cpu.send('Profiler.enable');
    await cpu.send('Profiler.setSamplingInterval', { interval: 1000 });
    await cpu.send('Profiler.start');
  }
  await page.evaluate(() => {
    const scope = /** @type {BrowserScope} */ (globalThis);
    scope.__ncTrackProfile.recording = true;
  });
  const activePage = page;

  /** @param {string} id */
  async function navigationControl(id) {
    const item = fixture.find(item => item.id === id);
    if (item === undefined) throw new Error(`Unknown Track fixture: ${id}`);
    const desktop = activePage.getByRole('button', { name: `Track ${item.title}`, exact: true });
    if (await desktop.isVisible()) return desktop;
    await activePage.getByRole('button', { name: 'Open workspace', exact: true }).click();
    const areaControl = activePage.getByRole('button', { name: area.name, exact: true });
    if (await areaControl.isVisible()) await areaControl.click();
    return activePage.getByRole('button', { name: item.title, exact: true });
  }
  /** @param {import('@playwright/test').Locator} control */
  async function clickControl(control) {
    // Native workspace pages move during entry: wait for the real control's actionability.
    await control.click();
  }
  /** @param {string} id */
  async function waitForTrack(id) {
    await activePage.waitForFunction(target => {
      const scope = /** @type {BrowserScope} */ (globalThis);
      return scope.__ncTrackProfile.state?.target === target && scope.__ncTrackProfile.state.full;
    }, id);
  }
  if (values.scenario === 'returns') {
    for (const [index, id] of ['large', 'small', 'large', 'small', 'large'].entries()) {
      const step = { step: index, target: id, attemptedAt: Date.now(), complete: false };
      steps.push(step);
      await clickControl(await navigationControl(id));
      await waitForTrack(id);
      step.complete = true;
      await page.waitForTimeout(500);
    }
  } else {
    const large = await navigationControl('large');
    const small = page.getByRole('button', { name: 'Track Small track', exact: true });
    if (!await small.isVisible()) throw new Error('Rapid scenario requires concurrently visible real Track controls; use returns on mobile');
    const largeBox = await large.boundingBox(); const smallBox = await small.boundingBox();
    if (largeBox === null || smallBox === null) throw new Error('Rapid navigation controls have no geometry');
    for (const [index, id] of ['large', 'small', 'large'].entries()) {
      const box = id === 'large' ? largeBox : smallBox;
      const step = { step: index, target: id, attemptedAt: Date.now(), complete: false };
      steps.push(step);
      await activePage.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
      if (index < 2) await new Promise(resolve => setTimeout(resolve, 20));
      else { await waitForTrack(id); step.complete = true; }
    }
    await page.waitForTimeout(500 + detailDelayMs);
    for (const step of steps.slice(0, 2)) step.superseded = true;
  }
  captured = await page.evaluate(() => {
    const scope = /** @type {BrowserScope} */ (globalThis);
    const probe = scope.__ncTrackProfile; probe.recording = false;
    return { ...probe, timeOrigin: scope.performance.timeOrigin, resources: scope.performance.getEntriesByType('resource').map(value => {
      const entry = /** @type {PerformanceResourceTiming} */ (value);
      return { url: entry.name, initiatorType: entry.initiatorType, start: entry.startTime, requestStart: entry.requestStart, responseStart: entry.responseStart,
        responseEnd: entry.responseEnd, duration: entry.duration, transferSize: entry.transferSize };
    }), elements: scope.document.querySelectorAll('*').length };
  });
  if (captured.clicks.length !== steps.length || steps.some(step => !step.complete && !step.superseded)) throw new Error('Capture has missing selections or incomplete steps');
  if (pageErrors.length || assetErrors.length || requests.some(request => !request.handled)) throw new Error('Capture has browser, asset or fixture errors');
  if (interrupted !== null) throw interrupted;
  await page.screenshot({ path: join(output, 'track.png') });
  complete = true;
} catch (error) {
  failures.push(failureOf(error));
  if (page !== undefined && !page.isClosed()) {
    try {
      captured = await page.evaluate(() => {
        const scope = /** @type {BrowserScope} */ (globalThis);
        return { ...scope.__ncTrackProfile, elements: scope.document.querySelectorAll('*').length };
      });
      await page.screenshot({ path: join(output, 'failure.png') });
      await writeFile(join(output, 'failure.txt'), await page.locator('body').innerText());
    } catch (captureError) { failures.push({ message: String(captureError) }); }
  }
  process.exitCode = 1;
} finally {
  if (cpu !== undefined) {
    try { const { profile } = await cpu.send('Profiler.stop'); await writeFile(join(output, 'cpu.cpuprofile'), JSON.stringify(profile)); }
    catch (error) { failures.push({ message: `CPU capture failed: ${String(error)}` }); process.exitCode = 1; complete = false; }
  }
  closing = true;
  for (const [timer, finish] of delayedReads) { clearTimeout(timer); finish(); }
  delayedReads.clear();
  /** @param {string} owner @param {() => unknown | Promise<unknown>} close */
  const cleanup = async (owner, close) => {
    try { await close(); }
    catch (error) { failures.push({ message: `${owner} cleanup failed: ${String(error)}` }); process.exitCode = 1; complete = false; }
  };
  await cleanup('Browser context', () => context?.close());
  await cleanup('Browser', () => browser?.close());
  await cleanup('Owned preview', () => {
    if (server === undefined) return;
    const owned = server;
    return new Promise(/** @param {(value?: undefined) => void} resolve @param {(reason: Error) => void} reject */ (resolve, reject) => {
      owned.httpServer.close(error => error ? reject(error) : resolve());
    });
  });
  process.removeListener('SIGINT', onInterrupt);
  process.removeListener('SIGTERM', onTerminate);
  await cleanup('Build immutability check', verifyBuild);
  const clicks = captured?.clicks ?? [];
  const summaries = clicks.map((click, index) => {
    const until = clicks[index + 1]?.at ?? Infinity;
    const states = (captured?.snapshots ?? []).filter(state => state.at >= click.at && state.at < until && state.target === click.target);
    const shell = states.find(state => state.shell);
    const first = states.find(state => state.firstHeading === `Report ${click.target}`);
    const full = states.find(state => state.full);
    const firstFrame = (captured?.frames ?? []).find(frame => frame.at >= click.at && frame.at < until);
    const shellFrame = (captured?.frames ?? []).find(frame => frame.at >= click.at && frame.at < until && frame.target === click.target && frame.shell);
    const afterFull = full === undefined ? undefined : (captured?.frames ?? []).find(frame => frame.at >= full.at && frame.at < until);
    const pulse = (captured?.pulses ?? []).find(pulse => pulse.clickAt === click.at && pulse.frameAt === undefined);
    return { target: click.target, clickAt: click.at, shellMs: shell === undefined ? null : shell.at - click.at,
      firstReportMs: first === undefined ? null : first.at - click.at, fullReportMs: full === undefined ? null : full.at - click.at,
      firstFrameMs: firstFrame === undefined ? null : firstFrame.at - click.at,
      firstFrameTarget: firstFrame?.target ?? null,
      firstFrameIncludedSelectedReport: firstFrame === undefined ? null : firstFrame.firstHeading === `Report ${click.target}`,
      firstFrameHadSelectedShell: firstFrame === undefined ? null : firstFrame.target === click.target && firstFrame.shell,
      selectedShellFrameMs: shellFrame === undefined ? null : shellFrame.at - click.at,
      selectedShellFrameIncludedReport: shellFrame === undefined ? null : shellFrame.firstHeading === `Report ${click.target}`,
      firstFrameAfterFullMs: afterFull === undefined ? null : afterFull.at - click.at,
      taskPulseMs: pulse === undefined ? null : pulse.at - click.at,
      shellIncludedReport: shell?.report ?? null, fullHeadings: full?.headings ?? null };
  });
  const report = { complete, label: values.label, fixture: { count, width, height: 900, variedProse: true, detailDelayMs,
    scenario: values.scenario, reducedMotion: values['reduced-motion'] }, metadata, buildInputs: inputs, buildFiles: hashes,
    diagnostics: { cpu: values.cpu, reactProfiling: metadata.reactProfiling,
      note: 'DOM observations are not paint/INP/readiness. Main-thread rAF is not compositor FPS. Worker events are observed with an additional listener; CPU sampling is a separate cohort. Timers indicate main-task availability, not actual input response.' },
    baseUrl, browser: browserVersion, steps, summaries, failures, pageErrors, assetErrors, requests, ...captured };
  await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2) + '\n');
  process.stdout.write(JSON.stringify({ output, complete, commit: metadata.commit, summaries, failures, pageErrors }) + '\n');
}
