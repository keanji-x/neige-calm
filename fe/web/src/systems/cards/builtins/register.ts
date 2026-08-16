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
 * The only value the registrar map accepts, and the only call site of
 * `registry.register` on the production boot path.
 *
 * What the types catch, exactly:
 *
 * 1. A *missing* `headless`. `CardEntry.headless` is optional on the interface —
 *    it has to be, because the frozen contract tests build entries without it
 *    and register them for real — so an omission is invisible to the compiler at
 *    a bare `register` call. `BuiltinRegistrar.of` requires it, and an optional
 *    property is not assignable to a required one, so deleting `headless` from a
 *    built-in entry is a typecheck error at its map slot below.
 * 2. *Skipping `BuiltinRegistrar.of`.* The map's value type is this class, whose
 *    `#register` field makes it nominal: no object literal, arrow function or
 *    `Object.assign` shape is assignable to it, and the constructor is private,
 *    so `BuiltinRegistrar.of` is the only expression that produces one. A future
 *    slice writing `terminal: (target) => { target.register(TERMINAL_ENTRY); }`
 *    does not typecheck. The registry's generic stays per-entry: the entry is
 *    captured here with its own narrow `Card`, never widened to the whole
 *    `RegisteredCard` union.
 *
 * What the types do *not* catch: a deliberate `as unknown as BuiltinRegistrar`
 * assertion inside this file. No in-language brand survives `as unknown`; that
 * is a visible, reviewable edit, not a silent omission.
 *
 * What the tests catch: a *wrong* declaration. `headless: false` on a headless
 * card type compiles fine here and is caught one layer out by
 * `register.contract.test.ts`, whose `HEADLESS_BY_TYPE` table decides the answer
 * per type from the oracle and is set-equal to `BUILTIN_CARD_ORDER`. That file
 * also asserts `typeof entry.headless === 'boolean'` on every registered entry,
 * so an omission stays red even if the table were mis-edited the same way — and
 * that runtime assertion is what covers the assertion escape above.
 */
class BuiltinRegistrar {
  readonly #register: (target: CardRegistry) => void;

  private constructor(register: (target: CardRegistry) => void) {
    this.#register = register;
  }

  static of<Card extends RegisteredCard>(
    entry: CardEntry<Card> & { readonly headless: boolean },
  ): BuiltinRegistrar {
    return new BuiltinRegistrar((target) => { target.register(entry); });
  }

  run(target: CardRegistry): void {
    this.#register(target);
  }
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
  // the loop below, not this literal, decides the registration order. The value
  // type is `BuiltinRegistrar`, so the only way to fill a slot is
  // `BuiltinRegistrar.of(entry)` — see its doc comment for what that forces and
  // what it does not.
  const registrars: Partial<Record<BuiltinCardType, BuiltinRegistrar>> = {
    spec: BuiltinRegistrar.of(SPEC_CARD_ENTRY),
    'wave-report': BuiltinRegistrar.of(WAVE_REPORT_CARD_ENTRY),
  };
  for (const type of BUILTIN_CARD_ORDER) {
    registrars[type]?.run(registry);
  }
}
