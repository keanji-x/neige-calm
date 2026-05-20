// ---------------- AddPanel ----------------
//
// Single "+" button → popover menu populated from `addPanelEntries()`. Each
// menu item carries an optional `createSchema`: items with no schema fall
// through to immediate kernel creation (today: terminal); items with a
// schema bubble back up to the Wave page, which renders an inline config
// card via `SchemaForm`. The two-step "menu → config card → submit"
// pattern keeps the visual style consistent across kinds and gives plugin
// authors a declarative knob to collect input.

import { useEffect, useRef, useState } from 'react';
import { addPanelEntries, type AddPanelMenuItem } from '../../cards/registry';

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

  // Snapshot the menu at mount — the registry is populated synchronously
  // at boot, so re-querying on every render only adds churn.
  const items = addPanelEntries();

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!wrapRef.current) return;
      if (e.target instanceof Node && wrapRef.current.contains(e.target)) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDoc);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  return (
    <div className="add-panel-wrap" ref={wrapRef}>
      <button
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
            items.map((item) => (
              <li key={item.type} role="none">
                <button
                  role="menuitem"
                  className="add-panel-menu-item"
                  onClick={() => {
                    setOpen(false);
                    onSelect(item);
                  }}
                >
                  {item.label}
                </button>
              </li>
            ))
          )}
        </ul>
      )}
    </div>
  );
}
