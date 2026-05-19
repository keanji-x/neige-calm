// ---------------- AddPanel ----------------

export type AddPanelKind = 'terminal' | 'doc' | 'plan';

export function AddPanel({
  onAdd,
}: {
  onAdd: (type: AddPanelKind) => void;
  /** Carried for API stability; ignored while only `terminal` is wired. */
  hasPlan?: boolean;
}) {
  // While the plugin host is still M3 work, only the built-in `terminal`
  // card is actually wired end-to-end. Showing menu items for `doc` /
  // `plan` would be a promise we can't keep, so the affordance collapses
  // to a single direct-action button. When plugins land we'll restore the
  // multi-option menu (driven by the manifest list rather than hard-coded).
  return (
    <button
      className="add-panel"
      onClick={() => onAdd('terminal')}
      title="New terminal"
    >
      + New terminal
    </button>
  );
}
