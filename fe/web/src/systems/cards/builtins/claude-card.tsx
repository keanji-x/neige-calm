// #2516: the Claude card's view. A Claude card is a PTY like a terminal card; an owner-created one also offers, at the right
// of its head, Update (its child runs a Claude other than the installed one) or Resume (its session has ended).

import { useEffect, useRef } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import {
  CLAUDE_RESTART_FAILURES, CLAUDE_RESTART_TEXT, claudeCardAction, type ClaudeCardAction, type ClaudeCardPort,
} from '../../../../../core/domain/claude-card.ts';
import { readWriteFailure } from '../../../../../core/domain/failure-class.ts';
import { useOperationFeedback } from '../../../ui/operation-feedback/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import type { CardComponentProps } from '../registry.js';
import { TerminalCardView } from './terminal-card.tsx';
import type { TerminalCard } from './terminal.ts';

export type ClaudeCard = Readonly<{
  type: 'claude';
  id: string;
  title: string | null;
  terminalId: string | null;
  sessionState: TerminalCard['sessionState'];
  cwd: string | null;
  gateCwd: string | null;
  /** Created by the owner (the add menu); a task worker's card is not, and offers neither Update nor Resume (#2516). */
  ownerCreated: boolean;
  /** The card's current runtime; a restart starts a new one. */
  workerSessionId: string | null;
  /** The Claude version the card's child started on, when the kernel recorded one. */
  runningVersion: string | null;
}>;

/** Lowercase to match the terminal card's own `'terminal'` fallback; `LetterAvatar` lowercases the title itself before picking the avatar colour. */
const CLAUDE_FALLBACK_TITLE = 'claude';

/** One server answer every Claude card shares; refetched after an on-demand refresh, never on its own. */
const INSTALLED_VERSION_KEY = Object.freeze(['claude', 'installed-version'] as const);
/** One on-demand refresh at a time across the cards: concurrent wakes join the one in flight. */
const REFRESH_KEY = Object.freeze(['claude', 'installed-version', 'refresh'] as const);

const ACTION_LABEL = Object.freeze({ update: 'Update', resume: 'Resume' } satisfies Record<ClaudeCardAction, string>);
const PENDING_LABEL = Object.freeze({ update: 'Updating…', resume: 'Resuming…' } satisfies Record<ClaudeCardAction, string>);

export function ClaudeCardView(props: CardComponentProps<ClaudeCard>) {
  const { card, host } = props;
  // A host built without the port (a test, a preview) offers no actions. Which card offers which is `claudeCardAction`'s.
  const port = host.claude ?? null;
  return TerminalCardView({
    ...props,
    fallbackTitle: CLAUDE_FALLBACK_TITLE,
    headActions: port === null ? null : <ClaudeCardActions card={card} port={port} />,
    // A restart starts a new runtime on the same terminal: attach to its child afresh.
    attachKey: card.workerSessionId,
  });
}

/** The page regained focus or became visible: ask the server to read the CLI again, then read its answer. */
function useRefreshOnWake(port: ClaudeCardPort): void {
  const client = useQueryClient();
  useEffect(() => {
    const wake = () => {
      if (document.visibilityState === 'hidden') return;
      void client.fetchQuery({
        queryKey: REFRESH_KEY,
        queryFn: async () => { await port.refreshInstalledVersion(); return null; },
        staleTime: 0,
        retry: false,
      }).then(
        () => client.invalidateQueries({ queryKey: INSTALLED_VERSION_KEY, exact: true }),
        () => undefined,
      );
    };
    window.addEventListener('focus', wake);
    document.addEventListener('visibilitychange', wake);
    return () => {
      window.removeEventListener('focus', wake);
      document.removeEventListener('visibilitychange', wake);
    };
  }, [client, port]);
}

function ClaudeCardActions({ card, port }: { card: ClaudeCard; port: ClaudeCardPort }) {
  const installed = useQuery({
    queryKey: INSTALLED_VERSION_KEY,
    queryFn: ({ signal }) => port.installedVersion(signal),
    staleTime: Infinity, retry: false, refetchOnWindowFocus: false,
  });
  useRefreshOnWake(port);
  const feedback = useOperationFeedback();
  const [pending, setPending] = useState<ClaudeCardAction | null>(null);
  // The state lands a render late; a second click before it must not send a second restart.
  const inFlight = useRef(false);
  const action = claudeCardAction({
    ownerCreated: card.ownerCreated,
    sessionState: card.sessionState,
    runningVersion: card.runningVersion,
    installedVersion: installed.data?.version ?? null,
  });
  const shown = pending ?? action;
  const restart = (which: ClaudeCardAction) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setPending(which);
    void feedback.run(
      port.restart(card.id),
      (error) => readWriteFailure(error, CLAUDE_RESTART_FAILURES, CLAUDE_RESTART_TEXT[which]).text,
    ).finally(() => {
      inFlight.current = false;
      setPending(null);
    });
  };
  return <>
    {feedback.error !== null && <span role="alert" data-nc-claude-card-error="">{feedback.error}</span>}
    {shown !== null && (
      <Button
        label={pending === null ? ACTION_LABEL[shown] : PENDING_LABEL[shown]}
        variant="secondary"
        size="sm"
        isLoading={pending !== null}
        data-nc-claude-card-action={shown}
        onClick={(event) => { event.stopPropagation(); restart(shown); }}
        // The head is the card's drag handle: a press on the button must not start a drag.
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => event.stopPropagation()}
      />
    )}
  </>;
}
