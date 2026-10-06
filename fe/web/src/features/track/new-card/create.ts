// One add-card intent's policy (#2131): what a draft sends, when two drafts are one intent, and which failure ends it.
// The route only wires it to the mutations, the key holder and the landing; nothing here holds state or mints a key.

import { ApiError, classifyFailure, NotSentError } from '../../../../../core/domain/failure-class.ts';
import {
  CARD_CREATE_FAILURES, cardCreateText, readKeyedCreateFailure,
  type CardWire, type NewCardBody, type NewCodexCardBody, type NewTerminalCardBody, type ThemeRgb,
} from '../../../../../core/domain/track.ts';
import type { CardAddMenuEntry, CardRegistry } from '../../../systems/cards/public.js';
import type { NewCardValues } from './public.tsx';

/** One add-card draft: its kind and the values typed into its form. */
export type CardDraft = Readonly<{ entry: CardAddMenuEntry; values: NewCardValues }>;

/** One draft's body on its kind's endpoint, built once per intent: an atomic worker endpoint, or the generic create. */
export type KeyedCardBody = Readonly<
  { kind: 'terminal'; body: NewTerminalCardBody } | { kind: 'codex'; body: NewCodexCardBody } | { kind: 'generic'; body: NewCardBody }
>;

export type CardCreatePort = Readonly<{
  createTerminal: (body: NewTerminalCardBody, idempotencyKey: string) => Promise<CardWire>;
  createCodex: (body: NewCodexCardBody, idempotencyKey: string) => Promise<CardWire>;
  createCard: (body: NewCardBody, idempotencyKey: string) => Promise<CardWire>;
}>;

/* Empty is absent, not `""`: the kernel reads an empty `cwd` as "no directory given" but an empty `title` as a real,
   blank title. */
function givenValue(values: NewCardValues, key: string): string | undefined {
  const value = (values[key] ?? '').trim();
  return value === '' ? undefined : value;
}

/** Two drafts are one intent when they are of one kind and would send the same values. */
export function sameCardDraft(held: CardDraft, next: CardDraft): boolean {
  const keys = new Set([...Object.keys(held.values), ...Object.keys(next.values)]);
  return held.entry.type === next.entry.type
    && [...keys].every((key) => givenValue(held.values, key) === givenValue(next.values, key));
}

/**
 * A draft's body: terminal and Codex on their atomic endpoints, any other kind through the generic create with the
 * entry's claimed kind; `null` for a kind the registry cannot create.
 */
export function keyedCardBodyOf({ entry, values }: CardDraft, theme: ThemeRgb, registry: CardRegistry): KeyedCardBody | null {
  const title = givenValue(values, 'title');
  const titled = title === undefined ? {} : { title };
  if (entry.type === 'terminal') return { kind: 'terminal', body: { theme, ...titled } };
  if (entry.type === 'codex') {
    const cwd = givenValue(values, 'cwd');
    return { kind: 'codex', body: { theme, ...titled, ...(cwd === undefined ? {} : { cwd }) } };
  }
  const registered = registry.get(entry.type);
  const strategy = registered?.create;
  if (strategy?.mode !== 'generic' || registered?.claim?.mode !== 'exact') return null;
  return { kind: 'generic', body: { kind: registered.claim.kind, payload: strategy.buildPayload(values), ...titled } };
}

/** Sends one attempt on its kind's endpoint, under its key and the body built at the first press. */
export function sendCardCreate(
  port: CardCreatePort, { entry }: CardDraft, keyed: Readonly<{ key: string; body: KeyedCardBody }> | null,
): Promise<CardWire> {
  if (keyed === null) throw new Error(`CardCreateUnsupported(${entry.type})`);
  const { key, body } = keyed;
  if (body.kind === 'terminal') return port.createTerminal(body.body, key);
  return body.kind === 'codex' ? port.createCodex(body.body, key) : port.createCard(body.body, key);
}

/**
 * Whether a failed attempt ends its intent, so the next press mints a new key: an answer the table reads as anything
 * but unknown, or a press that sent nothing — unless it resent an intent whose outcome was already unknown.
 */
export function cardCreateEnded(error: unknown, resent: boolean): boolean {
  if (error instanceof NotSentError) return !resent;
  return classifyFailure(error instanceof ApiError ? error.failure : null, CARD_CREATE_FAILURES) !== 'unknown';
}

/** What a failed attempt says; a resend that sent nothing leaves the earlier unknown outcome standing. */
export function cardCreateFailureText(label: string, resent: boolean): (error: unknown) => string {
  const text = cardCreateText(label);
  return (error) => (resent && error instanceof NotSentError
    ? text.unknown
    : readKeyedCreateFailure(error, CARD_CREATE_FAILURES, text).text);
}
