import { onlineManager } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { CONVERSATION_CREATE_TEXT, conversationCreateFailure, conversationCreateUnknownText, CONVERSATION_TEXT_MAX, FOLLOW_INSTALLATION_DEFAULT, type Conversation } from '../../../../core/domain/conversation.ts';
import { ApiError, NotSentError } from '../../../../core/domain/failure-class.ts';
import { admitTransport } from '../providers/recovery-mutation.ts';
import { mintIdempotencyKey } from '../providers/idempotency-key.ts';
import type { ConversationDraft, ConversationDraftId, ConversationRegistry } from './public.tsx';
import type { ConversationCreationSource } from './contracts.ts';

/** The identity and dispatched body move together through rekey/markSent only. */
type DraftEdit = Partial<Pick<ConversationDraft, 'text' | 'creating' | 'error' | 'remedy'>>;

export type ConversationDraftActionInput = Readonly<{
  registry: Pick<ConversationRegistry, 'editDraft' | 'startDraft' | 'adoptDraft' | 'discardDraft'>;
  draft: ConversationDraft | null;
  creating: boolean;
  source: ConversationCreationSource;
  sourceScopeId: string;
  transport: ApiTransportPort;
  supportsSideConversation: boolean;
  supportsDraftModel: boolean;
  openDraft: () => void;
  closeView: () => void;
  onGone: () => void;
}>;

/** A command assembly for this render's draft. Construction has no side effects;
 * dispatched attempts retain their original scope/key and settle through Registry. */
