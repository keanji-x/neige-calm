import { conversationCommands, dispatchComposerCommand, executeComposerCommand, type ComposerCommand } from '../thread/composer-command.ts';
import type { Dispatch, SetStateAction } from 'react';
import { useState } from '../../../ui/state/public.ts';
import { floatingControlClassName } from '../../../ui/floating-control/public.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './dock.module.css';

/** A compact entry to the existing conversation; delivery and draft ownership stay with the caller. */
export function ChatDock({ text, onChange, onSend, disabled, onBeginInput, commands = conversationCommands({ compactViewport: true }) }: Readonly<{
  commands?: readonly ComposerCommand[];
  text: string;
  onChange: Dispatch<SetStateAction<string>>;
  onSend: (text: string) => void;
  onBeginInput: () => void;
  disabled: boolean;
}>) {
  const [commandError, setCommandError] = useState<string | null>(null);
  return <form className={`${styles.bar} ${floatingControlClassName}`} aria-label="Quick chat" data-nc-chat-dock="" tabIndex={-1} onSubmit={(event) => {
    event.preventDefault();
    if (disabled || text.trim() === '') return;
    setCommandError(null);
    if (dispatchComposerCommand(commands, text, (command, argument) => {
      void executeComposerCommand(command, argument).then(() => { onChange(current => current === text ? '' : current); }, (reason: unknown) => {
        setCommandError(reason instanceof Error ? reason.message : 'Command failed.');
      });
    })) return;
    onSend(text);
  }}>
    {commandError && <p role="alert">{commandError}</p>}
    <textarea className={styles.input} aria-label="Chat message" name="chat-message" onFocus={onBeginInput} placeholder="说说你想做什么…" rows={1}
      value={text} onChange={(event) => onChange(event.target.value)} />
    <button type="submit" className={styles.send} aria-label="Send chat message" disabled={disabled || text.trim() === ''}>
      <Icon name="arrow-up" />
    </button>
  </form>;
}
