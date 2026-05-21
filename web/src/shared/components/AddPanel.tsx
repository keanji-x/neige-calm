// ---------------- AddPanel ----------------
//
// Single "+" button → popover menu populated from `addPanelEntries()`. Each
// menu item carries an optional `createSchema`: items with no schema fall
// through to immediate kernel creation (today: terminal); items with a
// schema bubble back up to the Wave page, which renders an inline config
// card via `SchemaForm`. The two-step "menu → config card → submit"
// pattern keeps the visual style consistent across kinds and gives plugin
// authors a declarative knob to collect input.
//
// Keyboard contract (Slice 7 of #56)
// ----------------------------------
// The popover behaves as a WAI-ARIA menu:
//   - Trigger is a real <button> with `aria-haspopup="menu"` +
//     `aria-expanded`. Enter / Space opens; Tab moves past it when closed.
//   - On open, focus moves to the first menuitem (so arrow navigation
//     works immediately) and the trigger's last-focused position is
//     stashed so we can restore.
//   - Inside the menu: ArrowUp/Down cycle (with wrap), Home/End jump to
//     ends, single letters jump by first-match typeahead, Enter/Space
//     activate, Escape closes.
//   - On close (Escape, activation, or outside click) focus returns to
//     the trigger button. Symmetric with the Modal focus contract — the
//     menu owns the restore so callers don't have to.
//
// All of the above is driven by `useRovingTabindex` (see
// `web/src/hooks/useRovingTabindex.ts`). The integration here is the
// "open / close lifecycle" half; the hook is the "in-composite key
// handling" half.

import { useEffect, useRef } from 'react';
import { useState } from '../state';
import { addPanelEntries, type AddPanelMenuItem } from '../../cards/registry';
import { useRovingTabindex } from '../../hooks/useRovingTabindex';

export type { AddPanelMenuItem } from '../../cards/registry';

// Backwards-compatible alias — `app/router.tsx` and `pages/Wave.tsx` still
// import this name. Now widened to `string` because plugin-driven kinds
// can appear in the menu too. The router casts back to its own dispatch
// table on receipt.
export type AddPanelKind = string;

export function AddPanel({
  onSelect,
}: {
  /** Callback fired when the user picks a menu entry. The caller decides
   *  whether to open a config card (schema-driven flow) or create the
   *  kind directly (no-config flow). */
  onSelect: (item: AddPanelMenuItem) => void;
}) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  // Snapshot the menu at mount — the registry is populated synchronously
  // at boot, so re-querying on every render only adds churn.
  const items = addPanelEntries();

  // Close helper that also restores focus to the trigger. We funnel both
  // outside-click and activation paths through this so the focus-restore
  // contract is one-stop. Escape goes through the hook's onEscape, which
  // calls back here too.
  //
  // The focus call is *synchronous* — not deferred — and that's load-
  // bearing: when activation opens a Modal in response to onSelect, the
  // Modal's mount-time effect snapshots `document.activeElement` as its
  // `previouslyFocusedRef`. If we focused the trigger via a microtask,
  // the Modal would race us and snapshot the about-to-unmount menuitem
  // instead — which means the Modal's own Escape-close restore would
  // noop (its target is no longer in the DOM) and focus would fall to
  // `<body>`. Focusing synchronously here means the trigger is `:focus`
  // before `onSelect` returns, so the Modal sees it as the previously-
  // focused element and restores back to it on close.
  //
  // The trigger button is the *sibling* of the menu in the DOM tree, so
  // focusing it while the menu is still rendered is safe — the menu
  // unmounts on the next render but doesn't interfere with the focus
  // call beforehand.
  const closeAndRestoreFocus = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  const activate = (index: number) => {
    const item = items[index];
    if (!item) return;
    // Close first (restores focus to trigger), then fire onSelect. The
    // caller may push a modal in response; the modal's `previously-
    // focused element` capture sees the trigger, so closing the modal
    // returns the user to where they started. Symmetric with the rename
    // and modal slices.
    closeAndRestoreFocus();
    onSelect(item);
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
  // item is focused (and the typeahead buffer starts fresh — the hook's
  // own buffer is on a ref and clears on its 500ms idle timer; that's
  // fine for the open-close cycle since we explicitly clear via the
  // index reset). When it closes we do nothing — focus restore is
  // already handled by `closeAndRestoreFocus`.
  useEffect(() => {
    if (open && items.length > 0) {
      setActiveIndex(0);
    }
    // Intentionally not depending on `setActiveIndex` — its identity is
    // stable across renders (useCallback inside the hook) and including
    // it would chase its identity rather than the `open` transition.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, items.length]);

  // Outside-click closes the menu. Escape is owned by the hook (it fires
  // when focus is inside the menu), but Escape elsewhere on the page is
  // not our concern — the menu only owns its own subtree.
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!wrapRef.current) return;
      if (e.target instanceof Node && wrapRef.current.contains(e.target)) return;
      // Outside click: close, but do *not* restore focus to the trigger
      // — the user explicitly clicked elsewhere, and yanking focus back
      // would be hostile. (Symmetric with Modal's overlay-click handler,
      // which also lets the dismissive gesture lose focus to <body>.)
      setOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    return () => {
      document.removeEventListener('mousedown', onDoc);
    };
  }, [open]);

  return (
    <div className="add-panel-wrap" ref={wrapRef}>
      <button
        ref={triggerRef}
        className="add-panel"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        aria-haspopup="menu"
        title="Add card"
      >
        + Add
      </button>
      {open && (
        <ul className="add-panel-menu" role="menu">
          {items.length === 0 ? (
            <li className="add-panel-empty">No card kinds registered</li>
          ) : (
            items.map((item, index) => {
              const { ref, tabIndex, onKeyDown } = getItemProps(index);
              const isActive = index === activeIndex;
              return (
                <li key={item.type} role="none">
                  <button
                    ref={ref}
                    role="menuitem"
                    className={
                      'add-panel-menu-item' + (isActive ? ' is-active' : '')
                    }
                    tabIndex={tabIndex}
                    onKeyDown={onKeyDown}
                    onClick={() => activate(index)}
                    onMouseEnter={() => setActiveIndex(index)}
                  >
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
