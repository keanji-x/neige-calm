import { useEffect, useRef, type ReactNode } from 'react';
import { useState } from '../state/public.ts';
import { useRovingTabindex } from '../focus/public.ts';

export interface MenuItem { label: string; onSelect: () => void; disabled?: boolean; icon?: ReactNode }
export interface MenuTriggerProps { ref: (element: HTMLButtonElement | null) => void; onClick: () => void; 'aria-haspopup': 'menu'; 'aria-expanded': boolean }
export interface MenuProps {
  items: readonly MenuItem[];
  trigger: (props: MenuTriggerProps) => ReactNode;
  wrapClassName?: string; menuClassName?: string; itemClassName?: string;
  emptyState?: ReactNode; emptyClassName?: string;
}

export function Menu({ items, trigger, wrapClassName, menuClassName, itemClassName, emptyState, emptyClassName }: MenuProps) {
  const [open, setOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const closeAndRestoreFocus = () => { setOpen(false); triggerRef.current?.focus(); };
  const activate = (index: number) => {
    const item = items[index];
    if (!item || item.disabled) return;
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
    {trigger({ ref: (element) => { triggerRef.current = element; }, onClick: () => setOpen((value) => !value), 'aria-haspopup': 'menu', 'aria-expanded': open })}
    {open && <ul className={menuClassName} role="menu">
      {items.length === 0 ? <li className={emptyClassName}>{emptyState}</li> : items.map((item, index) => {
        const props = getItemProps(index);
        return <li key={`${item.label}:${index}`} role="none"><button
          ref={props.ref} type="button" role="menuitem" tabIndex={props.tabIndex}
          className={`${itemClassName ?? ''}${index === activeIndex ? ' is-active' : ''}`.trim() || undefined}
          onKeyDown={props.onKeyDown} onClick={() => activate(index)} onMouseMove={() => setActiveIndex(index)}
          aria-disabled={item.disabled || undefined}>{item.icon}{item.label}</button></li>;
      })}
    </ul>}
  </div>;
}
