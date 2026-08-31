// The codex worker card. Kernel kind `'codex'`. Owns a surface.
//
// `INV-CARD-180` — the kernel mints *three* different cards under kind
// `'codex'`, told apart only by markers on the payload:
//
//  1. ordinary codex worker cards — a PTY, this entry's card;
//  2. spec harness cards — `spec_harness: true`
//     (`routes/waves.rs::spec_harness_card_payload`, `routes/today.rs`),
//     owned by `SPEC_CARD_ENTRY`, headless;
//  3. cove plain-chat conversation cards — `harness_profile: "plain_chat"`,
//     minted with the `spec_harness` key deliberately *absent* (`INV-CHAT-016`,
//     `operation/spec_harness_start_adapter.rs`). These run entirely on the
//     shared codex app-server and have **no PTY at all**: the adapter writes
//     `terminal_run_id: None`, so nothing in
//     `session_projection_lookup.rs::project_runtime_fields` can ever fill
//     `terminal_id` for them.
//
// So `fromKernel` refuses (2) *and* (3). Refusing the harness lets it fall
// through to `SPEC_CARD_ENTRY`; refusing plain chat resolves it to nothing at
// all, which puts it in `partitionWaveCards`'s `unknown` branch — where it sat
// before this adapter existed. Claiming it would give it a grid slot rendering
// `TerminalCardView` with `terminalId === null`, i.e. a card reading
// "Starting codex…" forever, one per conversation. A real plain-chat adapter is
// a separate issue; until it lands, `isPlainChatPayload` is exported so that
// adapter imports this predicate instead of restating it.
//
// Load-bearing dependency worth writing down: today the cove chat wave is
// unreachable from the wave route by design — `routes/waves.rs::user_visible_wave`
// strips it from `GET /api/coves/:id/waves`, the cove route does not navigate
// on row-open, and `app/router/public.tsx` explicitly gates server-listed
// conversations out of the Today registry that does navigate. That route-layer
// gate is in an unrelated module; the refusal here is what makes this file
// correct on its own if the gate ever moves.
//
// About the `claim` and the registration order. The honest statement: **the
// refusals are what separate the kinds**, and the *shared* `isSpecHarnessPayload`
// predicate is what keeps codex and spec from disagreeing about which cards are
// harnesses. This entry carries no `claim` and `BUILTIN_CARD_ORDER` registers
// codex before spec — but neither is load-bearing given the registry's
// fall-through semantics: `resolve` runs the exact-claim entry's `fromKernel`
// first and, on `null`, *continues* to the prefix path and then the
// insertion-order scan. An exact claim on `'codex'` here would therefore still
// resolve spec harnesses as `spec`, and swapping the two registrations changes
// nothing either. Both are kept as belt-and-braces, and the no-claim rule is
// pinned by an assertion in `codex.test.ts` (as `spec.test.ts` pins spec's) so
// it is a stated contract rather than an accident; the order rule is a stated
// convention that no test can make fail. Do not read either as the mechanism.
//
// Codex really does run inside a PTY — `CodexAdapter` calls `spawn_terminal`
// for its worker sessions — so, like claude, the card *is* a terminal card
// wearing a different name and reuses `TerminalCardView` rather than growing a
// parallel renderer. `terminal_id` is not on the stored card row; the kernel
// projects it into the payload on read
// (`session_projection_lookup.rs::project_runtime_fields`), kind-agnostically,
// exactly as terminal and claude cards get theirs. Reading it through the
// shared `terminalIdFromPayload` needs no wire-schema change.

import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import { isSpecHarnessPayload } from './spec.ts';
import { TerminalCardView } from './terminal-card.tsx';
import { terminalIdFromPayload } from './terminal.ts';

declare module '../registry.js' {
  interface CardDataMap {
    codex: CodexCard;
  }
}

export type CodexCard = Readonly<{
  type: 'codex';
  id: string;
  title: string | null;
  terminalId: string | null;
}>;

/**
 * Lowercase to match the terminal card's own `'terminal'` fallback — head
 * labels for a kernel-minted card with no title are lowercase here. The exact
 * lowercase string `'codex'` is also what `LetterAvatar`'s `semanticClass`
 * matches to pick `card-head-icon--codex`, and it lowercases the title itself,
 * so any casing of this word reaches the same branch.
 */
const CODEX_FALLBACK_TITLE = 'codex';

/**
 * `INV-CHAT-016` — the cove plain-chat marker, and nothing else.
 *
 * Mirrors the kernel's own reader (`calm-server/src/plain_chat.rs`), which is
 * `payload.get("harness_profile").and_then(as_str) == Some("plain_chat")`:
 *
 * - non-object and `null` payloads are ordinary shapes on the wire, so they
 *   narrow to `false` instead of throwing (same handling as
 *   `isSpecHarnessPayload`);
 * - the key absent → `false` — an ordinary codex worker card;
 * - a *different* string (`harness_profile: 'something_else'`) → `false`. No
 *   such profile exists today; if one is ever minted it is a new shape that
 *   must be decided on deliberately, not swept in by a truthiness test here.
 *   Falsely claiming it would give it a dead grid slot, which is the failure
 *   this predicate exists to stop.
 * - a non-string value (`true`, `1`, an object) → `false`, exactly as the
 *   kernel's `as_str` returns `None` for it.
 */
export function isPlainChatPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { harness_profile?: unknown }).harness_profile === 'plain_chat';
}

export const CODEX_CARD_ENTRY = Object.freeze({
  type: 'codex',
  component: (props: CardComponentProps<CodexCard>) => TerminalCardView({
    ...props,
    fallbackTitle: CODEX_FALLBACK_TITLE,
  }),
  headless: false,
  defaultSize: Object.freeze({ w: 6, h: 10, minW: 4, minH: 6 }),
  // No `claim` on purpose — see the header comment. It is belt-and-braces, not
  // the mechanism (the registry's exact-claim path falls through on `null`),
  // and `codex.test.ts` pins it as a contract.
  title: (card: CodexCard) => card.title ?? 'Codex',
  accessibleName: (card: CodexCard) => card.title ?? 'Codex',
  // Worker cards are minted by the kernel dispatcher off a task row; there is
  // no user-facing "new codex card" gesture to model here.
  create: Object.freeze({ mode: 'kernel-minted-only' as const }),
  fromKernel: (card: KernelCardInput): CodexCard | null => (
    card.kind === 'codex'
      && !isSpecHarnessPayload(card.payload)
      && !isPlainChatPayload(card.payload)
      ? Object.freeze({
        type: 'codex',
        id: card.id,
        title: null,
        terminalId: terminalIdFromPayload(card.payload),
      } as const)
      : null
  ),
}) satisfies CardEntry<CodexCard>;
