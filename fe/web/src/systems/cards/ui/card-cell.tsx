import { useLayoutEffect, useRef, type ComponentType } from 'react';
import { useState } from '../../../ui/state/public.ts';
import type { CardComponentProps, RegisteredCard } from '../registry.js';
import type { CardHost, CardHostCapabilities, MountedCard } from '../host.js';
import type { CardActivity } from '../../../../../core/domain/activity.js';
import { CardHead } from './card-head.tsx';

export type BoardHostItem = Readonly<{
  card: RegisteredCard;
  title: string;
  originalIndex: number;
  /** The kernel's `deletable` bit; absent means deletable (older servers omit it, matching `cardWireSchema`'s `.default(true)`). */
  deletable?: boolean;
  /** The kernel's activity verdict, or `null` when the board has no overlay to read. */
  activity: CardActivity | null;
  /** The composer's one-line state for the card, or `null`; see `CardComponentProps.notice`. */
  notice: string | null;
}>;

function cardWithTitle(item: BoardHostItem): RegisteredCard {
  const card = item.card;
  if (!('title' in card)) return card;
  const current = (card as { title: string | null }).title;
  if (current !== null && current !== '') return card;
  return { ...card, title: item.title };
}

export function CardCell({ host, item, focused, visible, onRemove }: {
  host: CardHost;
  item: BoardHostItem;
  focused: boolean;
  visible: boolean;
  onRemove?: () => void;
}) {
  const elementRef = useRef<HTMLSpanElement | null>(null);
  const mountedRef = useRef<MountedCard | null>(null);
  const cardRef = useRef(item.card);
  cardRef.current = cardWithTitle(item);
  const [capabilities, setCapabilities] = useState<CardHostCapabilities | null>(null);
  const entry = host.registry.get(item.card.type);
  const Component: ComponentType<CardComponentProps> | undefined = entry?.component;
  const cardId = item.card.id;

  useLayoutEffect(() => {
    const mounted = host.mount(cardRef.current);
    mountedRef.current = mounted;
    setCapabilities(mounted.card);
    return () => {
      mounted.unmount();
      if (mountedRef.current === mounted) {
        mountedRef.current = null;
        setCapabilities(null);
      }
    };
  }, [host, cardId]);

  useLayoutEffect(() => {
    mountedRef.current?.host.setVisible(visible);
    mountedRef.current?.host.setFocused(focused);
  }, [host, cardId, visible, focused]);

  useLayoutEffect(() => {
    const element = elementRef.current?.parentElement;
    if (element == null) return;
    const measure = () => {
      if (!visible) return;
      const box = element.getBoundingClientRect();
      mountedRef.current?.host.setGeometry({ width: box.width, height: box.height, ready: box.width > 0 && box.height > 0 });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [host, cardId, visible, capabilities]);

  if (Component === undefined || capabilities === null) {
    return (
      <div className="term">
        {/* No entry claims an unknown card, so the × is drawn here rather than by the absent component. */}
        <CardHead
          className="card-drag-handle"
          title={item.title}
          onClose={onRemove}
          closeAriaLabel={`Delete card ${item.title}`}
        />
        <div className="term-body">
          <p className="term-line">{Component === undefined ? 'Unknown card' : item.title}</p>
        </div>
      </div>
    );
  }
  return <><span ref={elementRef} hidden /><Component card={cardRef.current} host={capabilities} onRemove={onRemove} activity={item.activity} notice={item.notice} /></>;
}
