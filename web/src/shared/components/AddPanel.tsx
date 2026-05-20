// ---------------- AddPanel ----------------
//
// Single "+" button → popover menu populated from `addPanelEntries()`. Each
// menu item carries an optional `createSchema`: items with no schema fall
// through to immediate kernel creation (today: terminal); items with a
// schema bubble back up to the Wave page, which renders an inline config
// card via `SchemaForm`. The two-step "menu → config card → submit"
// pattern keeps the visual style consistent across kinds and gives plugin
// authors a declarative knob to collect input.

import { useEffect, useMemo, useRef } from 'react';
import { useState } from '../state';
import { addPanelEntries, type AddPanelMenuItem } from '../../cards/registry';
import type { PluginViewCatalogEntry } from '../../api/wire';

export type { AddPanelMenuItem } from '../../cards/registry';

// Backwards-compatible alias — `app/router.tsx` and `pages/Wave.tsx` still
// import this name. Now widened to `string` because plugin-driven kinds
// can appear in the menu too. The router casts back to its own dispatch
// table on receipt.
export type AddPanelKind = string;

/** Sentinel `item.type` prefix for plugin entries. Wave's `beginAdd`
 *  branches on this to dispatch to the plugin-tool-call create path
 *  instead of the static-kind switch. */
export const PLUGIN_TYPE_PREFIX = 'plugin:tool:';

/** Build the `AddPanelMenuItem.type` for a plugin entry. */
function pluginItemType(pluginId: string, toolName: string): string {
  return `${PLUGIN_TYPE_PREFIX}${pluginId}/${toolName}`;
}

/** Parse a plugin-entry `item.type` back into `{ pluginId, toolName }`,
 *  returning null for non-plugin items. */
export function parsePluginItemType(
  type: string,
): { pluginId: string; toolName: string } | null {
  if (!type.startsWith(PLUGIN_TYPE_PREFIX)) return null;
  const rest = type.slice(PLUGIN_TYPE_PREFIX.length);
  const slash = rest.indexOf('/');
  if (slash <= 0 || slash === rest.length - 1) return null;
  return { pluginId: rest.slice(0, slash), toolName: rest.slice(slash + 1) };
}

export function AddPanel({
  onSelect,
  pluginViews,
}: {
  /** Callback fired when the user picks a menu entry. The caller decides
   *  whether to open a config card (schema-driven flow) or create the
   *  kind directly (no-config flow). */
  onSelect: (item: AddPanelMenuItem) => void;
  /** Slice G: views from currently-enabled plugins. Each entry with a
   *  resolved `creator_tool` becomes a menu item; the caller (router)
   *  dispatches them via the `via_tool_call` create path. */
  pluginViews?: PluginViewCatalogEntry[];
}) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);

  // Merge static built-in entries with the live plugin catalog. Built-ins
  // first (stable position); plugin entries appended in catalog order.
  const items = useMemo<AddPanelMenuItem[]>(() => {
    const builtins = addPanelEntries();
    const plugins = (pluginViews ?? [])
      .filter((v) => !!v.creator_tool)
      .map((v) => ({
        type: pluginItemType(v.plugin_id, v.creator_tool as string),
        label: v.title,
        icon: v.icon ?? undefined,
      }));
    return [...builtins, ...plugins];
  }, [pluginViews]);

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