export function createConversationDraftActions({ registry, draft, creating, source, sourceScopeId, transport,
  supportsSideConversation, supportsDraftModel, openDraft, closeView, onGone,
}: ConversationDraftActionInput) {
  /* Every draft write goes through one of these three, each a whole-object update,
       and each a no-op when the draft it was computed from is no longer the one held. */
  const withDraft = (
    from: ConversationDraftId, next: (current: ConversationDraft) => ConversationDraft,
  ) => {
    registry.editDraft(from, next);
  };
  const amendDraft = (from: ConversationDraft, change: DraftEdit) => {
    withDraft(from, (current) => ({ ...current, ...change }));
  };
  /* The only way to change the key, and it always clears `sentText`: a key is the
       identity of an attempt and `sentText` is what that attempt sent. */
  const rekeyDraft = (from: ConversationDraft, key: string, change: DraftEdit = {}): ConversationDraft => {
    const next = { ...from, ...change, key, sentText: null };
    withDraft(from, (current) => ({ ...current, ...change, key, sentText: null }));
    return next;
  };
  /** Records that a POST is going out under this key with these words. Called
   *  before the request, so a failure finds the right baseline. */
  const markDraftSent = (from: ConversationDraft, text: string) => {
    withDraft(from, (current) => ({ ...current, text, sentText: text }));
  };

  /* The `+` opens a draft scoped to one concrete Track; Today materialises the
       launchpad before calling this, so the scope id is never empty. */
  const begin = (text: string | null): boolean => {
    /* A draft that was sent and failed is still open business: reopened with the
           same key, so the next attempt is a retry and not a second conversation. */
    if (draft !== null && draft.sentText !== null) {
      openDraft();
      return false;
    }
    /* The key is minted once, for the draft, not per send: a different key on the
           retry is a different derived card. */
    registry.startDraft({
      scopeId: sourceScopeId,
      model: FOLLOW_INSTALLATION_DEFAULT,
      key: mintIdempotencyKey(),
      text, autoSend: text !== null, sentText: null, creating: false, error: null, remedy: null,
    });
    openDraft();
    return true;
  };
  const start = () => { begin(null); };
  const startWithMessage = (text: string) => begin(text);

  /* `from` is not decoration: this runs after an `await`, and the reducer records
       the row only if `from` is still held. */
  const adopt = (from: ConversationDraftId, row: Conversation) => {
    registry.adoptDraft(from, row.id);
    /* Nothing is minted for the first sentence here: the kernel writes it to the
           transcript at drain, and the item read serves it back. */
  };

  /* A press that sent nothing is refused (#2131), unless an earlier attempt under this key, `sentText`, may have
     created the conversation: then it stays unconfirmed. Why nothing went out is the global indicator's to say. */
  const notSentText = (sentText: string | null): string =>
    sentText === null ? CONVERSATION_CREATE_TEXT.refused : CONVERSATION_CREATE_TEXT.unknown;

  /* Re-read the list and adopt this draft's OWN row (by derived id), never "the
       list grew". Three answers: `'unknown'` is the re-read itself failing, and
       treating it as `'absent'` would mint a new key over an attempt that may have
       committed. */
  const adoptIfItLanded = async (
    refresh: () => Promise<readonly Conversation[]>,
    derivedCardId: (idempotencyKey: string) => string,
    scopeId: string,
    key: string,
    current: () => boolean,
  ): Promise<'landed' | 'absent' | 'unknown'> => {
    if (!current()) return 'unknown';
    const rows = await refresh().catch(() => null);
    if (!current() || rows === null) return 'unknown';
    const cardId = derivedCardId(key);
    const landed = rows.find((row) => row.id === cardId);
    if (landed === undefined) return 'absent';
    adopt({ scopeId, key }, landed);
    return 'landed';
  };

  /* The draft's own send: it runs while there is no card, and its text lives in the
       registry's draft entry until the row it created is adopted. */
  const refuseOfflineDraft = (attempt: ConversationDraft, text: string): boolean => {
    if (attempt.side !== undefined && !supportsSideConversation) {
      amendDraft(attempt, { text, error: 'This server does not report side conversation support. Update or reconnect before retrying.', remedy: 'retry' });
      return true;
    }
    if ((attempt.model.model !== null || attempt.model.reasoning_effort !== null) && !supportsDraftModel) {
      amendDraft(attempt, { text, error: 'This server does not support choosing the first message’s model yet.', remedy: 'retry' });
      return true;
    }
    if (onlineManager.isOnline()) return false;
    amendDraft(attempt, { text, error: notSentText(attempt.sentText), remedy: 'retry' });
    return true;
  };

  const admitDraft = (attempt: ConversationDraft): (() => boolean) | null => {
    try {
      const admitted = admitTransport(transport);
      const checkpoint = admitted.recovery?.checkpoint();
      return () => { try { checkpoint?.(); return true; } catch { return false; } };
    } catch {
      amendDraft(attempt, { error: notSentText(attempt.sentText), remedy: 'retry' });
      return null;
    }
  };

  /**
   * One create of the draft's words, from the press's checks on. With `recheck`, the old key's row is looked for first
   * and only a re-read saying "no row" earns a new key; while the re-read fails, the draft offers `recheck` again.
   */
  const createDraft = (from: ConversationDraft, text: string, current: () => boolean, recheck: ConversationDraft['remedy']) => {
    const { create, refresh, derivedCardId } = source;
    const scopeId = sourceScopeId;
    /* The draft this send is for, fixed here: a send that outlives its draft changes nothing. */
    let attempt = from;
    let previouslySentText = attempt.sentText;
    void (async () => {
      try {
        if (recheck !== null) {
          const landing = await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key, current);
          if (landing === 'landed') return;
          if (landing === 'unknown') {
            amendDraft(attempt, { error: conversationCreateUnknownText(recheck), remedy: recheck });
            return;
          }
          attempt = rekeyDraft(attempt, mintIdempotencyKey());
        }
        if (!current()) { amendDraft(attempt, { error: notSentText(attempt.sentText), remedy: 'retry' }); return; }
        if (refuseOfflineDraft(attempt, text)) return;
        previouslySentText = attempt.sentText;
        markDraftSent(attempt, text);
        attempt = { ...attempt, text, sentText: text };
        const created = await create(text, attempt.key, attempt.model, attempt.side);
        if (current()) adopt(attempt, created);
        else amendDraft(attempt, { error: CONVERSATION_CREATE_TEXT.unknown, remedy: 'retry' });
      } catch (error: unknown) {
        if (error instanceof NotSentError) {
          // Marking a request optimistically must not invent dispatch when the
          // mutation's later guard refused it. Keep any earlier unknown send.
          registry.editDraft(attempt, (current) => ({ ...current, sentText: previouslySentText }));
          amendDraft(attempt, { error: notSentText(previouslySentText), remedy: 'retry' });
        } else {
          attempt = await handleCreateFailure(error, refresh, derivedCardId, scopeId, attempt, current);
        }
      } finally {
        amendDraft(attempt, { creating: false });
      }
    })();
  };

  const sendDraft = (text: string) => {
    if (creating || draft === null) return;
    /* The server refuses `text.trim().is_empty()` but counts `chars()` on the
           untrimmed text, in Unicode scalar values: so the blank check trims, the
           length check does not, and `Array.from` counts code points. */
    if (text.trim() === '') return;
    if (Array.from(text).length > CONVERSATION_TEXT_MAX) {
      /* Shown back, but never recorded as sent: no request left the browser, so
         the key is untouched and the next press is not "the text changed". */
      amendDraft(draft, {
        text,
        error: `This message is too long — the limit is ${CONVERSATION_TEXT_MAX} characters.`,
        remedy: null,
      });
      return;
    }
    if (refuseOfflineDraft(draft, text)) return;
    const current = admitDraft(draft); if (current === null) return;
    amendDraft(draft, { text, creating: true, error: null, remedy: null });
    /* Editing the text after a failure has to look at the list first: the old key may have succeeded with the old
       text, and only a re-read saying "no new row" earns a new key. */
    createDraft(draft, text, current, draft.sentText !== null && draft.sentText !== text ? 'retry' : null);
  };

  async function handleCreateFailure(
    error: unknown,
    refresh: () => Promise<readonly Conversation[]>,
    derivedCardId: (idempotencyKey: string) => string,
    scopeId: string,
    attempt: ConversationDraft,
    current: () => boolean,
  ): Promise<ConversationDraft> {
    if (!current()) {
      amendDraft(attempt, { error: CONVERSATION_CREATE_TEXT.unknown, remedy: 'retry' });
      return attempt;
    }
    const failure = conversationCreateFailure(error instanceof ApiError ? error.failure : null);
    const message = failure.message;
    switch (failure.kind) {
      case 'gone':
        registry.discardDraft(attempt);
        onGone();
        return attempt;
      case 'exhausted':
        /* A spent key can never succeed again, so a new one is minted and takes
                   `sentText` with it: nothing was posted under this key. */
        return rekeyDraft(attempt, mintIdempotencyKey(), { error: message, remedy: 'retry' });
      case 'stale-payload':
        amendDraft(attempt, { error: message, remedy: 'new-conversation' });
        return attempt;
      case 'blocked':
        /* Nothing committed and the key is unspent, so both it and the words are kept. */
        amendDraft(attempt, { error: message, remedy: 'retry' });
        return attempt;
      case 'exists': {
        /* The derived card exists, so this key can never mint again; a new key is
                   offered only once the re-read has said there is no row. */
        amendDraft(attempt, { error: message });
        const landing = await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key, current);
        if (landing === 'absent') amendDraft(attempt, { remedy: 'new-conversation' });
        if (landing === 'unknown') amendDraft(attempt, { error: CONVERSATION_CREATE_TEXT.unknown, remedy: 'retry' });
        return attempt;
      }
      case 'retry':
        /* Ambiguous (a 503 included: on this endpoint it is raised after the card is
                 minted), so the look for this key's card is not skipped. */
        amendDraft(attempt, { error: message });
        if (await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key, current) !== 'landed') {
          amendDraft(attempt, { remedy: 'retry' });
        }
        return attempt;
    }
  }

  const sendAsNewConversation = () => {
    if (creating || draft === null || draft.text === null) return;
    const text = draft.text;
    if (refuseOfflineDraft(draft, text)) return;
    const current = admitDraft(draft); if (current === null) return;
    amendDraft(draft, { creating: true, error: null, remedy: null });
    /* Pressed deliberately, but the same fence applies: a new key is only
       safe once the list has actually said the old one produced nothing. */
    createDraft(draft, text, current, 'new-conversation');
  };

  /* Retry means the same draft again: same key, same words. */
  const retryDraft = () => {
    if (draft === null || draft.text === null) return;
    sendDraft(draft.text);
  };

  /* A draft no request was ever made for has no identity worth keeping; one that
       was sent and failed keeps its key and words so the next attempt is a retry. */
  const closeDrawer = () => {
    closeView();
    if (draft !== null && draft.sentText === null) registry.discardDraft(draft);
  };

  return { start, startWithMessage, withDraft, sendDraft, retryDraft, sendAsNewConversation, closeDrawer };
}
