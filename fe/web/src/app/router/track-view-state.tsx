import { createTerminalWorkbenchSessions } from '../../features/track/workbench/public.tsx';
import { createContext, useContext, useLayoutEffect, useRef, type ReactNode } from 'react';
import { useRouterState } from '@tanstack/react-router';
import { useState } from '../../ui/state/public.ts';
import { createReportPreparationCache } from '../../features/report/document/preparation.ts';
import type { TrackSearch } from './track-search.ts';

type Offset = Readonly<{ top: number; left: number }>;
type TrackView = {
  search: TrackSearch;
  progressOpen: boolean;
  scroll: Map<string, Offset>;
};

function createTrackViews() {
  const views = new Map<string, TrackView>();
  return {
    reports: createReportPreparationCache(),
    terminals: createTerminalWorkbenchSessions(),
    get: (id: string) => views.get(id),
    ensure: (id: string) => {
      let view = views.get(id);
      if (view === undefined) {
        view = { search: {}, scroll: new Map(), progressOpen: false };
        views.set(id, view);
      }
      return view;
    },
  };
}

const TrackViewsContext = createContext<ReturnType<typeof createTrackViews> | null>(null);

/** Owned by the signed-in router tree; leaving a route disposes its resources,
 * but keeps its small UI snapshot. Nothing is written to browser storage. */
export function TrackViewProvider({ children }: { children: ReactNode }) {
  const [views] = useState(createTrackViews);
  return <TrackViewsContext.Provider value={views}>{children}</TrackViewsContext.Provider>;
}

export function useTrackViews() { return useContext(TrackViewsContext); }

/** The signed-in scope owns one data-only report cache. Execution stays injected. */
export function useTrackReportCache() {
  const views = useTrackViews();
  if (views === null) throw new Error('Track report preparation requires TrackViewProvider');
  return views.reports;
}

/** Detail/search are current immediately; report geometry waits for its committed content. */
export function useTrackViewState(trackId: string, reportReady: boolean) {
  const views = useTrackViews();
  const [progressOpen, setProgressOpen] = useState(() => views?.get(trackId)?.progressOpen ?? false);
  const search = useRouterState({ select: (state) => state.location.search as TrackSearch });
  const hash = useRouterState({ select: (state) => state.location.hash });
  const resumeBoard = useRouterState({ select: (state) => state.location.state.ncResumeTrackView === true });
  useLayoutEffect(() => {
    if (views !== null) {
      const { card, file, panel } = search;
      views.ensure(trackId).search = { card, file, panel };
    }
  }, [search, trackId, views]);
  useTrackScrollPort(views, trackId, 'page', reportReady, hash, resumeBoard);
  useTrackScrollPort(views, trackId, 'panel', true, hash, resumeBoard);
  useTrackScrollPort(views, trackId, 'board', true, hash, resumeBoard);
  return { open: progressOpen, onOpenChange: (open: boolean) => {
    if (views !== null) views.ensure(trackId).progressOpen = open;
    setProgressOpen(open);
  } };
}

type ScrollPort = 'page' | 'panel' | 'board';
function portNode(port: ScrollPort) {
  if (port === 'page') return document.querySelector<HTMLElement>('[data-nc-track-page]');
  if (port === 'panel') return document.querySelector<HTMLElement>('[data-nc-panel]');
  return document.querySelector<HTMLElement>('[data-nc-card-board]');
}

function useTrackScrollPort(views: ReturnType<typeof useTrackViews>, trackId: string, port: ScrollPort,
  ready: boolean, hash: string, resumeBoard: boolean) {
  const readerActed = useRef(false);
  // Input is observed from shell mount; placeholder scroll offsets are never saved.
  useLayoutEffect(() => {
    readerActed.current = false;
    const input = (event: Event) => {
      const node = portNode(port);
      if (node === null || !(event.target instanceof Node) || !node.contains(event.target)) return;
      const target = event.target instanceof Element ? event.target : event.target.parentElement;
      if (target?.closest('[data-nc-card-board], [data-nc-panel], [data-nc-track-page]') !== node) return;
      if (event.type === 'pointerdown' && target?.closest('button, a, input, textarea, select, [role="button"], [contenteditable="true"]') !== null) return;
      if (event instanceof KeyboardEvent) {
        if (!['PageDown', 'PageUp', 'Home', 'End', 'ArrowDown', 'ArrowUp', ' '].includes(event.key)) return;
        if (target?.closest('input, textarea, select, [contenteditable="true"]') !== null) return;
        if (event.key === ' ' && target?.closest('button, a, [role="button"]') !== null) return;
      }
      readerActed.current = true;
    };
    document.addEventListener('wheel', input, { capture: true, passive: true });
    document.addEventListener('touchmove', input, { capture: true, passive: true });
    document.addEventListener('pointerdown', input, true);
    document.addEventListener('keydown', input, true);
    return () => {
      document.removeEventListener('wheel', input, true);
      document.removeEventListener('touchmove', input, true);
      document.removeEventListener('pointerdown', input, true);
      document.removeEventListener('keydown', input, true);
    };
  }, [port, trackId]);
  useLayoutEffect(() => {
    if (views === null || !ready) return;
    const view = views.ensure(trackId);
    const saved = view.scroll.get(port);
    let restoring = true;
    let frame = 0;
    let observed = portNode(port);
    const restore = () => {
      const node = portNode(port);
      observed = node;
      if (node !== null && saved !== undefined && hash === '' && !readerActed.current && (port !== 'board' || resumeBoard)) {
        node.scrollTop = saved.top; node.scrollLeft = saved.left;
      }
    };
    const remember = (event: Event) => {
      const node = portNode(port);
      if (!restoring && node !== null && event.target === node) {
        observed = node; view.scroll.set(port, { top: node.scrollTop, left: node.scrollLeft });
      }
    };
    restore();
    if (port === 'board') {
      // BoardHost owns a two-frame selected-card reveal; only its resume waits for that owner.
      frame = requestAnimationFrame(() => { frame = requestAnimationFrame(() => {
        frame = requestAnimationFrame(() => { restore(); restoring = false; });
      }); });
    } else restoring = false;
    document.addEventListener('scroll', remember, true);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener('scroll', remember, true);
      if (!restoring && observed !== null) view.scroll.set(port, { top: observed.scrollTop, left: observed.scrollLeft });
    };
  }, [hash, port, ready, resumeBoard, trackId, views]);
}
