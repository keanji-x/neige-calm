// React context exposing the current track's row-level state (id,
// lifecycle) to descendant card components without threading props through
// every layer.
//
// Why: built-in cards (terminal, codex, plugin iframe) only need to
// know about *their* card row. The track-report card (#229 PR B) is
// different: its header renders the track's `lifecycle` badge so the
// user sees the same lifecycle vocabulary at both the page level and
// inside the report. Rather than retro-fitting every card-rendering
// helper (TrackGrid, TrackList, TrackCard) to pass `track` through, we
// publish the bare-minimum slice as context — only the track-report
// card reads it, and it falls back to `null` so headless tests don't
// need to wrap the component.
//
// Keep this context minimal — extending it for "everything a card
// might want" would couple cards to a far wider surface than they
// need.

import { createContext } from 'react';
import type { TrackLifecycle } from '../../types';

export interface TrackContextValue {
  /** Stable track id; useful for local-storage keys keyed per track. */
  id: string;
  /** Track's current lifecycle, fed straight from the kernel row. */
  lifecycle: TrackLifecycle;
}

/**
 * Default value is `null`: this lets a component decide between "no
 * track context provided, render a fallback" and "track context is
 * provided but its value is null" (we don't allow the latter, so the
 * `null` sentinel is unambiguous). Cards that opt in to this context
 * must handle the `null` branch explicitly.
 */
export const TrackContext = createContext<TrackContextValue | null>(null);
