import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import type { CodexCard } from './codex.ts';
import { CODEX_CARD_ENTRY, isPlainChatPayload } from './codex.ts';
import { partitionTrackCards } from './headless-filter.js';
import { registerAvailableBuiltinCards } from './register.js';

function wire(id: string, kind: string, payload: unknown) {
  return {
    id, kind, track_id: 'w1', title: null, sort: 0, payload,
    deletable: true, created_at: 0, updated_at: 0,
  };
}

describe('CODEX_CARD_ENTRY', () => {
  it('resolves kernel codex cards, including before terminal_id is projected', () => {
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x1', kind: 'codex', payload: { terminal_id: 't1' },
    })).toEqual({ type: 'codex', id: 'x1', title: null, terminalId: 't1', sessionState: null, cwd: null, gateCwd: null });
    // The kernel projects `terminal_id` on read; a card observed between mint
    // and projection resolves with a null terminal rather than not at all.
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x2', kind: 'codex', payload: { goal: 'do the thing' },
    })).toEqual({ type: 'codex', id: 'x2', title: null, terminalId: null, sessionState: null, cwd: null, gateCwd: null });
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x3', kind: 'terminal', payload: { terminal_id: 't1' },
    })).toBeNull();
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x4', kind: 'claude', payload: { terminal_id: 't1' },
    })).toBeNull();
  });

  /*
   * `INV-CARD-180`. Kind `'codex'` mints three different cards; the payload's
   * `planner_harness` bit is the only thing separating a harness from an ordinary
   * worker. If codex stopped refusing harness payloads it would swallow every
   * planner card into a surface-owning card — planner cards are headless by
   * `INV-CARD-181` and must not appear in the CARDS list at all. (The refusal
   * is the mechanism; codex being registered before planner and carrying no
   * `claim` is not — `resolve`'s exact-claim path falls through on `null`.)
   */
  it('[INV-CARD-180] refuses planner harness payloads so they fall through to planner', () => {
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 's1', kind: 'codex', payload: { planner_harness: true },
    })).toBeNull();
    // A refusal is only useful if the card really lands on planner afterwards, so
    // assert it through the booted production registry, in production order.
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(
      registry.resolve({ id: 's1', kind: 'codex', payload: { planner_harness: true } })?.type,
      'a planner harness card must still resolve as planner through the production registry',
    ).toBe('planner');
    expect(registry.get('planner')?.headless).toBe(true);
    // …and an ordinary codex card must not be taken by planner.
    expect(registry.resolve({ id: 'x1', kind: 'codex', payload: { terminal_id: 't1' } })?.type)
      .toBe('codex');
  });

  it('reads only the exact discriminator, not any truthy planner_harness', () => {
    // `isPlannerHarnessPayload` is `=== true`; sharing it with planner is what keeps
    // the two entries from disagreeing about which cards are harnesses.
    for (const payload of [{ planner_harness: false }, { planner_harness: 'true' }, { planner_harness: 1 }, null, 'x']) {
      expect(CODEX_CARD_ENTRY.fromKernel?.({ id: 'p', kind: 'codex', payload })?.type).toBe('codex');
    }
  });

  /*
   * `INV-CHAT-016` — the *third* shape under kind `'codex'`. An area plain-chat
   * conversation card carries `harness_profile: "plain_chat"` and deliberately
   * no `planner_harness` key, and has no PTY at all: the adapter writes
   * `terminal_run_id: None`, so `terminal_id` is never projected for it.
   * Claiming it would give every conversation a real grid slot rendering
   * `TerminalCardView` with a null terminal — "Starting codex…" forever.
   *
   * Refusing it is behaviour-preserving: it resolves to nothing and lands in
   * the `unknown` branch, exactly where it sat before this adapter existed.
   */
  it('[INV-CHAT-016] refuses area plain-chat cards, which have no PTY to render', () => {
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'chat1', kind: 'codex', payload: { schemaVersion: 1, harness_profile: 'plain_chat' },
    })).toBeNull();
    // Asserted through the really-booted production registry and the real
    // partition helper: nothing else in the registry may claim it either, and
    // it must reach `unknown`, not `visible`.
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.resolve({
      id: 'chat1', kind: 'codex', payload: { schemaVersion: 1, harness_profile: 'plain_chat' },
    })).toBeNull();
    const { visible, unknown } = partitionTrackCards(registry, [
      wire('chat1', 'codex', { schemaVersion: 1, harness_profile: 'plain_chat' }),
      wire('k-codex', 'codex', { terminal_id: 't1' }),
    ]);
    expect(visible.map((slot) => slot.wire.id)).toEqual(['k-codex']);
    expect(unknown.map((slot) => slot.wire.id)).toEqual(['chat1']);
  });

  it('[INV-CHAT-016] reads only the exact plain-chat marker', () => {
    // Mirrors the kernel's `payload.get("harness_profile").and_then(as_str)
    // == Some("plain_chat")`. Everything else is an ordinary codex card.
    expect(isPlainChatPayload({ harness_profile: 'plain_chat' })).toBe(true);
    for (const payload of [
      {}, { harness_profile: 'other_profile' }, { harness_profile: true },
      { harness_profile: 1 }, { harness_profile: null }, { harness_profile: {} },
      null, 'x', 7, undefined,
    ]) {
      expect(isPlainChatPayload(payload), `${JSON.stringify(payload)} is not the marker`).toBe(false);
      expect(CODEX_CARD_ENTRY.fromKernel?.({ id: 'p', kind: 'codex', payload })?.type).toBe('codex');
    }
  });

  /*
   * The no-claim rule, pinned the way `planner.test.ts` pins planner's.
   *
   * It is *not* the mechanism that separates the shared kind — `resolve` runs
   * the exact-claim entry's `fromKernel` and, on `null`, continues to the
   * insertion-order scan, so an exact claim here would resolve harnesses as
   * `planner` all the same and `validateEntry` would not object. Without this
   * assertion, adding the claim is a green mutation. Read through the
   * interface: the entry literal is checked with `satisfies`, so the constant's
   * own type only lists the members it declares.
   */
  it('[INV-CARD-180] takes no claim on the shared kernel kind', () => {
    expect((CODEX_CARD_ENTRY as CardEntry<CodexCard>).claim).toBeUndefined();
  });

  /*
   * This used to assert `kernel-minted-only`, on the reading that a worker card
   * comes from a task row rather than a gesture. That was a statement about the
   * front-end, not about the kernel: `POST /api/tracks/:id/codex-cards` has
   * always minted one atomically, and the panel now offers it. What the kernel
   * genuinely reserves to itself is the *planner harness* — a `codex` row with a
   * harness payload — and that is a different entry (`PLANNER_CARD_ENTRY`), which
   * keeps its `kernel-minted-only` and its own assertion.
   */
  it('is user-creatable through the kind\'s own atomic endpoint', () => {
    expect(CODEX_CARD_ENTRY.create.mode).toBe('atomic');
  });

  it('offers a title and a working directory, and nothing about codex itself', () => {
    /* Read through the interface, like the claim case above: the literal is
       checked with `satisfies`, so the constant's own type narrows every field
       to what it happens to declare and would make these assertions tautologies. */
    const addPanel = (CODEX_CARD_ENTRY as CardEntry<CodexCard>).addPanel;
    expect(addPanel?.label).toBe('codex');
    expect(addPanel?.fields?.map((field) => [field.key, field.kind]))
      .toEqual([['title', 'text'], ['cwd', 'directory']]);
    // Model and permission mode are answered inside codex's own slash-command
    // UX; collecting them here would be a second, unchangeable answer.
    expect(addPanel?.fields?.some((field) => field.required === true)).toBe(false);
  });

  it('registers as a surface-owning built-in', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.get('codex')?.headless).toBe(false);
    expect(registry.resolve({ id: 'x1', kind: 'codex', payload: { terminal_id: 't9' } })?.type)
      .toBe('codex');
  });

  /*
   * The bug this card exists to fix (#1150). A codex card with no adapter fell
   * into `unknown`, so `app/router` left it out of `gridItems`, `knownCard`
   * stayed false and the route effect replaced the requested `?card=` straight
   * back out — clicking a codex row in the CARDS panel did nothing at all,
   * while the terminal row beside it opened the board.
   */
  it('lands codex cards in the visible partition beside terminal cards', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const { visible, unknown } = partitionTrackCards(registry, [
      wire('k-codex', 'codex', { terminal_id: 't1' }),
      wire('k-term', 'terminal', { terminal_id: 't2' }),
      // Same kernel kind, harness bit set: still headless, still filtered out.
      wire('k-planner', 'codex', { planner_harness: true }),
    ]);
    expect(visible.map((slot) => slot.card.type)).toEqual(['codex', 'terminal']);
    expect(unknown).toEqual([]);
  });
});
