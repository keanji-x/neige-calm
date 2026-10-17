import { IconButton } from '@astryxdesign/core/IconButton';
import styles from './meta.module.css';

function ActionIcon({ kind }: { kind: 'copy' | 'copied' | 'edit' | 'regenerate' | 'undo' | 'return' }) {
  return <svg className={styles.icon} viewBox="0 0 24 24" fill="none" stroke="currentColor"
    strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
    {kind === 'undo' ? <><path d="m9 4-5 5 5 5" /><path d="M4 9h10a6 6 0 0 1 0 12" /></> : kind === 'return' ? <><path d="M20 4v7a3 3 0 0 1-3 3H4" /><path d="m9 9-5 5 5 5" /></> : kind === 'copied' ? <path d="m5 12 4 4L19 6" /> : kind === 'copy' ? <><rect x="8" y="8" width="12" height="12" rx="2" /><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" /></>
      : kind === 'edit' ? <><path d="m16 3 5 5-12 12-6 1 1-6L16 3Z" /><path d="m14 5 5 5" /></>
        : <><path d="M20 7v5h-5" /><path d="M4 17v-5h5" /><path d="M6 7a7 7 0 0 1 12-1l2 3M18 17a7 7 0 0 1-12 1l-2-3" /></>}
  </svg>;
}


export type MessageAction = Readonly<{
  id: string;
  kind: 'copy' | 'copied' | 'edit' | 'regenerate' | 'undo' | 'return';
  label: string;
  tooltip?: string;
  disabled?: boolean;
  run: () => void;
}>;

/** The same glyph, size, color and hit target for response and queued-message actions. */
export function MessageActions({ actions, label = 'Response actions' }: Readonly<{
  actions: readonly MessageAction[]; label?: string;
}>) {
  return <div className={styles.actions} role="group" aria-label={label}>
    {actions.map(action => <IconButton key={action.id} label={action.label} tooltip={action.tooltip}
      icon={<ActionIcon kind={action.kind} />} className={styles.action} variant="ghost" size="sm"
      isDisabled={action.disabled} onClick={action.run} />)}
  </div>;
}
