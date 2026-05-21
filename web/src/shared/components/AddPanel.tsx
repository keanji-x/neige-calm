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
// AddPanel today is a THIN WRAPPER over the `Menu` primitive in
// `web/src/ui/Menu/`. AddPanel's only remaining responsibilities are:
//   1. Reading the registry snapshot of available item kinds.
//   2. Forwarding selection back to the caller (which decides between
//      schema-driven and direct-create flows).
//
// The keyboard / focus / outside-click contract is owned by `Menu`. See
// `web/src/ui/Menu/Menu.tsx` for the WAI-ARIA + focus-restore contract
// (in particular the synchronous-focus-restore-before-onSelect rule
// that lets onSelect open a Dialog without breaking focus return). The
// CSS class names below (`add-panel-wrap`, `add-panel`, `add-panel-menu`,
// `add-panel-menu-item`, `add-panel-empty`) are kept verbatim — the
// cleanup pass that renames them to primitive-neutral selectors lives
// in a separate PR.
//
// Backwards-compatible alias — `app/router.tsx` and `pages/Wave.tsx`
// still import `AddPanelMenuItem` and the widened `AddPanelKind` from
// this module. We re-export from here so call sites don't all need to
// switch to the registry import in this PR.

import { addPanelEntries, type AddPanelMenuItem } from '../../cards/registry';
import { Menu, type MenuItem } from '../../ui/Menu/Menu';

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
  // Snapshot the menu at mount — the registry is populated synchronously
  // at boot, so re-querying on every render only adds churn.
  const entries = addPanelEntries();

  // Map registry entries to the Menu primitive's MenuItem shape. We
  // close over each entry so the wrapper's `onSelect(entry)` signature
  // is preserved — the Menu primitive sees a parameter-less callback,
  // which is the cleaner generic shape.
  const menuItems: MenuItem[] = entries.map((entry) => ({
    label: entry.label,
    onSelect: () => onSelect(entry),
  }));

  return (
    <Menu
      items={menuItems}
      wrapClassName="add-panel-wrap"
      menuClassName="add-panel-menu"
      itemClassName="add-panel-menu-item"
      emptyClassName="add-panel-empty"
      emptyState="No card kinds registered"
      trigger={({ ref, onClick, 'aria-haspopup': ariaHasPopup, 'aria-expanded': ariaExpanded }) => (
        <button
          ref={ref}
          className="add-panel"
          onClick={onClick}
          aria-expanded={ariaExpanded}
          aria-haspopup={ariaHasPopup}
          title="Add card"
        >
          + Add
        </button>
      )}
    />
  );
}
