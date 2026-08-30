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
import { CLAUDE_CARD_ENTRY } from './claude.js';
import { SPEC_CARD_ENTRY } from './spec.js';
import { TERMINAL_CARD_ENTRY } from './terminal.js';
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
 * 2. *Ordinary structural stand-ins for `BuiltinRegistrar.of`.* The map's value
 *    type is this class, and its `#register` field makes it nominal, so a bare
 *    arrow function, an object literal carrying a `run` method, and
 *    `Object.assign({}, { run })` are all rejected (`Property '#register' is
 *    missing in type ... but required in type 'BuiltinRegistrar'`); the private
 *    constructor additionally rejects `class X extends BuiltinRegistrar`. A
 *    future slice writing `terminal: (target) => { target.register(ENTRY); }`
 *    does not typecheck.
 *
 * What the types do *not* catch — `of` is emphatically **not** provably the only
 * expression of this type, and the list below is **open**: it is a sample, not
 * an enumeration, and no count of it means anything. Any API whose declaration
 * hands back `any` or an identity `T` fills a slot with no assertion at all, and
 * the standard library keeps more of those than anyone has listed here. Each of
 * the following was verified against this file, both for typecheck and for what
 * it does at boot:
 *
 *  - `x as unknown as BuiltinRegistrar` — no in-language brand survives
 *    `as unknown`. *Runs*, and registers whatever entry it closes over.
 *  - `Object.create(BuiltinRegistrar.prototype)` — `lib.es5.d.ts` types
 *    `Object.create(o)` as `any`, so it assigns to a slot silently. Throws at
 *    boot: `TypeError: Cannot read private member #register from an object
 *    whose class did not declare it`.
 *  - `structuredClone(BuiltinRegistrar.of(entry))` — declared `T => T`, so the
 *    slot keeps the static type `BuiltinRegistrar`. Throws at boot: the clone
 *    carries neither the private field nor the prototype, so `run is not a
 *    function`.
 *  - `Object.assign(Object.create(BuiltinRegistrar.prototype), { run })` —
 *    `any & {...}` collapses to `any`. *Runs*: the own `run` shadows the
 *    prototype method, so `#register` is never read.
 *  - `Object.setPrototypeOf({ run(target) {…} }, BuiltinRegistrar.prototype)` —
 *    declared to return `any`. *Runs*, for the same shadowing reason.
 *  - `Reflect.construct(BuiltinRegistrar, [register])` — declared to return
 *    `any`, and `private constructor` is erased at runtime, so this builds a
 *    *real* instance (`instanceof` holds, `#register` is set). *Runs*.
 *
 * So "it would throw at boot" is not a property of this gate: several of these
 * shapes run and really register. What stops an entry smuggled in that way from
 * skipping its headless declaration is the runtime
 * `typeof entry.headless === 'boolean'` assertion in `register.contract.test.ts`,
 * applied per entry to the real production registry. The one guarantee that does
 * hold for every escape, known or not yet written down, is that it is a visible,
 * reviewable edit to *this* file — never a silent omission.
 *
 * The registry generic is narrow *today* because of how the two entry constants
 * are written, not because `of` forces it: both are object literals under
 * `satisfies CardEntry<SpecCard>` / `CardEntry<WaveReportCard>`, so `of` infers
 * each one's own narrow `Card`. A future entry annotated as a bare
 * `CardEntry & { readonly headless: boolean }` already has `RegisteredCard` as
 * its default generic, and `of` accepts it.
 *
 * What the tests catch: a *wrong* declaration. `headless: false` on a headless
 * card type compiles fine here and is caught one layer out by
 * `register.contract.test.ts`, whose `HEADLESS_BY_TYPE` table decides the answer
 * per type from the oracle and is set-equal to `BUILTIN_CARD_ORDER`. That file's
 * separate `typeof entry.headless === 'boolean'` assertion keeps an omission red
 * even if the table were mis-edited the same way.
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
 * The registrar map's own type, named so the negative fixture below is written
 * against the exact type the boot path uses rather than a copy of it.
 */
type BuiltinRegistrarMap = Partial<Record<BuiltinCardType, BuiltinRegistrar>>;

type AssertTrue<Condition extends true> = Condition;

/**
 * Compile-time negative fixture for the gate itself.
 *
 * Widening the slot type back to
 * `BuiltinRegistrar | ((target: CardRegistry) => void)` — that is, deleting the
 * only reason this class exists — leaves the typecheck and every runtime test
 * green, so the gate could be dismantled with nothing turning red. This pins it:
 * the moment either ordinary structural stand-in — a bare arrow, or an object
 * carrying a `run` method — becomes assignable to a slot,
 * `SlotRejectsStructuralRegistrars` resolves to `false` and `AssertTrue` fails
 * to instantiate (`Type 'false' does not satisfy the constraint 'true'`). Both
 * arms are needed: widening to a bare arrow also breaks the `?.run(...)` call
 * below, but widening to `{ run }` would otherwise be completely silent.
 *
 * It has to live in this file because `BuiltinRegistrar` is deliberately not
 * exported, so no test file can name the type. It is `export`ed only because an
 * unused local type alias is itself an error (`TS6196` /
 * `@typescript-eslint/no-unused-vars`); nothing imports it, and nothing should.
 */
type BuiltinRegistrarSlot = NonNullable<BuiltinRegistrarMap[BuiltinCardType]>;

type SlotRejectsStructuralRegistrars =
  ((target: CardRegistry) => void) extends BuiltinRegistrarSlot
    ? false
    : { run(target: CardRegistry): void } extends BuiltinRegistrarSlot
      ? false
      : true;

export type StructuralRegistrarIsNotAssignable = AssertTrue<SlotRejectsStructuralRegistrars>;

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
  // type is `BuiltinRegistrar`, so no ordinary structural value can fill a slot
  // and `BuiltinRegistrar.of(entry)` is the practical way in — see its doc
  // comment for what that forces and for the escapes it does not close.
  const registrars: BuiltinRegistrarMap = {
    terminal: BuiltinRegistrar.of(TERMINAL_CARD_ENTRY),
    spec: BuiltinRegistrar.of(SPEC_CARD_ENTRY),
    claude: BuiltinRegistrar.of(CLAUDE_CARD_ENTRY),
    'wave-report': BuiltinRegistrar.of(WAVE_REPORT_CARD_ENTRY),
  };
  for (const type of BUILTIN_CARD_ORDER) {
    registrars[type]?.run(registry);
  }
}
