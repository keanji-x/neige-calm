import { describe, expect, it } from 'vitest';

import type { KernelCardInput } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import { ASSISTANT_CARD_ENTRY } from './assistant.ts';
import { CLAUDE_CARD_ENTRY } from './claude.ts';
import { CODEX_CARD_ENTRY } from './codex.ts';
import { partitionWaveCards } from './headless-filter.js';
import type { BuiltinCardType } from './register.js';
import { BUILTIN_CARD_ORDER, registerAvailableBuiltinCards } from './register.js';
import { SPEC_CARD_ENTRY } from './spec.js';
import { TERMINAL_CARD_ENTRY } from './terminal.js';
import { WAVE_REPORT_CARD_ENTRY } from './wave-report.js';

declare module '../registry.js' {
  interface CardDataMap {
    declaredHeadlessFixture: { readonly type: 'declared-headless-fixture'; readonly id: string };
  }
}

const LANDED = ['terminal', 'codex', 'spec', 'assistant', 'claude', 'wave-report'] as const;

describe('builtin card composition contract', () => {
  it('[INV-CARD-225] pins the nine-item order tuple', () => {
    // Not a set and not sorted: the registry's fallback scan runs in insertion
    // order, so this literal *is* the resolution semantics. Changing an entry
    // or dropping one changes which adapter claims a shared kernel kind.
    expect([...BUILTIN_CARD_ORDER]).toEqual([
      'terminal', 'codex', 'spec', 'assistant', 'claude', 'wave-report',
      'file-viewer', 'iframe', 'plugin-iframe',
    ]);
    expect(BUILTIN_CARD_ORDER).toHaveLength(9);
    expect(new Set(BUILTIN_CARD_ORDER).size).toBe(9);
  });

  it('registers only the entries that exist, with no placeholders for the three that do not', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.entries().map((entry) => entry.type)).toEqual([...LANDED]);
    for (const type of BUILTIN_CARD_ORDER) {
      if ((LANDED as readonly string[]).includes(type)) expect(registry.get(type)).toBeDefined();
      // A no-op/unknown placeholder would satisfy "eight entries" and then
      // swallow real kernel cards into a card that renders nothing.
      else expect(registry.get(type), `${type} must be absent, not a placeholder`).toBeUndefined();
    }
    expect(registry.get('terminal')).toBe(TERMINAL_CARD_ENTRY);
    expect(registry.get('codex')).toBe(CODEX_CARD_ENTRY);
    expect(registry.get('spec')).toBe(SPEC_CARD_ENTRY);
    expect(registry.get('assistant')).toBe(ASSISTANT_CARD_ENTRY);
    expect(registry.get('claude')).toBe(CLAUDE_CARD_ENTRY);
    expect(registry.get('wave-report')).toBe(WAVE_REPORT_CARD_ENTRY);
  });

  it('[INV-CARD-225] keeps the landed entries in tuple-relative order across the skipped holes', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const registered = registry.entries().map((entry) => entry.type);
    const tupleIndexes = registered.map((type) => BUILTIN_CARD_ORDER.indexOf(type as never));
    expect(tupleIndexes).not.toContain(-1);
    expect([...tupleIndexes]).toEqual([...tupleIndexes].sort((left, right) => left - right));
    // With codex and assistant landed the first six slots are contiguous; the
    // hole that is still real is the tail — file-viewer (6), iframe (7) and
    // plugin-iframe (8) are unregistered, so `wave-report` at index 5 is last.
    // Asserted as *tuple* indexes rather than 0..n, so a later slice that lands
    // one of those three out of tuple position turns this red.
    expect(tupleIndexes).toEqual([0, 1, 2, 3, 4, 5]);
    // The hole is a suffix, and the assertion above only pins that while the
    // unlanded set really is the tail. Pin the other side too, from the tuple
    // itself: every skipped type sits after every registered one.
    const skipped = BUILTIN_CARD_ORDER
      .map((type, index) => ({ type, index }))
      .filter(({ type }) => !(registered as readonly string[]).includes(type));
    expect(skipped.map(({ type }) => type)).toEqual(['file-viewer', 'iframe', 'plugin-iframe']);
    expect(Math.min(...skipped.map(({ index }) => index))).toBeGreaterThan(Math.max(...tupleIndexes));
  });

  it('[INV-CARD-180] leaves the shared codex kind to the codex adapter first, then falls back to spec', () => {
    // Both halves are the real production entries, registered in
    // `BUILTIN_CARD_ORDER`'s relative order (codex before spec).
    //
    // What can actually fail here is codex's *refusal* of `spec_harness`: drop
    // it and the first assertion goes red. The order and the absent `claim` are
    // belt-and-braces given `resolve`'s fall-through — it continues past any
    // entry returning `null` — so swapping the two registrations below leaves
    // both assertions green. Do not read this test as proving the order; the
    // no-claim rule is pinned directly, by assertion, in `codex.test.ts` and
    // `spec.test.ts`.
    const registry = createCardRegistry();
    registry.register(CODEX_CARD_ENTRY);
    registry.register(SPEC_CARD_ENTRY);

    expect(
      registry.resolve({ id: 'spec', kind: 'codex', payload: { spec_harness: true } })?.type,
      'spec_harness must fall through the earlier codex adapter and resolve as spec',
    ).toBe('spec');
    expect(
      registry.resolve({ id: 'codex', kind: 'codex', payload: {} })?.type,
      'ordinary codex payload must resolve through codex before the shared-kind spec fallback',
    ).toBe('codex');
  });

  /*
   * #1189 §5.4 — the assistant marker's half of the same rule.
   *
   * The card the wave conversation endpoint mints is `kind: 'codex'` carrying
   * `harness_profile: 'assistant'`, and `codex` is scanned before `assistant`.
   * What is really under test is `CODEX_CARD_ENTRY`'s refusal: delete that
   * clause and the assertion below reads `'codex'`, which in production means a
   * headless conversation card drawn as an empty terminal in CARDS and on the
   * board. Both entries are the production ones, registered in tuple-relative
   * order.
   */
  it('[INV-CARD-180] leaves an assistant-marked codex card to the assistant adapter', () => {
    const registry = createCardRegistry();
    registry.register(CODEX_CARD_ENTRY);
    registry.register(ASSISTANT_CARD_ENTRY);

    expect(
      registry.resolve({ id: 'a', kind: 'codex', payload: { harness_profile: 'assistant' } })?.type,
      'the assistant marker must fall through the earlier codex adapter',
    ).toBe('assistant');
    /* And the marker is the whole predicate, in both directions: a cove chat
       card carries `plain_chat` under the same field and must not become an
       assistant. */
    expect(
      registry.resolve({ id: 'p', kind: 'codex', payload: { harness_profile: 'plain_chat' } }),
      'a plain chat card is not a wave assistant',
    ).toBeNull();
  });

  /*
   * Both mistakes delete cards from the product: a missing declaration puts a
   * card that renders nothing into the CARDS list and the grid; a spurious one
   * deletes every card of that type from both the moment its entry lands.
   *
   * `CardEntry.headless` is optional on the interface, so a bare
   * `registry.register` call cannot see either. For built-ins the *missing*
   * half is closed at compile time — `register.ts` fills its registrar map only
   * with `BuiltinRegistrar.of(entry)`, which requires `headless` — and the
   * `toBeTypeOf('boolean')` assertion below re-checks it at runtime so the
   * suite does not depend on that types-only argument (a type assertion could
   * still forge a registrar). The *wrong* half has no compile-time signal at
   * all and lives here.
   *
   * The expectation below is written per built-in **type**, decided from the
   * oracle rather than read back off the entries, and covers all nine — so a
   * later slice cannot land an entry whose headlessness nobody decided. Nothing
   * here executes a component: an entry is a plain object and calling its
   * component outside a renderer would throw for any entry that uses a hook.
   */
  describe('headless is declared on the entry, and the declaration is what filters', () => {
    // Source of truth: `INV-CARD-181` (spec), `INV-CARD-201` (wave-report) and
    // the wave assistant (#1189 §5.4) are headless — all three are read in the
    // conversation drawer or the report column and draw no card of their own;
    // every other built-in owns a surface.
    const HEADLESS_BY_TYPE: Readonly<Record<BuiltinCardType, boolean>> = Object.freeze({
      terminal: false, codex: false, spec: true, assistant: true, claude: false,
      'wave-report': true, 'file-viewer': false, iframe: false, 'plugin-iframe': false,
    });
    const bootedProductionRegistry = () => {
      const registry = createCardRegistry();
      registerAvailableBuiltinCards(registry);
      return registry;
    };

    it('[INV-CARD-225] decides headlessness for every type in the order tuple, and only those', () => {
      expect(Object.keys(HEADLESS_BY_TYPE).sort()).toEqual([...BUILTIN_CARD_ORDER].sort());
    });

    it('[INV-CARD-226] declares headless on exactly the entries that are headless', () => {
      const entries = bootedProductionRegistry().entries();
      expect(entries.length).toBeGreaterThan(0);
      for (const entry of entries) {
        const expected = HEADLESS_BY_TYPE[entry.type as BuiltinCardType];
        expect(expected, `${entry.type} is registered but not in the headless decision table`).toBeTypeOf('boolean');
        // Independent of the table above, and deliberately not `=== true`:
        // `undefined` means nobody decided. Without this line an omitted
        // declaration plus a `false` row in the table — one mistake made twice
        // in the same direction — would leave both assertions green.
        expect(
          entry.headless,
          `${entry.type} must state its headlessness explicitly; absent is the fail-open default`,
        ).toBeTypeOf('boolean');
        expect(
          entry.headless === true,
          expected
            ? `${entry.type} is headless but does not declare it — it would occupy an empty slot`
            : `${entry.type} declares headless but owns a surface — it would vanish from the wave`,
        ).toBe(expected);
      }
    });

    it('[INV-CARD-226] filters on that declaration, not on the type name', () => {
      // A surface-sized entry with a type no filter could special-case: if the
      // partition read anything other than `entry.headless` this card would
      // survive into the visible branch.
      const registry = createCardRegistry();
      registry.register({
        type: 'declared-headless-fixture',
        component: () => null,
        headless: true,
        defaultSize: { w: 4, h: 6, minW: 3, minH: 3 },
        title: () => 'fixture',
        accessibleName: () => 'fixture',
        create: { mode: 'kernel-minted-only' },
        fromKernel: (raw) => (raw.kind === 'declared' ? { type: 'declared-headless-fixture', id: raw.id } : null),
      });
      const wire = {
        id: 'd1', kind: 'declared', wave_id: 'w1', title: null, sort: 0, payload: null,
        deletable: true, created_at: 0, updated_at: 0,
      };
      const { visible, unknown } = partitionWaveCards(registry, [wire]);
      expect(visible).toEqual([]);
      expect(unknown).toEqual([]);
    });
  });

  /*
   * `partitionWaveCards` reads headlessness with `registry.get(card.type)` on
   * the card `resolve` handed back. `resolve` never checks that a resolved
   * card's `type` belongs to the entry that produced it, so that lookup is only
   * sound while every entry's `fromKernel` mints its own type. The narrow
   * `CardEntry<Card>` annotation ties the two together at compile time; an
   * entry annotated as a bare `CardEntry` drops the tie with no other signal,
   * and a headless card whose lookup misses would fail open into the visible
   * list. Asserted here on the entries that are really registered.
   */
  describe('every registered entry mints cards of its own type', () => {
    // Keyed by the landed types, so a slice that registers a new built-in must
    // hand it a probe rather than quietly leave it unasserted.
    const PROBE_BY_TYPE: Readonly<Record<(typeof LANDED)[number], KernelCardInput>> = Object.freeze({
      terminal: { id: 'probe-term', kind: 'terminal', payload: { terminal_id: 't1' } },
      codex: { id: 'probe-codex', kind: 'codex', payload: { terminal_id: 't3' } },
      spec: { id: 'probe-spec', kind: 'codex', payload: { spec_harness: true } },
      assistant: { id: 'probe-assistant', kind: 'codex', payload: { harness_profile: 'assistant' } },
      claude: { id: 'probe-claude', kind: 'claude', payload: { terminal_id: 't2' } },
      'wave-report': { id: 'probe-report', kind: 'wave-report', payload: null },
    });

    it('probes every entry the production boot registers, and only those', () => {
      const registry = createCardRegistry();
      registerAvailableBuiltinCards(registry);
      expect(Object.keys(PROBE_BY_TYPE).sort()).toEqual(registry.entries().map((entry) => entry.type).sort());
    });

    it('[INV-CARD-226] resolves each probe back to the entry that owns it', () => {
      const registry = createCardRegistry();
      registerAvailableBuiltinCards(registry);
      const entries = registry.entries();
      expect(entries.length).toBeGreaterThan(0);
      for (const entry of entries) {
        const probe = PROBE_BY_TYPE[entry.type as (typeof LANDED)[number]];
        expect(
          entry.fromKernel?.(probe)?.type,
          `${entry.type} must mint its own type, or the headless lookup reads another entry`,
        ).toBe(entry.type);
        // The same trip `partitionWaveCards` makes: resolve, then look the
        // entry back up by the resolved card's type.
        const card = registry.resolve(probe);
        expect(card?.type, `${entry.type} probe must resolve through the registry`).toBe(entry.type);
        if (card !== null) expect(registry.get(card.type)).toBe(entry);
      }
    });
  });

  it('takes no entries and keeps no state: two registries boot independently', () => {
    const first = createCardRegistry();
    const second = createCardRegistry();
    registerAvailableBuiltinCards(first);
    registerAvailableBuiltinCards(second);
    expect(second.entries().map((entry) => entry.type)).toEqual([...LANDED]);
    expect(registerAvailableBuiltinCards).toHaveLength(1);
  });
});
