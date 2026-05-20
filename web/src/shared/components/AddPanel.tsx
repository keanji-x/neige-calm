// ---------------- AddPanel ----------------

export type AddPanelKind = 'terminal';

export function AddPanel({
  onAdd,
}: {
  onAdd: (type: AddPanelKind) => void;
}) {
  // Only the built-in `terminal` card is wired end-to-end today. When the
  // plugin host (M3) lands, restore a multi-option menu driven by the
  // plugin manifest rather than hard-coded kinds.
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
