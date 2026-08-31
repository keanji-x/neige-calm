// The codex worker card. Kernel kind `'codex'`. Owns a surface.
//
// `INV-CARD-180` — the kernel mints *two* different cards under kind `'codex'`:
// ordinary codex worker cards and spec harness cards, told apart only by the
// `spec_harness` discriminator on the payload. So this entry deliberately
// carries **no `claim`**. An exact claim on `'codex'` would put this entry in
// front of the whole resolution path; the card kinds stay separable only
// because `BUILTIN_CARD_ORDER` registers codex before spec and the registry
// falls back to a full scan in insertion order. `fromKernel` therefore has to
// *refuse* spec harness payloads so they fall through to `SPEC_CARD_ENTRY` —
// and it refuses them with `isSpecHarnessPayload`, the same predicate spec
// accepts them with. One predicate, imported rather than copied, is what stops
// a future edit fixing one of the two card kinds while silently breaking the
// other.
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

export const CODEX_CARD_ENTRY = Object.freeze({
  type: 'codex',
  component: (props: CardComponentProps<CodexCard>) => TerminalCardView({
    ...props,
    fallbackTitle: CODEX_FALLBACK_TITLE,
  }),
  headless: false,
  defaultSize: Object.freeze({ w: 6, h: 10, minW: 4, minH: 6 }),
  // No `claim` on purpose — see the header comment. Spec shares this kernel
  // kind and both entries must stay on the insertion-ordered full scan.
  title: (card: CodexCard) => card.title ?? 'Codex',
  accessibleName: (card: CodexCard) => card.title ?? 'Codex',
  // Worker cards are minted by the kernel dispatcher off a task row; there is
  // no user-facing "new codex card" gesture to model here.
  create: Object.freeze({ mode: 'kernel-minted-only' as const }),
  fromKernel: (card: KernelCardInput): CodexCard | null => (
    card.kind === 'codex' && !isSpecHarnessPayload(card.payload)
      ? Object.freeze({
        type: 'codex',
        id: card.id,
        title: null,
        terminalId: terminalIdFromPayload(card.payload),
      } as const)
      : null
  ),
}) satisfies CardEntry<CodexCard>;
