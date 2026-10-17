import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { TrackReport } from '../../../../../core/domain/report.ts';
import type { CardWire } from '../../../../../core/domain/track.ts';
import { createReportPreparationCache, prepareReportDocument, type PreparedReport, type ReportPreparationPort } from './preparation.ts';
import { useReportPreparation } from './resource.ts';

function card(title: string): CardWire {
  return { id: 'report', track_id: 'track', kind: 'track-report', title: null, sort: 0,
    payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: [
      { id: 'one', kind: 'prose', rev: 1, payload: { markdown: `# ${title}\n\nEvidence for ${title}.` } },
    ] }, deletable: false, created_at: 1, updated_at: 1 };
}

function controlled() {
  const jobs: { signal: AbortSignal; prepared: PreparedReport; resolve: (value: PreparedReport) => void; reject: (reason: unknown) => void }[] = [];
  const execute: ReportPreparationPort = vi.fn((report: TrackReport | null, signal: AbortSignal) => new Promise<PreparedReport>((resolve, reject) => {
    jobs.push({ signal, prepared: structuredClone(prepareReportDocument(report)), resolve, reject });
  }));
  const complete = async (index: number) => {
    await act(async () => { jobs[index].resolve(jobs[index].prepared); await Promise.resolve(); });
  };
  return { execute, jobs, complete };
}

afterEach(cleanup);

it('does not execute while disabled and exposes a completed cache hit immediately', async () => {
  const cache = createReportPreparationCache(); const wire = card('Cached'); const source = cache.source([wire]);
  const work = controlled();
  const view = renderHook(({ enabled }) => useReportPreparation('track', source, cache, work.execute, enabled), {
    initialProps: { enabled: false },
  });
  expect(view.result.current).toMatchObject({ status: 'waiting', candidate: null, error: null });
  expect(work.execute).not.toHaveBeenCalled();
  view.rerender({ enabled: true });
  expect(view.result.current.status).toBe('preparing');
  await work.complete(0);
  expect(view.result.current.status).toBe('ready');
  view.unmount();
  const cached = renderHook(() => useReportPreparation('track', source, cache, work.execute, false));
  expect(cached.result.current).toMatchObject({ status: 'ready', candidate: { source, preparation: work.jobs[0].prepared } });
  expect(work.execute).toHaveBeenCalledTimes(1);
});

it('retains only a same-selection model and atomically replaces it with the cloned new revision', async () => {
  const cache = createReportPreparationCache(); const first = cache.source([card('First')]); const next = cache.source([card('Next')]);
  const work = controlled();
  const view = renderHook(({ source }) => useReportPreparation('track', source, cache, work.execute, true), {
    initialProps: { source: first },
  });
  await work.complete(0);
  const candidate = view.result.current.candidate!;
  expect(candidate.preparation.report).not.toBe(first.report);
  view.rerender({ source: next });
  expect(view.result.current).toMatchObject({ status: 'preparing', candidate });
  expect(view.result.current.candidate?.preparation.outline[0].label).toBe('First');
  await work.complete(1);
  expect(view.result.current).toMatchObject({ status: 'ready', candidate: { source: next, preparation: work.jobs[1].prepared } });
  expect(view.result.current.candidate?.preparation.outline[0].label).toBe('Next');
});

it('rejects A1 and B results after an A→B→A visit even when execution ignores abort', async () => {
  const cache = createReportPreparationCache(); const a = cache.source([card('A')]); const b = cache.source([card('B')]);
  const work = controlled();
  const view = renderHook(({ key, source }) => useReportPreparation(key, source, cache, work.execute, true), {
    initialProps: { key: 'a', source: a },
  });
  view.rerender({ key: 'b', source: b });
  expect(view.result.current.candidate).toBeNull();
  expect(work.jobs[0].signal.aborted).toBe(true);
  view.rerender({ key: 'a', source: a });
  expect(work.jobs[1].signal.aborted).toBe(true);
  await work.complete(0); await work.complete(1);
  expect(view.result.current).toMatchObject({ status: 'preparing', candidate: null });
  expect(cache.peek(a)).toBeUndefined(); expect(cache.peek(b)).toBeUndefined();
  await work.complete(2);
  expect(view.result.current).toMatchObject({ status: 'ready', candidate: { source: a } });
});

