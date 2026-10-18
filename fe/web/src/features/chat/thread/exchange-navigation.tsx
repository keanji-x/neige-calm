import { memo, useEffect, useMemo, useRef, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { markdownExcerpt } from '../../../../../core/markdown/public.ts';
import { isUserConversationTurn, opensExchange, type TranscriptEntry } from '../../../../../core/domain/conversation.ts';
import { scrollGeometry } from '../../../ui/drawer/scroll-geometry.ts';
import { EdgeNavigator } from '../../../ui/edge-navigation/public.tsx';
import { observeResize } from '../../../ui/edge-navigation/resize.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './thread.module.css';

/** The rail owns its current marker. Scrolling it does not render the transcript. */
export const ExchangeNavigation = memo(function ExchangeNavigation({ turns, frameRef, seam, navigate }: Readonly<{
  turns: readonly TranscriptEntry[];
  frameRef: RefObject<HTMLDivElement | null>;
  seam: HTMLElement | null;
  navigate: (position: () => void) => void;
}>) {
  const exchanges = useMemo(() => exchangesOf(turns), [turns]);
  const items = useMemo(() => exchanges.map((item, index) => ({
    id: item.id, title: item.text.trim() === '' ? `Exchange ${index + 1}` : item.text,
    readExcerpt: () => item.reply === null ? '' : markdownExcerpt(item.reply),
    label: `Jump to exchange ${index + 1}${railLabel(item.text) === '' ? '' : `: ${railLabel(item.text)}`}`,
  })), [exchanges]);
  const exchangeKey = useMemo(() => JSON.stringify(exchanges.map(exchange => exchange.id)), [exchanges]);
  const shown = exchanges.length > 0 && seam !== null;
  const [active, setActive] = useState<string | null>(null);
  const readActive = useRef<() => void>(() => {});

  /* The lit dot is the last exchange whose opening marker sits at or above an
     edge: the pane's top while a pane-height of scroll remains, sliding to the
     bottom as it runs out. Zero-height panes still install their listeners. */
  useEffect(() => {
    const frame = frameRef.current;
    if (!shown || frame === null) return;
    const scroller = frame.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (scroller === null) return;
    const markers = [...frame.querySelectorAll<HTMLElement>('[data-nc-exchange]')];
    if (markers.length === 0) return;

    const read = () => {
      if (scroller.clientHeight === 0) return;
      const pane = scrollGeometry(scroller);
      const remaining = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
      const slid = Math.min(scroller.scrollTop, Math.max(0, pane.height - remaining));
      const edge = Math.min(pane.height, ACTIVE_MARKER_SLACK_PX + slid);
      // Exchange markers follow vertical transcript flow, so preceding history
      // does not need a full geometry scan or a retained position cache.
      let low = 0; let high = markers.length;
      while (low < high) {
        const middle = Math.floor((low + high) / 2);
        if (pane.y(markers[middle].getBoundingClientRect().top) <= edge) low = middle + 1;
        else high = middle;
      }
      const id = markers[Math.max(0, low - 1)]?.dataset.ncExchange;
      if (id !== undefined) setActive(id);
    };
    readActive.current = read;
    let queued: number | null = null;
    const onScroll = () => {
      if (queued !== null) return;
      queued = requestAnimationFrame(() => { queued = null; read(); });
    };
    const unobserve = observeResize(scroller, read);
    scroller.addEventListener('scroll', onScroll, { passive: true });
    read();
    return () => {
      readActive.current = () => {};
      unobserve();
      scroller.removeEventListener('scroll', onScroll);
      if (queued !== null) cancelAnimationFrame(queued);
    };
  }, [shown, exchangeKey, frameRef]);

  return !shown || seam === null ? null : createPortal(
    <EdgeNavigator className={styles.edgeNavigation} label="Jump to an exchange" items={items} activeId={active}
      onSelect={id => {
        // Resolve before releasing follow intent: a missing exchange is a no-op.
        const target = exchangePosition(frameRef.current, id);
        if (target === null) return;
        navigate(target);
        setActive(id);
        // A clamped write emits no scroll, so immediately re-read the marker.
        readActive.current();
      }} />,
    seam,
  );
});

type Exchange = Readonly<{ id: string; text: string; reply: string | null }>;

// Absorb subpixel differences between requested and actual scroll positions.
const ACTIVE_MARKER_SLACK_PX = 4;
const RAIL_LABEL_MAX = 60;

function exchangesOf(turns: readonly TranscriptEntry[]): readonly Exchange[] {
  const found: Exchange[] = [];
  let open: { id: string; text: string; reply: string | null } | null = null;
  for (const [index, turn] of turns.entries()) {
    if (isUserConversationTurn(turn) && opensExchange(turns, index)) {
      if (open !== null) found.push(open);
      open = { id: turn.id, text: turn.text, reply: null };
    } else if (open !== null && open.reply === null && turn.author === 'agent') {
      open.reply = turn.text;
    }
  }
  if (open !== null) found.push(open);
  return found;
}

function railLabel(text: string): string {
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length <= RAIL_LABEL_MAX ? line : `${line.slice(0, RAIL_LABEL_MAX - 1)}…`;
}

function exchangePosition(frame: HTMLElement | null, id: string): (() => void) | null {
  if (frame === null) return null;
  const marker = [...frame.querySelectorAll<HTMLElement>('[data-nc-exchange]')]
    .find(candidate => candidate.dataset.ncExchange === id);
  if (marker === undefined) return null;
  const scroller = marker.closest<HTMLElement>('[data-nc-drawer-scroll]');
  if (scroller === null) return null;
  return () => { scroller.scrollTop += scrollGeometry(scroller).y(marker.getBoundingClientRect().top); };
}
