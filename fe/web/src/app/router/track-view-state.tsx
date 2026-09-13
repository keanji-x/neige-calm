import { createContext, useContext, useLayoutEffect, type ReactNode } from 'react';
import { useRouterState } from '@tanstack/react-router';
import { useState } from '../../ui/state/public.ts';
import type { TrackSearch } from './track-search.ts';

type Offset = Readonly<{ top: number; left: number }>;
type TrackView = {
  search: TrackSearch;
  scroll: Map<string, Offset>;
};

function createTrackViews() {
  const views = new Map<string, TrackView>();
  return {
    get: (id: string) => views.get(id),
    ensure: (id: string) => {
      let view = views.get(id);
      if (view === undefined) {
        view = { search: {}, scroll: new Map() };
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

/** Called only after detail has arrived and the keyed track body mounts. */
export function useTrackViewState(trackId: string) {
  const views = useTrackViews();
  const search = useRouterState({ select: (state) => state.location.search as TrackSearch });
  const hash = useRouterState({ select: (state) => state.location.hash });
  const resumeBoard = useRouterState({ select: (state) => state.location.state.ncResumeTrackView === true });
  useLayoutEffect(() => {
    if (views !== null) {
      const { card, file, panel } = search;
      views.ensure(trackId).search = { card, file, panel };
    }
  }, [search, trackId, views]);
  // Hash navigation is an explicit destination, including on the same track.
  useLayoutEffect(() => {
    if (views === null) return;
    const view = views.ensure(trackId);
    const saved = new Map(view.scroll);
    let restoring = true;
    const nodes = new Map<string, HTMLElement>();
    const collect = () => {
      const page = document.querySelector<HTMLElement>('[data-nc-track-page]');
      const board = document.querySelector<HTMLElement>('[data-nc-card-board]');
      const panel = document.querySelector<HTMLElement>('[data-nc-panel]');
      if (page !== null) nodes.set('page', page);
      if (board !== null) nodes.set('board', board);
      if (panel !== null) nodes.set('panel', panel);
    };
    const restore = () => {
      collect();
      for (const [selector, node] of nodes) {
        const offset = saved.get(selector);
        if (offset !== undefined && hash === '' && (selector !== 'board' || resumeBoard)) {
          node.scrollTop = offset.top;
          node.scrollLeft = offset.left;
        }
      }
    };
    const remember = (event: Event) => {
      if (restoring) return;
      collect();
      for (const [key, node] of nodes) {
        if (event.target === node) view.scroll.set(key, { top: node.scrollTop, left: node.scrollLeft });
      }
    };
    restore();
    // The board reveals its selected card after two layout frames. Resume the
    // saved viewport after that reveal, then let subsequent user scrolls win.
    let frame = 0;
    frame = requestAnimationFrame(() => {
      frame = requestAnimationFrame(() => {
        frame = requestAnimationFrame(() => { restore(); restoring = false; });
      });
    });
    document.addEventListener('scroll', remember, true);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener('scroll', remember, true);
      if (!restoring) {
        for (const [selector, node] of nodes) {
          view.scroll.set(selector, { top: node.scrollTop, left: node.scrollLeft });
        }
      }
    };
  }, [hash, resumeBoard, trackId, views]);
}
