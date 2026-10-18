import { useEffect, useRef, useSyncExternalStore } from 'react';
import type { CardWire, NewTerminalCardBody } from '../../../../../core/domain/track.ts';
import { CardDockHost, type BoardHostItem, type CardHost } from '../../../systems/cards/public.js';
import { useState } from '../../../ui/state/public.ts';
import { cardCreateEnded, cardCreateFailureText } from '../new-card/create.ts';
import styles from './workbench.module.css';
import { DrawerHeader } from '../../../ui/drawer/header.tsx';
import { useCompactViewport } from '../../../ui/viewport/public.ts';

type TerminalRequest = Readonly<{ key: string; body: NewTerminalCardBody }>;
export type TerminalWorkbenchSession = {
  held: TerminalRequest | null;
  pending: Promise<CardWire> | null;
  selected: string | null;
  epoch: number;
  error: string | null;
  subscribe: (listener: () => void) => () => void;
  getSnapshot: () => number;
  changed: () => void;
};

/** Signed-in, memory-only intents outlive route DOM; no global/canonical card identity. */
export function createTerminalWorkbenchSessions() {
  const tracks = new Map<string, TerminalWorkbenchSession>();
  return { forTrack: (trackId: string): TerminalWorkbenchSession => {
    let session = tracks.get(trackId);
    if (session === undefined) {
      const listeners = new Set<() => void>();
      let revision = 0;
      session = { held: null, pending: null, selected: null, epoch: 0, error: null,
        subscribe: listener => { listeners.add(listener); return () => { listeners.delete(listener); }; },
        getSnapshot: () => revision,
        changed: () => { revision += 1; listeners.forEach(listener => listener()); },
      };
      tracks.set(trackId, session);
    }
    return session;
  } };
}

/** Track owns selection and the held create intent; app injects transport and key/theme policy. */
export function useTerminalWorkbench({ trackId, cards, create, request, session, ordinaryCardOpen, onHide }: {
  trackId: string;
  ordinaryCardOpen: boolean;
  onHide: () => void;
  session: TerminalWorkbenchSession;
  cards: readonly CardWire[];
  create: (body: NewTerminalCardBody, key: string) => Promise<CardWire>;
  request: () => TerminalRequest;
}) {
  const [open, setOpen] = useState(false);
  useSyncExternalStore(session.subscribe, session.getSnapshot);
  const [visited, setVisited] = useState<readonly string[]>([]);
  const [focus, setFocus] = useState(false);
  const compact = useCompactViewport();
  // Once the ordinary compact board has opened, it retains ownership until desktop.
  const [ordinaryHost, setOrdinaryHost] = useState(false);
  if (ordinaryHost && !compact) setOrdinaryHost(false);
  if (!ordinaryHost && compact && ordinaryCardOpen) setOrdinaryHost(true);
  const compactRef = useRef(compact);
  compactRef.current = compact;
  useEffect(() => { if (compact) { session.epoch += 1; setFocus(false); } }, [compact, session]);
  const alive = useRef(true);
  const opener = useRef<HTMLElement | null>(null);
  const terminals = cards.filter(card => card.kind === 'terminal');
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, [trackId]);
  useEffect(() => {
    if (!open) return;
    const moved = (event: FocusEvent) => {
      if (event.target instanceof Element && event.target.closest('[data-nc-terminal-workbench]') === null) setFocus(false);
    };
    document.addEventListener('focusin', moved);
    return () => document.removeEventListener('focusin', moved);
  }, [open]);
  const select = (id: string) => {
    session.epoch += 1;
    session.selected = id;
    session.changed();
    setFocus(true);
  };
  const createOne = (returnFocus: HTMLElement | null = null) => {
    if (session.pending !== null) return;
    const resent = session.held !== null;
    const intent = session.held ?? request();
    session.held = intent;
    session.error = null;
    const origin = document.activeElement;
    const epoch = session.epoch;
    const operation = create(intent.body, intent.key);
    session.pending = operation;
    session.changed();
    void operation.then(card => {
      session.held = null;
      if (epoch === session.epoch) session.selected = card.id;
      if (!alive.current) return;
      // Completion never opens a hidden view or steals a later focus owner's caret.
      setFocus(!compactRef.current && epoch === session.epoch && (document.activeElement === origin || (returnFocus !== null && document.activeElement === returnFocus)));
    }, (cause: unknown) => {
      if (cardCreateEnded(cause, resent)) session.held = null;
      session.error = cardCreateFailureText('terminal', resent)(cause);
    }).finally(() => {
      if (session.pending === operation) session.pending = null;
      session.changed();
    });
  };
  const reveal = (id?: string) => {
    if (compactRef.current) return;
    if (session.pending === null) session.epoch += 1;
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setOpen(true);
    // An explicit inventory choice supersedes a pending create's selection,
    // while the held request keeps its original key/body until it settles.
    if (id !== undefined && terminals.some(card => card.id === id)) { select(id); return; }
    // Unknown outcomes retain the first body/key even if a refresh has found cards meanwhile.
    if (session.held !== null) { createOne(); return; }
    const candidate = id ?? terminals.find(card => card.id === session.selected)?.id ?? terminals[0]?.id;
    if (candidate !== undefined) select(candidate);
    else createOne();
  };
  const hide = () => { session.epoch += 1; setOpen(false); setFocus(false); onHide(); opener.current?.focus({ preventScroll: true }); };
  // Deletion chooses another existing option but never implicitly creates a replacement.
  const selectedId = terminals.some(card => card.id === session.selected) ? session.selected : terminals[0]?.id ?? null;
  // Every displayed selection, including list/deletion fallback, enters the retained set
  // before its host mounts. Unvisited terminals remain unattached.
  if (open && selectedId !== null && !visited.includes(selectedId)) setVisited([...visited, selectedId]);
  return { open, ordinaryHost, selectedId, cardIds: new Set(terminals.map(card => card.id)), visited, pending: session.pending !== null, canRetry: session.held !== null, error: session.error, focus, reveal, hide, select,
    suspend: () => { session.epoch += 1; setOpen(false); setFocus(false); onHide(); }, blur: () => setFocus(false), retry: () => { if (session.held !== null) createOne(); }, newSession: (returnFocus: HTMLElement | null) => { session.epoch += 1; opener.current = returnFocus; setOpen(true); createOne(returnFocus); } };
}

export type TerminalWorkbench = ReturnType<typeof useTerminalWorkbench>;

export function TerminalWorkbenchView({ workbench, items, host, visible, onRemove }: {
  workbench: TerminalWorkbench;
  items: readonly BoardHostItem[];
  host: CardHost;
  visible: boolean;
  onRemove: (id: string) => void;
}) {
  return <div className={styles.workbench} data-nc-terminal-workbench="" hidden={!visible} inert={!visible} aria-hidden={!visible || undefined}>
    {workbench.selectedId === null && <DrawerHeader title="Terminal" closeLabel="Hide terminal" onClose={workbench.hide} />}
    {items.filter(item => workbench.visited.includes(item.card.id)).map(item => <CardDockHost
      key={item.card.id} host={host} item={item} visible={visible && item.card.id === workbench.selectedId}
      focused={workbench.focus} onRemove={() => onRemove(item.card.id)} header={{ onHide: workbench.hide, hideLabel: 'Hide terminal' }} />)}
    {workbench.pending && <p role="status">Creating terminal…</p>}
    {workbench.error !== null && <p role="alert">{workbench.error} {workbench.canRetry && <button type="button" data-nc-action="tertiary" disabled={workbench.pending} onClick={workbench.retry}>Try again</button>}</p>}
  </div>;
}