it('aborts replaced sources and disabled preparation without exposing stale failures', async () => {
  const cache = createReportPreparationCache(); const first = cache.source([card('Old')]); const next = cache.source([card('New')]);
  const work = controlled();
  const view = renderHook(({ source, enabled }) => useReportPreparation('track', source, cache, work.execute, enabled), {
    initialProps: { source: first, enabled: true },
  });
  view.rerender({ source: next, enabled: true });
  expect(work.jobs[0].signal.aborted).toBe(true);
  await act(async () => { work.jobs[0].reject(new Error('Obsolete failure')); await Promise.resolve(); });
  expect(view.result.current).toMatchObject({ status: 'preparing', error: null });
  view.rerender({ source: next, enabled: false });
  expect(work.jobs[1].signal.aborted).toBe(true);
  await work.complete(1);
  expect(view.result.current).toMatchObject({ status: 'waiting', candidate: null, error: null });
  expect(cache.peek(next)).toBeUndefined();
  view.rerender({ source: next, enabled: true });
  await work.complete(2);
  expect(view.result.current.status).toBe('ready');
});

it('retains a candidate revision on failure and retries only its current lease', async () => {
  const cache = createReportPreparationCache(); const first = cache.source([card('Seen')]); const next = cache.source([card('Retry')]);
  const work = controlled();
  const view = renderHook(({ source }) => useReportPreparation('track', source, cache, work.execute, true), {
    initialProps: { source: first },
  });
  await work.complete(0);
  const oldRetry = view.result.current.retry;
  view.rerender({ source: next });
  await act(async () => { work.jobs[1].reject('Preparation failed'); await Promise.resolve(); });
  expect(view.result.current).toMatchObject({ status: 'failed', candidate: { source: first } });
  expect(view.result.current.error).toBeInstanceOf(Error);
  expect(view.result.current.error?.message).toBe('Preparation failed');
  act(() => { oldRetry(); });
  expect(work.execute).toHaveBeenCalledTimes(2);
  act(() => { view.result.current.retry(); });
  expect(view.result.current).toMatchObject({ status: 'preparing', candidate: { source: first }, error: null });
  await work.complete(2);
  expect(view.result.current).toMatchObject({ status: 'ready', candidate: { source: next } });
});

it('releases StrictMode and unmounted executions and starts independent resource instances', async () => {
  const cache = createReportPreparationCache(); const source = cache.source([card('Strict')]); const work = controlled();
  const view = renderHook(() => useReportPreparation('track', source, cache, work.execute, true), {
    reactStrictMode: true,
  });
  expect(work.jobs).toHaveLength(2);
  expect(work.jobs[0].signal.aborted).toBe(true);
  expect(work.jobs[1].signal.aborted).toBe(false);
  await work.complete(0);
  expect(view.result.current.candidate).toBeNull();
  const separate = renderHook(() => useReportPreparation('track', source, cache, work.execute, true));
  view.unmount();
  expect(work.jobs[1].signal.aborted).toBe(true);
  expect(work.jobs[2].signal.aborted).toBe(false);
  await work.complete(1);
  expect(cache.peek(source)).toBeUndefined();
  await work.complete(2);
  expect(separate.result.current.status).toBe('ready');
});

it('never retains a previous cache scope or selection model during the new render', async () => {
  const cache = createReportPreparationCache(); const source = cache.source([card('Private')]); const work = controlled();
  const view = renderHook(({ key, owner }) => useReportPreparation(key, source, owner, work.execute, true), {
    initialProps: { key: 'a', owner: cache },
  });
  await work.complete(0);
  view.rerender({ key: 'b', owner: createReportPreparationCache() });
  expect(view.result.current).toMatchObject({ status: 'preparing', candidate: null });
});

it('memoizes the cache/executor adapter across unchanged renders and keeps empty success explicit', async () => {
  const cache = createReportPreparationCache(); const source = cache.source([card('Stable')]); const work = controlled();
  const view = renderHook(({ enabled }) => useReportPreparation('track', source, cache, work.execute, enabled), {
    initialProps: { enabled: true },
  });
  view.rerender({ enabled: true }); view.rerender({ enabled: true });
  expect(work.jobs).toHaveLength(1);
  expect(work.jobs[0].signal.aborted).toBe(false);
  await work.complete(0);
  const candidate = view.result.current.candidate;
  view.rerender({ enabled: true });
  expect(view.result.current.candidate).toEqual(candidate);
  expect(work.jobs).toHaveLength(1);
  const empty = cache.source([]);
  const emptyView = renderHook(() => useReportPreparation('empty', empty, cache, work.execute, false));
  expect(emptyView.result.current).toMatchObject({ status: 'ready', candidate: { source: empty, preparation: { kind: 'empty', report: null } } });
  expect(work.jobs).toHaveLength(1);
});
