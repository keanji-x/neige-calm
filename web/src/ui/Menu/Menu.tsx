// Reusable menu — popover composite implementing the WAI-ARIA "menu"
// pattern (button trigger that opens a list of menuitems). Extracted
// from `shared/components/AddPanel` so that the trigger/popover/keyboard
// machinery can be reused at future call sites without each one re-
// rolling the roving-tabindex + open-lifecycle dance.
//
// Shape
// -----
//   <Menu
//     items={[...]}
//     trigger={(triggerProps) => (
//       <button {...triggerProps}>+ Add</button>
//     )}
//   />
//
// The render-prop trigger pattern (rather than a compositional
// `<Menu.Trigger>` subcomponent) keeps the caller in charge of the
// trigger element's class, label, and arbitrary extra props — AddPanel's
// trigger needs `className="add-panel"` and a `title` tooltip; future
// call sites may need icon buttons. The Menu only owns the ARIA bits
// (`aria-haspopup`, `aria-expanded`), the ref, and the click handler;
// callers spread the rest onto whatever DOM they want.
//
// Keyboard contract
// -----------------
// Mirrors the WAI-ARIA APG menu pattern documented in
// `docs/a11y-contract.md`:
//   - Trigger: Enter / Space / click opens; Tab steps past when closed.
//   - On open, focus moves to the first menuitem.
//   - In-menu: ArrowUp/Down cycle (wrap), Home/End jump, single-letter
//     typeahead, Enter/Space activate, Escape closes.
//   - On close via Escape or activation: focus returns to the trigger.
//   - On close via outside click: focus is NOT restored to the trigger
//     — the user gestured elsewhere intentionally.
//
// All of the in-composite key handling is delegated to
// `useRovingTabindex`; this file owns the open/close lifecycle.
//
// Synchronous-focus-restore contract (LOAD-BEARING)
// -------------------------------------------------
// When the user activates a menuitem we do TWO things in a single
// synchronous tick, in this order:
//
//   1. Move focus back to the trigger button.
//   2. Call `item.onSelect()`.
//
// This ordering is a contract, not an incidental implementation detail.
// If `onSelect` opens a Dialog, the Dialog's "snapshot the previously-
// focused element" effect must see the trigger as `document.activeElement`
// — otherwise the Dialog's own close-time focus-restore would target a
// menuitem that no longer exists in the DOM, and focus would fall to
// `<body>`. Doing both calls synchronously means the trigger is `:focus`
// before `onSelect` returns, so the Dialog snapshots correctly.
//
// The trigger button is the *sibling* of the menu in the DOM tree, so
// focusing it while the menu is still rendered is safe — the menu
// unmounts on the next render but doesn't interfere with the focus call.
//
// See `Menu.contract.test.tsx` for the executable lock of this contract.

import { useEffect, useRef, type ReactNode } from 'react';
import { useState } from '../../shared/state';
import { useRovingTabindex } from '../hooks/useRovingTabindex';

export interface MenuItem {
  /** Human-readable label. Doubles as the accessible name of the
   *  menuitem, and is what the typeahead matches against. */
  label: string;
  /** Activation callback. Fired AFTER focus has been moved back to the
   *  trigger button. If this opens a Dialog, the Dialog will correctly
   *  snapshot the trigger as the focus-restore target. */
  onSelect: () => void;
  /** Optional disabled flag. Disabled items are rendered but skipped by
   *  activation. (Roving navigation still visits them — matches WAI-ARIA
   *  APG: disabled menuitems remain focusable so keyboard users can read
   *  them.) */
  disabled?: boolean;
  /** Optional leading icon. */
  icon?: ReactNode;
}

export interface MenuTriggerProps {
  ref: React.RefCallback<HTMLButtonElement>;
  onClick: () => void;
  'aria-haspopup': 'menu';
  'aria-expanded': boolean;
}

