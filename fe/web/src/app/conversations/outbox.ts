import { replaceEqualDeep } from '@tanstack/react-query';
import { useEffect, useMemo, useRef } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { PlannerAttachment } from '../../../../core/api/generated/wire.ts';
import {
  conversationNameFrom, isConversationMessage,
  type ConversationMessage, type ConversationTurn, type SendOutcome, type SentPlannerInput, type TranscriptEntry,
} from '../../../../core/domain/conversation.ts';
import { KeyedSendFailure, retryUnknownSend } from '../../../../core/domain/conversation-delivery.ts';
import {
  outboxView, settleSendOp, withConfirmedSends, withoutQueuedEntry, type LandedReads, type ReplacedTurn, type SendOp,
} from '../../../../core/domain/conversation-outbox.ts';
import { withRefill } from '../../../../core/domain/conversation-rewind.ts';
import { recoveryDelay } from '../../../../core/domain/recovery/access.ts';
import { ApiError, OfflineSubmissionError } from '../providers/queries.ts';
import { admitTransport } from '../providers/recovery-mutation.ts';
import { mintIdempotencyKey } from '../router/idempotency-key.ts';
import { useState } from '../../ui/state/public.ts';
import { useConversationRegistry, type RememberedConversation } from './public.tsx';

type RunQueryOptions<T> = Readonly<{ queryFn: (context: { signal: AbortSignal }) => Promise<T> }>;

/**
 * The run query's reads, numbered in the tab's read order as each starts; a result carries the
 * number of the read it came from, and one this view did not read carries 0.
 */
export type RunReads = Readonly<{
  track: <T extends object, Options extends RunQueryOptions<T>>(options: Options) => Options & {
    structuralSharing: (previous: unknown, next: unknown) => unknown;
  };
  startOf: (result: object | undefined) => number;
}>;

export function useRunReads(nextRead: () => number): RunReads {
  const [reads] = useState<RunReads>(() => {
    /* Structural sharing can keep the stored result when nothing changed; the kept one then takes the newer number. */
    const starts = new WeakMap<object, number>();
    const startOf = (result: object | undefined) => result === undefined ? 0 : starts.get(result) ?? 0;
    return {
      track: (options) => ({
        ...options,
        queryFn: async (context) => {
          const start = nextRead();
          const result = await options.queryFn(context);
          starts.set(result, start);
          return result;
        },
        structuralSharing: (previous, next) => {
          const shared = replaceEqualDeep(previous, next) as object;
          starts.set(shared, startOf(next as object));
          return shared;
        },
      }),
      startOf,
    };
  });
  return reads;
}

/** The row a confirmed send leaves in the tab's memory, written whether or not its conversation is open. */
function rememberSent(entry: RememberedConversation, ops: readonly SendOp[], text: string, atMs: number): RememberedConversation {
  const shown = withConfirmedSends(entry.turns, ops);
  return {
    conversation: {
      ...entry.conversation,
      title: entry.conversation.title ?? conversationNameFrom(text),
      updatedAt: Math.max(entry.conversation.updatedAt, atMs),
      turns: shown.filter(isConversationMessage).length,
    },
    turns: entry.turns,
  };
}

/**
 * The open conversation's keyed sends (#2043): the press, the retries under one key, Try again, Edit
 * and Dismiss of a failed one, and its view of the thread. The ops live in the registry's outbox for
 * that conversation, so a send settles there whichever conversation is shown, and leaves it only when
 * server state shows it (`outboxView`) or the reader drops it.
 */
