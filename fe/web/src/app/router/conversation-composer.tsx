import type { ComponentProps, Dispatch, SetStateAction } from 'react';
import { ChatComposer } from '../../features/chat/thread/public.tsx';
import { ChatDock } from '../../features/chat/dock/public.tsx';
import { useConversationComposerText, useNewConversationText } from '../conversations/public.tsx';

type TextTarget = Readonly<{ kind: 'card' | 'scope'; id: string }>;

function useText(target: TextTarget) {
  const existing = useConversationComposerText(target.kind === 'card' ? target.id : null);
  const unsent = useNewConversationText(target.kind === 'scope' ? target.id : null);
  return target.kind === 'card' ? existing : unsent;
}

/** Text changes subscribe here, below both pane controllers. Status and focus stay injected. */
export function ConversationComposer({ target, changeText, ...props }: Omit<ComponentProps<typeof ChatComposer>, 'draft'> & {
  target: TextTarget;
  changeText: Dispatch<SetStateAction<string>>;
}) {
  const text = useText(target);
  return <ChatComposer {...props} draft={{ text, onChange: changeText }} />;
}

export function ConversationDock({ target, ...props }: Omit<ComponentProps<typeof ChatDock>, 'text'> & { target: TextTarget }) {
  const text = useText(target);
  return <ChatDock {...props} text={text} />;
}