export interface MenuProps {
  /** Menu entries. */
  items: MenuItem[];
  /** Render prop for the trigger element. The caller chooses the tag,
   *  class, and label; the Menu provides the ARIA + ref + onClick. */
  trigger: (triggerProps: MenuTriggerProps) => ReactNode;
  /** CSS class for the outer wrapper (the element that owns the
   *  trigger AND popover, used for outside-click detection). */
  wrapClassName?: string;
  /** CSS class for the popover <ul role="menu">. */
  menuClassName?: string;
  /** CSS class for each menu item <button role="menuitem">. The active
   *  (roving-focused) item also gets ` is-active` appended. */
  itemClassName?: string;
  /** Rendered inside the popover when `items` is empty. */
  emptyState?: ReactNode;
  /** CSS class for the empty-state <li>. */
  emptyClassName?: string;
}

export function Menu({
  items,
  trigger,
  wrapClassName,
  menuClassName,
  itemClassName,
  emptyState,
  emptyClassName,
}: MenuProps) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  // Close helper that also restores focus to the trigger. Both Escape
  // (via the hook's onEscape) and activation go through this path so
  // the focus-restore contract is funneled through a single function.
  //
  // The focus call is *synchronous* — see the file header. Do not defer.
  const closeAndRestoreFocus = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  const activate = (index: number) => {
    const item = items[index];
    if (!item || item.disabled) return;
    // Close (synchronously restores focus to trigger), then fire
    // onSelect. The caller may push a Dialog in response; the Dialog's
    // previously-focused-element snapshot sees the trigger button.
    closeAndRestoreFocus();
    item.onSelect();
  };

  const { activeIndex, setActiveIndex, getItemProps } =
    useRovingTabindex<HTMLButtonElement>({
      itemCount: items.length,
      initialIndex: 0,
      loop: true,
      onActivate: activate,
      onEscape: closeAndRestoreFocus,
      getLabel: (i) => items[i]?.label ?? '',
    });

  // When the menu opens, snap the active index back to 0 so the first
  // item is focused. The hook's typeahead buffer is on a ref and clears
  // on its idle timer; that's fine for the open-close cycle.
  useEffect(() => {
    if (open && items.length > 0) {
      setActiveIndex(0);
    }
    // Intentionally not depending on `setActiveIndex` — its identity is
    // stable across renders (useCallback inside the hook) and including
    // it would chase its identity rather than the `open` transition.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, items.length]);

  // Outside-click closes the menu, but does NOT restore focus — the
  // user gestured elsewhere intentionally. (Symmetric with Dialog's
  // overlay-click handler.)
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!wrapRef.current) return;
      if (e.target instanceof Node && wrapRef.current.contains(e.target)) return;
      setOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    return () => {
      document.removeEventListener('mousedown', onDoc);
    };
  }, [open]);

  const triggerProps: MenuTriggerProps = {
    ref: (el) => {
      triggerRef.current = el;
    },
    onClick: () => setOpen((v) => !v),
    'aria-haspopup': 'menu',
    'aria-expanded': open,
  };

  return (
    <div className={wrapClassName} ref={wrapRef}>
      {trigger(triggerProps)}
      {open && (
        <ul className={menuClassName} role="menu">
          {items.length === 0 ? (
            <li className={emptyClassName}>{emptyState}</li>
          ) : (
            items.map((item, index) => {
              const { ref, tabIndex, onKeyDown } = getItemProps(index);
              const isActive = index === activeIndex;
              const className =
                (itemClassName ?? '') + (isActive ? ' is-active' : '');
              return (
                <li key={item.label + ':' + index} role="none">
                  <button
                    ref={ref}
                    type="button"
                    role="menuitem"
                    className={className.trim() || undefined}
                    tabIndex={tabIndex}
                    onKeyDown={onKeyDown}
                    onClick={() => activate(index)}
                    onMouseEnter={() => setActiveIndex(index)}
                    aria-disabled={item.disabled || undefined}
                  >
                    {item.icon}
                    {item.label}
                  </button>
                </li>
              );
            })
          )}
        </ul>
      )}
    </div>
  );
}