export function useConversationOutbox({
  cardId, transport, send, serverEntries, serverTurns, liveReplies, queuedEntryIds, stalled, landed,
  queuesInput, highWater, pressed,
}: {
  cardId: string;
  transport: ApiTransportPort;
  /** `POST …/planner/input` for this card; `answered` runs when its 200 is in hand, before its refresh reads start. */
  send: (text: string, attachments: readonly string[], key: string, replacesTurn: string | null, admitted: ApiTransportPort,
    answered: () => void) => Promise<SentPlannerInput>;
  serverEntries: readonly TranscriptEntry[];
  serverTurns: readonly ConversationMessage[];
  liveReplies: readonly ConversationTurn[];
  queuedEntryIds: ReadonlySet<string>;
  stalled: boolean;
  landed: LandedReads;
  /** Whether the kernel queues a message posted now (`kernelQueuesInput`), read at the press. */
  queuesInput: boolean;
  /** The newest persisted item id read so far. */
  highWater: number;
  /** A send of the conversation shown went out. */
  pressed: () => void;
}) {
  const registry = useConversationRegistry();
  const { editOutbox, beginSend, nextRead, updateExisting } = registry;
  const ops = registry.outboxOf(cardId);
  /** The card shown now; a send's own `cardId` is the one it was pressed in. */
  const shownCardId = useRef(cardId);
  shownCardId.current = cardId;
  const view = useMemo(
    () => outboxView({ serverEntries, serverTurns, liveReplies, queuedEntryIds, stalled, ops, landed }),
    [serverEntries, serverTurns, liveReplies, queuedEntryIds, stalled, ops, landed],
  );
  const { retire } = view;
  /* Filtering by key is safe only because a `confirmed` or `replayed` op, the only ones retired, never changes phase
     again: an op under that key here is still the one the view retired. */
  useEffect(() => {
    if (retire.length === 0) return;
    editOutbox(cardId, (current) => {
      const kept = current.filter((op) => !retire.includes(op.key));
      return kept.length === current.length ? current : kept;
    });
  }, [cardId, editOutbox, retire]);

  /**
   * A composer send's images leave the composer of the card they were sent from, whichever conversation is shown by
   * now: on delivery, and on Dismiss, since a send that may have been stored may have bound them.
   */
  const releaseComposerImages = (sentTo: string, attachments: readonly PlannerAttachment[]) => {
    const sentIds = new Set(attachments.map((attachment) => attachment.id));
    registry.editComposer(sentTo, (current) => current.attachments.some((image) => sentIds.has(image.id))
      ? { ...current, attachments: current.attachments.filter((image) => !sentIds.has(image.id)) } : current);
    registry.editUpload(sentTo, (current) => current.refusal === null ? current : { ...current, refusal: null });
  };

  /** One run of a begun op: its attempts under one key, settled into the outbox of the card it was pressed in. */
  const run = (op: SendOp & { phase: 'sending' }, admittedAtPress: ApiTransportPort): Promise<SendOutcome> => {
    const sentTo = cardId;
    const { key, echo, fromComposer, replaces } = op;
    const attachments: readonly PlannerAttachment[] = echo.attachments ?? [];
    const settle = (next: SendOp | null) => editOutbox(sentTo, (current) => settleSendOp(current, key, next));
    let answeredRead = 0;
    /* An unknown answer is sent again under the same key, so the server queues the message at most once. Each
       retry is admitted again; one that cannot go out counts as an attempt. */
    const admitRetry = (): ApiTransportPort | null => { try { return admitTransport(transport); } catch { return null; } };
    return retryUnknownSend(
      (attempt) => {
        const admitted = attempt === 0 ? admittedAtPress : admitRetry();
        return admitted === null ? Promise.reject(new OfflineSubmissionError())
          : send(echo.text, attachments.map((attachment) => attachment.id), key, replaces?.turnId ?? null, admitted,
            () => { answeredRead = nextRead(); });
      },
      (error) => (error instanceof ApiError ? error.failure : null),
      (retry) => new Promise<void>((resolve) => { setTimeout(resolve, recoveryDelay(retry, Math.random())); }),
      op.unknown,
    ).then(({ sent, everUnknown }): SendOutcome => {
      if (fromComposer) releaseComposerImages(sentTo, attachments);
      /* Not claimed: the answer may replay an entry deleted, rewound or reset since, which no read would ever
         show. The echo stays until reads started after this answer land, and they alone then show the message. */
      if (everUnknown) { settle({ key, echo, fromComposer, replaces, phase: 'replayed', afterRead: answeredRead }); return 'delivered'; }
      const confirmed = settle({ key, echo: { ...echo, entryId: sent.entry_id }, fromComposer, replaces, phase: 'confirmed' });
      /* The answer can outlive the drawer: the row is written straight through for the conversation it was sent
         to, through `updateExisting` because a background refresh may already have put newer data there. */
      updateExisting(sentTo, (entry) => rememberSent(entry, confirmed, echo.text, echo.atMs));
      return 'delivered';
    }, (error: unknown): SendOutcome => {
      const failed = error instanceof KeyedSendFailure ? error : new KeyedSendFailure(error, 'unknown');
      const message = failed.cause instanceof Error && failed.cause.message !== '' ? failed.cause.message : 'Could not send the message.';
      if (replaces !== null && failed.delivery === 'refused') {
        /* Refused before any write: the turn is untouched, so the op is settled by its refill alone. Its words and
           images go back to the composer it was sent from and nothing of it stays in the outbox, so the thread is
           the server's again and the turn can be edited anew. */
        settle(null);
        registry.editComposer(sentTo, (current) => withRefill(current, { text: echo.text, attachments }));
        registry.noteRefusedEdit(sentTo, message);
        return 'refused';
      }
      /* `message` is shown only for an op that was not sent: an unknown op's footer says no more than that it is unconfirmed. */
      settle({ key, echo, fromComposer, replaces, phase: 'failed', delivery: failed.delivery, message });
      return failed.delivery === 'refused' ? 'refused' : 'unresolved';
    }).then((outcome) => shownCardId.current === sentTo ? outcome : 'abandoned');
  };

  /** Take the failed op under `key` out of the outbox; returns it, or `null` when it is no longer the failed op. */
  const discard = (key: string): SendOp | null => {
    let taken: SendOp | null = null;
    editOutbox(cardId, (current) => {
      taken = current.find((op) => op.key === key && op.phase === 'failed') ?? null;
      return taken === null ? current : settleSendOp(current, key, null);
    });
    return taken;
  };

  return {
    view,
    /**
     * A press: a new op under a new key. `attachments` are ids already uploaded; naming one here is what makes it
     * permanent. `fromComposer`: they are the composer's own, so a delivery clears them (and the upload refusal) there.
     * `replaces`: the turn an Edit's Send replaces, in the same request (#2043); beginning the op ends that Edit.
     */
    send: (conversationId: string, text: string, attachments: readonly PlannerAttachment[], fromComposer: boolean,
      replaces: ReplacedTurn | null): Promise<SendOutcome> => {
      if (conversationId !== cardId || stalled || (view.failed !== null && view.failed.delivery !== 'refused')) {
        return Promise.resolve('not-sent');
      }
      /* Admitted at the press. Where the transport carries a recovery admission (the bundled build), a press that
         cannot leave the browser is refused here and sends nothing: the composer keeps its words, and the global
         recovery status already says why. The web build admits every press, and a send that cannot leave fails as a
         transport error — unknown, retried. */
      let admitted: ApiTransportPort;
      try { admitted = admitTransport(transport); } catch { return Promise.resolve('refused'); }
      const op = {
        key: mintIdempotencyKey(), fromComposer, replaces, phase: 'sending', unknown: false,
        echo: {
          id: `echo-${mintIdempotencyKey()}`, author: 'you', text, atMs: Date.now(),
          /* The echo carries the images: an image-only message has no text to match on, so the ids are the second criterion. */
          attachments,
          /* The op's, not the run's: a resumed op's first attempt may already be in the transcript, above any high
             water read later, and only this one lets that row stand for the op. */
          serverHighWaterBefore: highWater,
          /* Read at the press from the last `GET /planner/run` snapshot, against the kernel's whitelist, and never
             recomputed from the live phase later. */
          queued: queuesInput,
          /* Not knowable yet: a 200 with no unknown attempt before it names it. */
          entryId: null,
        },
      } as const;
      if (!beginSend(cardId, op)) return Promise.resolve('not-sent');
      if (shownCardId.current === cardId) pressed();
      return run(op, admitted);
    },
    /**
     * Try again resumes the failed op, it does not press again: the echo (images included — an image-only message
     * re-sent as `{ text: "" }` is refused, and its ids are still bound), the key, the high water and the unknown-ness
     * all carry over, so a first attempt that was stored after all is answered again and never queued twice.
     */
    retrySend: (key: string) => {
      const failed = view.failed;
      if (failed?.key !== key || stalled) return;
      let admitted: ApiTransportPort;
      /* Not admitted: the op keeps its standing and its footer unchanged; the global recovery status says why. */
      try { admitted = admitTransport(transport); } catch { return; }
      const op = {
        key, echo: failed.echo, fromComposer: failed.fromComposer, replaces: failed.replaces, phase: 'sending',
        unknown: failed.delivery === 'unknown',
      } as const;
      if (!beginSend(cardId, op)) return;
      pressed();
      void run(op, admitted);
    },
    /** Edit of the failed op: it leaves the outbox, and the caller puts its words and images back in the composer. */
    discardFailedSend: (key: string) => { discard(key); },
    /**
     * Dismiss of the failed op: it leaves the outbox and nothing of it comes back, since an unknown send may already be
     * delivered (a read then shows it). Its composer images leave the composer too, as a delivery takes them: they may
     * be bound to the stored message, and the next message must not send them again.
     */
    dismissFailedSend: (key: string) => {
      const dismissed = discard(key);
      if (dismissed?.fromComposer === true) releaseComposerImages(cardId, dismissed.echo.attachments ?? []);
    },
    /** A queued entry this client deleted: the confirmed send that claimed it will never be shown by a read. */
    forgetQueuedEntry: (entryId: string) => { editOutbox(cardId, (current) => withoutQueuedEntry(current, entryId)); },
  };
}
