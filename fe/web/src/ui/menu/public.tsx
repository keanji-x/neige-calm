import { Fragment, useEffect, useRef, type ReactNode } from 'react';
import { useSurfacePresence } from '../motion/surface-presence.ts';
import { readSurfaceFrame } from '../motion/geometry.ts';
import { useState } from '../state/public.ts';
import { useRovingTabindex } from '../focus/public.ts';

export interface MenuItem { label: string; labelContent?: ReactNode; onSelect: () => void; disabled?: boolean; icon?: ReactNode; current?: boolean; separatorBefore?: boolean }
export interface MenuTriggerProps { ref: (element: HTMLButtonElement | null) => void; onClick: () => void; 'aria-haspopup': 'menu'; 'aria-expanded': boolean }
export interface MenuProps {
  items: readonly MenuItem[];
  trigger: (props: MenuTriggerProps) => ReactNode;
  wrapClassName?: string; menuClassName?: string; itemClassName?: string;
  emptyState?: ReactNode; emptyClassName?: string;
  /** Optional visual class for the semantic boundary before an action group. */
  separatorClassName?: string;
}

export function Menu({ items, trigger, wrapClassName, menuClassName, itemClassName, emptyState, emptyClassName, separatorClassName }: MenuProps) {
  const [open, setOpen] = useState(false);
  const [retained, setRetained] = useState(false);
  const anchor = useRef<HTMLSpanElement | null>(null);
  const placement = useRef<HTMLDivElement | null>(null);
  const surface = useRef<HTMLUListElement | null>(null);
  const present = open || retained;
  useSurfacePresence({ parts: { kind: 'single', primary: { element: surface, readFrame: () => readSurfaceFrame(placement.current) } },
    geometry: { kind: 'lift', origin: { kind: 'anchor', anchor: { ref: anchor }, unavailable: 'surface-center' } },
    open, present, enabled: true, onExited: () => setRetained(false) });
  const wrapperRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const closeAndRestoreFocus = () => { setOpen(false); triggerRef.current?.focus(); };
  const activate = (index: number) => {
    const item = items[index];
    if (!open || !item || item.disabled) return;
    closeAndRestoreFocus();
    item.onSelect();
  };
  const { activeIndex, setActiveIndex, getItemProps } = useRovingTabindex<HTMLButtonElement>({
    itemCount: items.length, onActivate: activate, onEscape: closeAndRestoreFocus,
    getLabel: (index) => items[index]?.label ?? '',
  });
  useEffect(() => { if (open && items.length > 0) setActiveIndex(0); }, [items.length, open, setActiveIndex]);
  useEffect(() => {
    if (!open) return;
    const onMouseDown = (event: MouseEvent) => {
      if (event.target instanceof Node && !wrapperRef.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener('mousedown', onMouseDown);
    return () => document.removeEventListener('mousedown', onMouseDown);
  }, [open, setOpen]);
  return <div className={wrapClassName} ref={wrapperRef}>
    <span ref={anchor} style={{ display: 'inline-flex', maxWidth: '100%' }}>{trigger({ ref: (element) => { triggerRef.current = element; }, onClick: () => { setRetained(true); setOpen((value) => !value); }, 'aria-haspopup': 'menu', 'aria-expanded': open })}</span>
    {present && <div ref={placement} className={menuClassName} inert={!open} aria-hidden={!open || undefined}
      style={{ display: 'grid', padding: 0, border: 0, background: 'transparent', boxShadow: 'none', overflow: 'visible' }}>
    <ul ref={surface} className={menuClassName} role="menu" inert={!open} aria-hidden={!open || undefined}
      style={{ position: 'relative', inset: 'auto', translate: 'none', width: '100%', maxHeight: 'inherit', margin: 0 }}>
      {items.length === 0 ? <li className={emptyClassName}>{emptyState}</li> : items.map((item, index) => {
        const props = getItemProps(index);
        return <Fragment key={`${item.label}:${index}`}>
          {item.separatorBefore && <li role="separator" className={separatorClassName} />}
          <li role="none"><button
          ref={props.ref} type="button" role="menuitem" tabIndex={props.tabIndex}
          className={`${itemClassName ?? ''}${index === activeIndex ? ' is-active' : ''}`.trim() || undefined}
          onKeyDown={open ? props.onKeyDown : undefined} onClick={() => activate(index)} onMouseMove={() => { if (open) setActiveIndex(index); }}
          aria-current={item.current ? 'page' : undefined}
          aria-disabled={item.disabled || undefined} aria-label={item.labelContent === undefined ? undefined : item.label}>{item.icon}{item.labelContent ?? item.label}</button></li>
        </Fragment>;
      })}
    </ul></div>}
  </div>;
}
