// Built-in card composition (`INV-CARD-225`).
//
// `BUILTIN_CARD_ORDER` is the single authority on registration order. The
// registry's kernel resolution falls back to a full scan in insertion order, so
// the order is business semantics (codex must get first refusal on kind
// `'codex'` before spec picks the card up), not an incidental detail.
//
// Entries that have not landed yet are simply **absent** from the registrar
// map and skipped. There are no placeholder, no-op or "unknown" entries: a card
// type either has a real adapter or the kernel card falls through to the
// unknown slot, which is a diagnosable state. Later slices add their real entry
// to this same map; nobody writes a second registration sequence.

import type { CardEntry, CardRegistry, RegisteredCard } from '../registry.js';
import { SPEC_CARD_ENTRY } from './spec.js';
import { WAVE_REPORT_CARD_ENTRY } from './wave-report.js';

export const BUILTIN_CARD_ORDER = Object.freeze([
  'terminal',
  'codex',
  'spec',
  'claude',
  'wave-report',
  'file-viewer',
  'iframe',
  'plugin-iframe',
] as const);

export type BuiltinCardType = (typeof BUILTIN_CARD_ORDER)[number];

/**
 * The production door onto `registry.register`, and the reason every built-in
 * states its headlessness out loud.
 *
 * `CardEntry.headless` is optional on the interface — it has to be, because the
 * frozen contract tests build entries without it and register them for real —
 * so an omission is invisible to the compiler at the `register` call. Here it is
 * **required**, and an optional property is not assignable to a required one:
 * deleting `headless` from a built-in entry is a typecheck error at its
 * registrar below. Nothing that goes through this function can fall back on the
 * fail-open "absent means it has a surface" default by accident.
 *
 * That is the whole of what this catches: a *missing* declaration. It says
 * nothing about a *wrong* one — declaring `headless: false` on a headless card
 * type compiles fine and is caught one layer out, by the `HEADLESS_BY_TYPE`
 * table in `register.contract.test.ts`, which decides the answer per type from
 * the oracle and is set-equal to `BUILTIN_CARD_ORDER`.
 */
function registerBuiltin<Card extends RegisteredCard>(
  registry: CardRegistry,
  entry: CardEntry<Card> & { readonly headless: boolean },
): void {
  registry.register(entry);
}

/**
 * Register every built-in card that exists today, in `BUILTIN_CARD_ORDER`.
 *
 * The signature is fixed: the caller owns the registry instance and nothing
 * else. Entries are not injected — they belong to `systems/cards`, so app has
 * no way to reorder them or slip a different adapter in.
 */
export function registerAvailableBuiltinCards(registry: CardRegistry): void {
  // Keyed by `BuiltinCardType` and ordered like the tuple purely for reading;
  // the loop below, not this literal, decides the registration order. Each
  // value registers its own entry so the registry's per-card generic keeps the
  // narrow card type instead of widening to the whole `RegisteredCard` union.
  const registrars: Partial<Record<BuiltinCardType, (target: CardRegistry) => void>> = {
    spec: (target) => { registerBuiltin(target, SPEC_CARD_ENTRY); },
    'wave-report': (target) => { registerBuiltin(target, WAVE_REPORT_CARD_ENTRY); },
  };
  for (const type of BUILTIN_CARD_ORDER) {
    registrars[type]?.(registry);
  }
}
