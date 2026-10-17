import { terminalCommand, TERMINAL_DESKTOP_MESSAGE } from '../../../../../core/domain/terminal-command.ts';
import { useState } from '../../../ui/state/public.ts';
import { floatingControlClassName } from '../../../ui/floating-control/public.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './dock.module.css';

/** A compact entry to the existing conversation; delivery and draft ownership stay with the caller. */
export function ChatDock({ text, onChange, onSend, disabled, onBeginInput }: Readonly<{
  text: string;
  onChange: (text: string) => void;
  onSend: (text: string) => void;
  onBeginInput: () => void;
  disabled: boolean;
}>) {
  const [commandError, setCommandError] = useState(false);
  return <form className={`${styles.bar} ${floatingControlClassName}`} aria-label="Quick chat" data-nc-chat-dock="" tabIndex={-1} onSubmit={(event) => {
    event.preventDefault();
    if (disabled || text.trim() === '') return;
    const command = terminalCommand(text);
    if (command !== null) { setCommandError(true); return; }
    setCommandError(false);
    onSend(text);
  }}>
    {commandError && <p role="alert">{TERMINAL_DESKTOP_MESSAGE}</p>}
    <textarea className={styles.input} aria-label="Chat message" name="chat-message" onFocus={onBeginInput} placeholder="说说你想做什么…" rows={1}
      value={text} onChange={(event) => onChange(event.target.value)} />
    <button type="submit" className={styles.send} aria-label="Send chat message" disabled={disabled || text.trim() === ''}>
      <Icon name="arrow-up" />
    </button>
  </form>;
}
