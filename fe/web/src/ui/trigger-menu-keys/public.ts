// Where Enter or Tab in a composer field goes while an Astryx trigger menu (`/`, `@`) may be open.
// DOM-only: it reads the combobox attributes `useTriggerMenu` publishes on the field.

export type TriggerMenuKeyRoute =
  /** An IME candidate is being accepted: neither the menu nor the composer may act on the key. */
  | 'composing'
  /** The menu picks the row it has rendered as highlighted. */
  | 'menu'
  /** The menu is searching. It still holds the last query's rows, so the key is dropped. */
  | 'swallow'
  /** Menu closed, or open with nothing to pick: the composer's own Enter, Tab and Shift+Enter. */
  | 'composer';

/**
 * In this order: composing; a rendered highlighted option; the menu's `role="status"` loading
 * row (Astryx's own "Searching…", the same for every trigger and source); anything else.
 */
export function triggerMenuKeyRoute(event: Pick<KeyboardEvent, 'isComposing'>, field: Element): TriggerMenuKeyRoute {
  if (event.isComposing) return 'composing';
  if (field.getAttribute('aria-expanded') !== 'true') return 'composer';
  const document = field.ownerDocument;
  const active = field.getAttribute('aria-activedescendant');
  if (active !== null && document.getElementById(active)?.getAttribute('role') === 'option') return 'menu';
  const menu = document.getElementById(field.getAttribute('aria-controls') ?? '');
  return menu?.querySelector('[role="status"]') == null ? 'composer' : 'swallow';
}
